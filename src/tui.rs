//! Interactive terminal UI.
//!
//! A background task refreshes the snapshot every `refresh_secs` (or on demand)
//! and sends it to the UI loop, which only draws and handles keys.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};
use tokio::sync::{mpsc, watch};

use crate::config::Config;
use crate::filter::RunFilter;
use crate::github::Client;
use crate::logs;
use crate::notify;
use crate::snapshot::{self, ExpandRequest, Snapshot};
use crate::status::Status;
use crate::tree::{self, Folds, Row, RowKey};

struct App {
    snapshot: Option<Snapshot>,
    rows: Vec<Row>,
    list: ListState,
    folds: Folds,
    expand: ExpandRequest,
    loading: bool,
    message: Option<String>,
    last_refresh: Option<Instant>,
    /// Present when desktop notifications are enabled.
    watcher: Option<notify::Watcher>,
    /// Text being typed after `/`; `Some` while the filter field has focus.
    filter_input: Option<String>,
    /// Text of the applied filter, shown in the footer.
    filter_text: String,
    /// Open log panel (`l` on a step or job).
    log_view: Option<LogView>,
    /// Downloaded job logs by job id, kept in memory for this session only.
    log_cache: HashMap<u64, Result<String, String>>,
}

struct LogTarget {
    slug: String,
    view: LogView,
}

/// What the log panel shows.
struct LogView {
    title: String,
    job_id: u64,
    /// Selected step and the spans of every step of the job; `None` shows
    /// the whole job.
    step: Option<(u32, Vec<logs::StepSpan>)>,
    /// Toggled with `w`: whole job instead of the step.
    whole: bool,
    lines: Vec<String>,
    note: Option<String>,
    /// Lines scrolled from the top; `None` means "stick to the bottom".
    scroll: Option<usize>,
}

impl LogView {
    /// Recomputes the visible lines from the cached log, if it arrived.
    fn fill(&mut self, cache: &HashMap<u64, Result<String, String>>) {
        self.note = None;
        self.lines = match cache.get(&self.job_id) {
            None => vec!["loading log…".into()],
            Some(Err(err)) => vec![format!("cannot load log: {err}")],
            Some(Ok(log)) => match (&self.step, self.whole) {
                (Some((number, spans)), false) => {
                    let by_marker = logs::cut_step(log, spans, *number);
                    let by_time = || {
                        let span = spans.iter().find(|s| s.number == *number)?;
                        logs::slice_step(log, span.started.as_deref(), span.completed.as_deref())
                    };
                    match by_marker.or_else(by_time) {
                        Some(lines) => logs::tail(lines, logs::STEP_TAIL),
                        None => {
                            self.note = Some("step not found in log; showing end of job".into());
                            logs::tail(logs::whole_job(log), logs::STEP_TAIL)
                        }
                    }
                }
                _ => logs::whole_job(log),
            },
        };
        self.scroll = None;
    }
}

impl App {
    fn rebuild(&mut self) {
        let selected_key = self
            .list
            .selected()
            .and_then(|i| self.rows.get(i))
            .map(|r| r.key.clone());
        self.rows = self
            .snapshot
            .as_ref()
            .map(|s| tree::build(s, &self.folds))
            .unwrap_or_default();
        let index = selected_key
            .and_then(|key| self.rows.iter().position(|r| r.key == key))
            .or(if self.rows.is_empty() { None } else { Some(0) });
        self.list.select(index);
    }

    fn selected(&self) -> Option<&Row> {
        self.list.selected().and_then(|i| self.rows.get(i))
    }

