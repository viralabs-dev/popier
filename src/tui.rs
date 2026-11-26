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
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};
use tokio::sync::{mpsc, watch};

use crate::config::Config;
use crate::filter::RunFilter;
use crate::github::Client;
use crate::logs;
use crate::notify;
use crate::paint::{self, Look};
use crate::snapshot::{self, ExpandRequest, Snapshot};
use crate::status::Status;
use crate::theme::{BarStyle, Theme};
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
    theme: Theme,
    bar: BarStyle,
    /// Drives the spinner.
    started: Instant,
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
    let theme = Theme::resolve(config.settings.theme.as_deref())?;
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
        theme,
        bar: config.settings.bar,
        started: Instant::now(),
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

        if !event::poll(Duration::from_millis(100))? {
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
            KeyCode::Char('t') => {
                app.theme = app.theme.next();
                app.message = Some(format!(
                    "theme {} · keep it with theme = \"{}\" in config.toml",
                    app.theme.name, app.theme.name
                ));
            }
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
    let theme = app.theme.clone();
    let look = Look {
        theme: &theme,
        bar: app.bar,
        tick: (app.started.elapsed().as_millis() / 100) as usize,
    };
    let area = frame.area();
    frame.render_widget(
        Block::new().style(Style::new().bg(theme.bg).fg(theme.ink)),
        area,
    );
    let [head, _, main, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);

    if let Some(view) = app.log_view.as_mut() {
        let [main, footer] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
        draw_log(frame, view, &look, main, footer);
        return;
    }

    let rate = client.rate_limit();
    let info = paint::HeaderInfo {
        loading: app.loading,
        updated_secs: app.last_refresh.map(|at| at.elapsed().as_secs()),
        rate: rate.remaining.zip(rate.limit),
    };
    frame.render_widget(
        Paragraph::new(paint::header(&app.rows, &info, &look, head.width))
            .style(Style::new().bg(theme.surface)),
        head,
    );

    let items: Vec<ListItem> = paint::rows(&app.rows, &look, main.width)
        .into_iter()
        .map(ListItem::new)
        .collect();
    let list = List::new(items).highlight_style(
        Style::new()
            .bg(theme.selection)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(list, main, &mut app.list);

    let bar = Style::new().bg(theme.surface_raised);
    let tag = Style::new()
        .fg(theme.on_accent)
        .bg(theme.accent)
        .add_modifier(Modifier::BOLD);
    if let Some(input) = &app.filter_input {
        let line = Line::from(vec![
            Span::styled(" filter ", tag),
            Span::styled(format!(" {input}"), Style::new().fg(theme.ink)),
            Span::styled("▏", Style::new().fg(theme.accent)),
            Span::styled(
                "   branch part or #PR · ⏎ keep · Esc clear",
                Style::new().fg(theme.muted),
            ),
        ]);
        frame.render_widget(Paragraph::new(line).style(bar), footer);
        return;
    }

    let mut spans = paint::hints(
        &[
            ("↑↓", "move"),
            ("⏎", "fold"),
            ("⇥", "next repo"),
            ("/", "filter"),
            ("l", "log"),
            ("t", "theme"),
            ("a", "all"),
            ("r", "refresh"),
            ("o", "open"),
            ("q", "quit"),
        ],
        &look,
    );
    if !app.filter_text.is_empty() {
        spans.push(Span::styled(format!(" filter: {} ", app.filter_text), tag));
        spans.push(Span::styled(" Esc clears ", Style::new().fg(theme.muted)));
    }
    if let Some(msg) = &app.message {
        spans.push(Span::styled(format!("  {msg}"), Style::new().fg(theme.ink)));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(bar), footer);
}

fn draw_log(
    frame: &mut ratatui::Frame,
    view: &mut LogView,
    look: &Look,
    main: ratatui::layout::Rect,
    footer: ratatui::layout::Rect,
) {
    let theme = look.theme;
    let height = main.height.saturating_sub(2) as usize;
    let max_scroll = view.lines.len().saturating_sub(height);
    let scroll = view.scroll.map_or(max_scroll, |s| s.min(max_scroll));
    view.scroll = view.scroll.map(|_| scroll);

    let lines: Vec<Line> = view.lines.iter().map(|l| log_line(l, theme)).collect();
    let scope = if view.step.is_none() || view.whole {
        "job"
    } else {
        "step"
    };
    let title = Line::from(vec![
        Span::styled(
            " ◆ ",
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("log ({scope}) · {} ", view.title),
            Style::new().fg(theme.ink).add_modifier(Modifier::BOLD),
        ),
    ]);
    let paragraph = Paragraph::new(lines)
        .block(
            Block::bordered()
                .border_style(Style::new().fg(theme.line))
                .title(title),
        )
        .style(Style::new().fg(theme.ink).bg(theme.bg))
        .scroll((scroll as u16, 0));
    frame.render_widget(paragraph, main);

    let mut spans = paint::hints(
        &[
            ("↑↓", "scroll"),
            ("PgUp/PgDn", "page"),
            ("g/G", "top/bottom"),
            ("w", "step/job"),
            ("Esc", "close"),
        ],
        look,
    );
    spans.push(Span::styled(
        format!(
            "{}/{} lines",
            (scroll + height).min(view.lines.len()),
            view.lines.len()
        ),
        Style::new().fg(theme.muted),
    ));
    if let Some(note) = &view.note {
        spans.push(Span::styled(
            format!("  {note}"),
            Style::new().fg(theme.running),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::new().bg(theme.surface_raised)),
        footer,
    );
}

/// Colors GitHub's log markers (`##[error]`, `##[group]`, …).
fn log_line(line: &str, theme: &Theme) -> Line<'static> {
    let style = if line.starts_with("##[error]") {
        Style::new().fg(theme.failure)
    } else if line.starts_with("##[warning]") {
        Style::new().fg(theme.running)
    } else if line.starts_with("##[group]") || line.starts_with("##[endgroup]") {
        Style::new().fg(theme.muted).add_modifier(Modifier::BOLD)
    } else {
        Style::new()
    };
    Line::from(Span::styled(line.to_owned(), style))
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

    /// A busy snapshot: one repo running, one failed, one queued, one calm.
    fn demo() -> Snapshot {
        use crate::snapshot::{JobView, RepoView, StepView, WorkflowView};
        let t = |secs_ago: i64| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            let s = now - secs_ago;
            // Unix seconds back to RFC 3339 (civil from days).
            let (days, rem) = (s.div_euclid(86_400), s.rem_euclid(86_400));
            let z = days + 719_468;
            let era = z.div_euclid(146_097);
            let doe = z - era * 146_097;
            let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
            let mp = (5 * doy + 2) / 153;
            let d = doy - (153 * mp + 2) / 5 + 1;
            let m = if mp < 10 { mp + 3 } else { mp - 9 };
            let y = yoe + era * 400 + i64::from(m <= 2);
            Some(format!(
                "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
                rem / 3600,
                rem % 3600 / 60,
                rem % 60
            ))
        };
        let step = |n: u32, name: &str, status: Status, a: i64, b: Option<i64>| StepView {
            number: n,
            name: name.into(),
            status,
            started_at: if status == Status::Queued { None } else { t(a) },
            completed_at: b.and_then(t),
        };
        let wf = |id: u64, name: &str, status: Status, start: i64, end: i64, jobs| WorkflowView {
            workflow_id: id,
            run_id: id,
            name: name.into(),
            branch: Some("main".into()),
            status,
            url: String::new(),
            started_at: t(start),
            updated_at: t(end),
            jobs,
        };
        let job = |id: u64, name: &str, status: Status, steps| JobView {
            id,
            name: name.into(),
            status,
            url: None,
            steps,
        };
        let repo = |name: &str, workflows| RepoView {
            name: name.into(),
            slug: format!("o/{name}"),
            workflows: Ok(workflows),
        };
        Snapshot {
            repos: vec![
                repo(
                    "brabo-web",
                    vec![
                        wf(1, "CI", Status::Success, 900, 680, None),
                        wf(
                            2,
                            "Deploy",
                            Status::Running,
                            62,
                            0,
                            Some(Ok(vec![
                                job(
                                    20,
                                    "build",
                                    Status::Success,
                                    vec![
                                        step(1, "Set up job", Status::Success, 62, Some(60)),
                                        step(2, "Build", Status::Success, 60, Some(24)),
                                    ],
                                ),
                                job(
                                    21,
                                    "deploy",
                                    Status::Running,
                                    vec![
                                        step(1, "Set up job", Status::Success, 24, Some(21)),
                                        step(2, "Upload assets", Status::Running, 21, None),
                                        step(3, "Invalidate CDN", Status::Queued, 0, None),
                                    ],
                                ),
                            ])),
                        ),
                    ],
                ),
                repo(
                    "brabo-api",
                    vec![wf(
                        3,
                        "CI",
                        Status::Failure,
                        300,
                        166,
                        Some(Ok(vec![job(
                            30,
                            "test",
                            Status::Failure,
                            vec![
                                step(1, "Set up job", Status::Success, 300, Some(298)),
                                step(2, "Run tests", Status::Failure, 298, Some(166)),
                                step(3, "Upload coverage", Status::Skipped, 166, Some(166)),
                            ],
                        )])),
                    )],
                ),
                repo(
                    "brabo-broker",
                    vec![wf(4, "CI", Status::Queued, 30, 30, Some(Ok(vec![])))],
                ),
                repo(
                    "brabo-engine",
                    vec![
                        wf(5, "CI", Status::Success, 2000, 1800, None),
                        wf(6, "Release", Status::Success, 9000, 8000, None),
                    ],
                ),
            ],
        }
    }

    fn demo_app(theme: &str) -> App {
        let mut app = App {
            snapshot: Some(demo()),
            rows: Vec::new(),
            list: ListState::default(),
            folds: Folds::default(),
            expand: ExpandRequest::default(),
            loading: false,
            message: None,
            last_refresh: Some(Instant::now()),
            watcher: None,
            filter_input: None,
            filter_text: String::new(),
            log_view: None,
            log_cache: HashMap::new(),
            theme: Theme::builtin(theme).unwrap(),
            bar: BarStyle::Line,
            started: Instant::now(),
        };
        app.rebuild();
        app.list.select(Some(3));
        app
    }

    #[test]
    fn draws_every_theme_at_every_width() {
        use ratatui::{Terminal, backend::TestBackend};
        let client = Client::new("test".into()).unwrap();
        for theme in crate::theme::BUILTIN {
            for width in [50, 79, 90, 120, 160] {
                let mut app = demo_app(theme);
                let mut term = Terminal::new(TestBackend::new(width, 24)).unwrap();
                term.draw(|f| draw(f, &mut app, &client)).unwrap();
                let buffer = term.backend().buffer();
                let line: String = (0..width).map(|x| buffer[(x, 0)].symbol()).collect();
                assert!(line.contains("popier"), "{theme} at {width}: {line}");
            }
        }
    }

    /// Writes an HTML picture of the dashboard: `POPIER_RENDER=<dir> cargo test render -- --ignored`.
    #[test]
    #[ignore]
    fn render_html() {
        use ratatui::style::Color;
        use ratatui::{Terminal, backend::TestBackend};
        let dir = std::env::var("POPIER_RENDER").unwrap();
        let client = Client::new("test".into()).unwrap();
        for theme in crate::theme::BUILTIN {
            let mut app = demo_app(theme);
            let (w, h) = (112, 22);
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| draw(f, &mut app, &client)).unwrap();
            let buf = term.backend().buffer();
            let hex = |c: Color, d: &str| match c {
                Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
                Color::Reset => d.into(),
                other => format!("{other}").to_lowercase(),
            };
            let mut html = String::from(
                "<pre style=\"font:13px/20px 'JetBrains Mono',monospace;margin:0;background:#111;color:#ddd\">",
            );
            for y in 0..h {
                for x in 0..w {
                    let c = &buf[(x, y)];
                    let bold = c.modifier.contains(Modifier::BOLD);
                    html.push_str(&format!(
                        "<span style=\"color:{};background:{};font-weight:{}\">{}</span>",
                        hex(c.fg, "inherit"),
                        hex(c.bg, "transparent"),
                        if bold { 700 } else { 400 },
                        c.symbol().replace('&', "&amp;").replace('<', "&lt;")
                    ));
                }
                html.push('\n');
            }
            html.push_str("</pre>");
            std::fs::write(format!("{dir}/{theme}.html"), html).unwrap();
        }
    }
}
