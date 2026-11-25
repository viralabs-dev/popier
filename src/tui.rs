//! Interactive terminal UI.
//!
//! A background task refreshes the snapshot every `refresh_secs` (or on demand)
//! and sends it to the UI loop, which only draws and handles keys.

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
use crate::github::Client;
use crate::snapshot::{self, ExpandRequest, Snapshot};
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
    };

    let mut terminal = ratatui::init();
    let result = ui_loop(
        &mut terminal,
        &mut app,
        &client,
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
    client: &Client,
    snap_rx: &mut mpsc::UnboundedReceiver<Snapshot>,
    refresh_tx: &mpsc::UnboundedSender<()>,
    expand_tx: &watch::Sender<ExpandRequest>,
) -> Result<()> {
    let request_refresh = |app: &mut App| {
        app.loading = true;
        let _ = expand_tx.send(app.expand.clone());
        let _ = refresh_tx.send(());
    };

    loop {
        while let Ok(snap) = snap_rx.try_recv() {
            app.snapshot = Some(snap);
            app.loading = false;
            app.last_refresh = Some(Instant::now());
            app.rebuild();
        }

        terminal.draw(|frame| draw(frame, app, client))?;

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
        match key.code {
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

    let mut spans = vec![Span::styled(
        " ↑↓/jk move  ⏎ fold  Tab next repo  a all  r refresh  o open  q quit ",
        Style::default().fg(Color::DarkGray),
    )];
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