    fn move_by(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let current = self.list.selected().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, self.rows.len() as isize - 1);
        self.list.select(Some(next as usize));
    }

    fn jump_repo(&mut self, forward: bool) {
        let current = self.list.selected().unwrap_or(0);
        let found = if forward {
            self.rows
                .iter()
                .enumerate()
                .skip(current + 1)
                .find(|(_, r)| r.depth == 0)
        } else {
            self.rows
                .iter()
                .enumerate()
                .take(current)
                .rev()
                .find(|(_, r)| r.depth == 0)
        };
        if let Some((i, _)) = found {
            self.list.select(Some(i));
        }
    }

    /// Toggles the selected row. Returns true when new data must be fetched.
    fn toggle(&mut self) -> bool {
        let Some(row) = self.selected().cloned() else {
            return false;
        };
        let mut needs_fetch = false;
        match &row.key {
            RowKey::Repo(slug) => {
                if !self.folds.collapsed_repos.remove(slug) {
                    self.folds.collapsed_repos.insert(slug.clone());
                }
            }
            RowKey::Workflow(id) => {
                if row.expanded {
                    self.folds.collapsed_workflows.insert(*id);
                    self.expand.pinned.remove(id);
                } else {
                    self.folds.collapsed_workflows.remove(id);
                    if !self.has_jobs(*id) {
                        self.expand.pinned.insert(*id);
                        needs_fetch = true;
                    }
                }
            }
            _ => {}
        }
        self.rebuild();
        needs_fetch
    }

    /// Log panel for the selected job or step row, with its repository slug.
    fn log_target(&self) -> Option<LogTarget> {
        let (run_id, job_idx, step_no) = match self.selected()?.key {
            RowKey::Job(run, job) => (run, job, None),
            RowKey::Step(run, job, step) => (run, job, Some(step)),
            _ => return None,
        };
        let snapshot = self.snapshot.as_ref()?;
        for repo in &snapshot.repos {
            let Ok(workflows) = &repo.workflows else {
                continue;
            };
            let Some(wf) = workflows.iter().find(|w| w.run_id == run_id) else {
                continue;
            };
            let job = wf.jobs.as_ref()?.as_ref().ok()?.get(job_idx)?;
            let step = step_no.and_then(|n| job.steps.iter().find(|s| s.number == n));
            let mut title = format!("{} › {} › {}", repo.name, wf.name, job.name);
            if let Some(step) = step {
                title.push_str(&format!(" › {}. {}", step.number, step.name));
            }
            return Some(LogTarget {
                slug: repo.slug.clone(),
                view: LogView {
                    title,
                    job_id: job.id,
                    step: step.map(|s| {
                        let spans = job
                            .steps
                            .iter()
                            .map(|s| logs::StepSpan {
                                number: s.number,
                                name: s.name.clone(),
                                started: s.started_at.clone(),
                                completed: s.completed_at.clone(),
                                skipped: s.status == Status::Skipped,
                            })
                            .collect();
                        (s.number, spans)
                    }),
                    whole: false,
                    lines: Vec::new(),
                    note: None,
                    scroll: None,
                },
            });
        }
        None
    }

    fn has_jobs(&self, run_id: u64) -> bool {
        self.snapshot.iter().flat_map(|s| &s.repos).any(|repo| {
            repo.workflows
                .as_ref()
                .is_ok_and(|wfs| wfs.iter().any(|w| w.run_id == run_id && w.jobs.is_some()))
        })
    }
}

pub async fn run(config: Config, client: Client) -> Result<()> {
    let client = Arc::new(client);
    let config = Arc::new(config);
    let refresh = Duration::from_secs(config.settings.refresh_secs);

    let initial_expand = ExpandRequest::from_mode(config.settings.expand);
    let (expand_tx, expand_rx) = watch::channel(initial_expand.clone());
    let (refresh_tx, mut refresh_rx) = mpsc::unbounded_channel::<()>();
    let (snap_tx, mut snap_rx) = mpsc::unbounded_channel::<Snapshot>();

    // Background refresher.
    {
        let client = client.clone();
        let config = config.clone();
        tokio::spawn(async move {
            loop {
                let expand = expand_rx.borrow().clone();
                let snap = snapshot::take(&client, &config, &expand).await;
                if snap_tx.send(snap).is_err() {
                    break;
                }
                tokio::select! {
                    _ = tokio::time::sleep(refresh) => {}
                    msg = refresh_rx.recv() => if msg.is_none() { break },
                }
                // Coalesce bursts of manual refresh requests.
                while refresh_rx.try_recv().is_ok() {}
            }
        });
    }

    let mut app = App {
        snapshot: None,
        rows: Vec::new(),
        list: ListState::default(),
        folds: Folds::default(),
        expand: initial_expand,
        loading: true,
        message: None,
        last_refresh: None,
        watcher: config
            .settings
            .notify
            .then(|| notify::Watcher::new(config.settings.notify_branches.clone())),
        filter_input: None,
        filter_text: String::new(),
        log_view: None,
        log_cache: HashMap::new(),
    };

    let mut terminal = ratatui::init();
    let result = ui_loop(
        &mut terminal,
        &mut app,
        client.clone(),
        &mut snap_rx,
        &refresh_tx,
        &expand_tx,
    );
    ratatui::restore();
    result
}

fn ui_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    client: Arc<Client>,
    snap_rx: &mut mpsc::UnboundedReceiver<Snapshot>,
    refresh_tx: &mpsc::UnboundedSender<()>,
    expand_tx: &watch::Sender<ExpandRequest>,
) -> Result<()> {
    let request_refresh = |app: &mut App| {
        app.loading = true;
        let _ = expand_tx.send(app.expand.clone());
        let _ = refresh_tx.send(());
    };

    // Applies `text` as the run filter. Notifications pause while a filter is
    // on (the filtered view is not the real "latest run" of each workflow) and
    // resume silently when it is cleared.
    let apply_filter = |app: &mut App, text: &str| {
        let filter = RunFilter::parse(text);
        if filter == app.expand.filter {
            return;
        }
        app.filter_text = text.trim().to_owned();
        app.expand.filter = filter;
        if let Some(watcher) = app.watcher.as_mut() {
            watcher.reset();
        }
        request_refresh(app);
    };

    let (log_tx, mut log_rx) = mpsc::unbounded_channel::<(u64, Result<String, String>)>();

    loop {
        while let Ok(snap) = snap_rx.try_recv() {
            let filtering = app.expand.filter.is_some();
            if let Some(watcher) = app.watcher.as_mut().filter(|_| !filtering) {
                for event in watcher.observe(&snap) {
                    notify::send(&event);
                    if event.change == notify::Change::Failed {
                        app.message = Some(format!("✗ {} › {} failed", event.repo, event.workflow));
                    }
                }
            }
            app.snapshot = Some(snap);
            app.loading = false;
            app.last_refresh = Some(Instant::now());
            app.rebuild();
        }

        while let Ok((job_id, log)) = log_rx.try_recv() {
            app.log_cache.insert(job_id, log);
            if let Some(view) = app.log_view.as_mut().filter(|v| v.job_id == job_id) {
                view.fill(&app.log_cache);
            }
        }

        terminal.draw(|frame| draw(frame, app, &client))?;

        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        app.message = None;

        // Log panel open: keys scroll it.
        if let Some(view) = app.log_view.as_mut() {
            let current = view.scroll.unwrap_or(usize::MAX);
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('l') => app.log_view = None,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(());
                }
                KeyCode::Down | KeyCode::Char('j') => view.scroll = Some(current.saturating_add(1)),
                KeyCode::Up | KeyCode::Char('k') => view.scroll = Some(current.saturating_sub(1)),
                KeyCode::PageDown | KeyCode::Char(' ') => {
                    view.scroll = Some(current.saturating_add(20))
                }
                KeyCode::PageUp => view.scroll = Some(current.saturating_sub(20)),
                KeyCode::Home | KeyCode::Char('g') => view.scroll = Some(0),
                KeyCode::End | KeyCode::Char('G') => view.scroll = None,
                KeyCode::Char('w') if view.step.is_some() => {
                    view.whole = !view.whole;
                    view.fill(&app.log_cache);
                }
                _ => {}
            }
            continue;
        }

        // Filter field has focus: keys edit the text, the filter applies live.
        if let Some(input) = app.filter_input.as_mut() {
            match key.code {
                KeyCode::Enter => app.filter_input = None,
                KeyCode::Esc => {
                    app.filter_input = None;
                    apply_filter(app, "");
                }
                KeyCode::Backspace => {
                    input.pop();
                    let text = input.clone();
                    apply_filter(app, &text);
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(());
                }
                KeyCode::Char(c) => {
                    input.push(c);
                    let text = input.clone();
                    apply_filter(app, &text);
                }
                _ => {}
            }
            continue;
        }

        match key.code {
            KeyCode::Char('/') => app.filter_input = Some(app.filter_text.clone()),
            KeyCode::Esc if app.expand.filter.is_some() => apply_filter(app, ""),
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
            KeyCode::Down | KeyCode::Char('j') => app.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => app.move_by(-1),
            KeyCode::PageDown => app.move_by(10),
            KeyCode::PageUp => app.move_by(-10),
            KeyCode::Home | KeyCode::Char('g') => app.list.select(Some(0)),
            KeyCode::End | KeyCode::Char('G') => app.move_by(isize::MAX / 2),
            KeyCode::Tab | KeyCode::Char('J') => app.jump_repo(true),
            KeyCode::BackTab | KeyCode::Char('K') => app.jump_repo(false),
            KeyCode::Enter | KeyCode::Char(' ') => {
                if app.toggle() {
                    request_refresh(app);
                }
            }
            KeyCode::Char('a') => {
                app.expand.all = !app.expand.all;
                app.folds.collapsed_workflows.clear();
                app.message = Some(
                    if app.expand.all {
                        "expanding all workflows"
                    } else {
                        "expanding only active/failed"
                    }
                    .into(),
                );
                request_refresh(app);
            }
            KeyCode::Char('r') => request_refresh(app),
            KeyCode::Char('l') => {
                if let Some(target) = app.log_target() {
                    let mut view = target.view;
                    if !app.log_cache.contains_key(&view.job_id) {
                        let (client, tx, slug, job_id) =
                            (client.clone(), log_tx.clone(), target.slug, view.job_id);
                        tokio::spawn(async move {
                            let log = client
                                .job_log(&slug, job_id)
                                .await
                                .map_err(|e| format!("{e:#}"));
                            let _ = tx.send((job_id, log));
                        });
                    }
                    view.fill(&app.log_cache);
                    app.log_view = Some(view);
                } else {
                    app.message = Some("select a job or step to see its log".into());
                }
            }
            KeyCode::Char('o') => {
                if let Some(url) = app.selected().and_then(|r| r.url.clone()) {
                    app.message = Some(match open_url(&url) {
                        Ok(()) => format!("opened {url}"),
                        Err(err) => format!("cannot open browser: {err}"),
                    });
                }
            }
            _ => {}
        }
    }
}

fn draw(frame: &mut ratatui::Frame, app: &mut App, client: &Client) {
    let [main, footer] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());

    if let Some(view) = app.log_view.as_mut() {
        draw_log(frame, view, main, footer);
        return;
    }

    let items: Vec<ListItem> = app.rows.iter().map(row_line).map(ListItem::new).collect();
    let title = if app.loading {
        " popier · refreshing… "
    } else {
        " popier "
    };
    let list = List::new(items)
        .block(Block::bordered().title(title.bold()))
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(45, 45, 60))
                .add_modifier(Modifier::BOLD),
        );
    frame.render_stateful_widget(list, main, &mut app.list);

    if let Some(input) = &app.filter_input {
        let line = Line::from(vec![
            Span::styled(" filter: ", Style::default().fg(Color::Cyan)),
            Span::raw(input.clone()),
            Span::styled("▏", Style::default().fg(Color::Cyan)),
            Span::styled(
                "   branch part or #PR · ⏎ keep · Esc clear",
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        frame.render_widget(Paragraph::new(line), footer);
        return;
    }

    let mut spans = vec![Span::styled(
        " ↑↓/jk move  ⏎ fold  Tab next repo  / filter  l log  a all  r refresh  o open  q quit ",
        Style::default().fg(Color::DarkGray),
    )];
    if !app.filter_text.is_empty() {
        spans.push(Span::styled(
            format!(" filter: {} (Esc clears) ", app.filter_text),
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ));
    }
    if let Some(at) = app.last_refresh {
        spans.push(Span::raw(format!(
            " updated {}s ago",
            at.elapsed().as_secs()
        )));
    }
    let rate = client.rate_limit();
    if let (Some(rem), Some(lim)) = (rate.remaining, rate.limit) {
        spans.push(Span::styled(
            format!("  api {rem}/{lim}"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    if let Some(msg) = &app.message {
        spans.push(Span::styled(
            format!("  {msg}"),
            Style::default().fg(Color::Cyan),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), footer);
}

fn draw_log(
    frame: &mut ratatui::Frame,
    view: &mut LogView,
    main: ratatui::layout::Rect,
    footer: ratatui::layout::Rect,
) {
    let height = main.height.saturating_sub(2) as usize;
    let max_scroll = view.lines.len().saturating_sub(height);
    let scroll = view.scroll.map_or(max_scroll, |s| s.min(max_scroll));
    view.scroll = view.scroll.map(|_| scroll);

    let lines: Vec<Line> = view.lines.iter().map(|l| log_line(l)).collect();
    let scope = if view.step.is_none() || view.whole {
        "job"
    } else {
        "step"
    };
    let title = format!(" log ({scope}) · {} ", view.title);
    let paragraph = Paragraph::new(lines)
        .block(Block::bordered().title(title.bold()))
        .scroll((scroll as u16, 0));
    frame.render_widget(paragraph, main);

    let mut spans = vec![Span::styled(
        " ↑↓/jk scroll  PgUp/PgDn page  g/G top/bottom  w step/job  Esc close ",
        Style::default().fg(Color::DarkGray),
    )];
    spans.push(Span::raw(format!(
        " {}/{} lines",
        (scroll + height).min(view.lines.len()),
        view.lines.len()
    )));
    if let Some(note) = &view.note {
        spans.push(Span::styled(
            format!("  {note}"),
            Style::default().fg(Color::Yellow),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), footer);
}

/// Colors GitHub's log markers (`##[error]`, `##[group]`, …).
fn log_line(line: &str) -> Line<'static> {
    let style = if line.starts_with("##[error]") {
        Style::default().fg(Color::Red)
    } else if line.starts_with("##[warning]") {
        Style::default().fg(Color::Yellow)
    } else if line.starts_with("##[group]") || line.starts_with("##[endgroup]") {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };
    Line::from(Span::styled(line.to_owned(), style))
}

fn row_line(row: &Row) -> Line<'static> {
    let mut spans = vec![Span::raw("  ".repeat(row.depth as usize))];
    if row.foldable {
        spans.push(Span::styled(
            if row.expanded { "▾ " } else { "▸ " },
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        spans.push(Span::raw("  "));
    }
    if let Some(status) = row.status {
        spans.push(Span::styled(
            format!("{} ", status.icon()),
            Style::default().fg(status.color()),
        ));
    }
    let dim = Style::default().fg(Color::DarkGray);
    let is_step = matches!(row.key, RowKey::Step(..));
    if is_step && let Some(d) = &row.detail {
        spans.push(Span::styled(format!("{d} "), dim));
    }
    let text_style = match (row.depth, row.is_error) {
        (0, _) => Style::default().add_modifier(Modifier::BOLD),
        (_, true) => Style::default().fg(Color::Red),
        _ => Style::default(),
    };
    spans.push(Span::styled(row.text.clone(), text_style));
    if !is_step && let Some(d) = &row.detail {
        spans.push(Span::styled(format!(" {d}"), dim));
    }
    Line::from(spans)
}

fn open_url(url: &str) -> std::io::Result<()> {
    std::process::Command::new("xdg-open")
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(step: Option<(&str, &str)>) -> LogView {
        let step = step.map(|(a, b)| {
            let span = logs::StepSpan {
                number: 1,
                name: "step".into(),
                started: Some(a.to_owned()),
                completed: Some(b.to_owned()),
                skipped: false,
            };
            (1, vec![span])
        });
        LogView {
            title: String::new(),
            job_id: 7,
            step,
            whole: false,
            lines: Vec::new(),
            note: None,
            scroll: Some(3),
        }
    }

    const LOG: &str =
        "2026-11-25T22:19:40.1Z a\n2026-11-25T22:19:44.0Z b\n2026-11-25T22:19:50.0Z c";

    #[test]
    fn log_view_states() {
        let mut cache = HashMap::new();
        let mut v = view(Some(("2026-11-25T22:19:44Z", "2026-11-25T22:19:45Z")));
        v.fill(&cache);
        assert_eq!(v.lines, vec!["loading log…"]);

        cache.insert(7, Ok(LOG.to_owned()));
        v.fill(&cache);
        assert_eq!(v.lines, vec!["b"]);
        assert_eq!(v.scroll, None);

        v.whole = true;
        v.fill(&cache);
        assert_eq!(v.lines, vec!["a", "b", "c"]);

        let mut missing = view(Some(("2026-11-26T00:00:00Z", "2026-11-26T00:00:01Z")));
        missing.fill(&cache);
        assert!(missing.note.is_some());
        assert_eq!(missing.lines.len(), 3);

        cache.insert(7, Err("GitHub API 410: gone".into()));
        v.fill(&cache);
        assert!(v.lines[0].starts_with("cannot load log"));
    }
}
