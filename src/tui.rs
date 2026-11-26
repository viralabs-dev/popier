//! Interactive terminal UI.
//!
//! A background task refreshes the snapshot every `refresh_secs` (or on demand)
//! and sends it to the UI loop, which only draws and handles keys.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use ratatui::DefaultTerminal;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};
use tokio::sync::{mpsc, watch};

use crate::config::Config;
use crate::detail;
use crate::filter::{self, RunFilter};
use crate::github::Client;
use crate::logs;
use crate::notify;
use crate::paint::{self, Look};
use crate::snapshot::{self, ExpandRequest, RepoView, Snapshot, WorkflowView};
use crate::status::Status;
use crate::theme::{BarStyle, Theme};
use crate::tree::{self, Folds, Row, RowKey};

struct App {
    snapshot: Option<Snapshot>,
    rows: Vec<Row>,
    /// Every repo row with its group, also inside folded groups, for the header.
    repo_rows: Vec<(Option<String>, Row)>,
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
    /// Job logs to download (slug, job id), and the ids already asked for.
    log_wanted: Vec<(String, u64)>,
    log_asked: std::collections::HashSet<u64>,
    theme: Theme,
    bar: BarStyle,
    /// Drives the spinner.
    started: Instant,
    /// Details sidebar open (`d`).
    details: bool,
    /// Where the tree list was last drawn, to map mouse clicks to rows.
    list_area: Rect,
    /// Runs opened with `A` before their jobs were fetched: their steps open
    /// when the jobs arrive.
    open_steps_when_loaded: HashSet<u64>,
    /// Top-level group or repo the tree is narrowed to (`f`), with its name.
    focus: Option<(RowKey, String)>,
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
                (Some((number, spans)), false) => match step_lines(log, spans, *number) {
                    Some(lines) => logs::tail(lines, logs::STEP_TAIL),
                    None => {
                        self.note = Some("step not found in log; showing end of job".into());
                        logs::tail(logs::whole_job(log), logs::STEP_TAIL)
                    }
                },
                _ => logs::whole_job(log),
            },
        };
        self.scroll = None;
    }
}

/// The step's own lines of the job log: by its `##[group]Run` marker, else by
/// its start and end times.
fn step_lines(log: &str, spans: &[logs::StepSpan], number: u32) -> Option<Vec<String>> {
    logs::cut_step(log, spans, number).or_else(|| {
        let span = spans.iter().find(|s| s.number == number)?;
        logs::slice_step(log, span.started.as_deref(), span.completed.as_deref())
    })
}

/// Log lines shown under an open step in the tree.
const TREE_LOG_LINES: usize = 8;
/// Log lines shown in the details sidebar for a step.
const PANEL_LOG_LINES: usize = 10;

/// What is known about a step's log.
enum StepLog {
    Loading,
    Failed(String),
    /// The step's lines, or the whole job when the step was not found in it.
    Lines(Vec<String>),
}

impl App {
    fn rebuild(&mut self) {
        let selected_key = self
            .list
            .selected()
            .and_then(|i| self.rows.get(i))
            .map(|r| r.key.clone());
        let search = filter::search_text(&self.filter_text);
        if let (Some(s), Some((key, _))) = (self.snapshot.as_ref(), &self.focus) {
            // The focused group or repo left the config: back to the full view.
            if tree::subtree(tree::build(s, &self.folds), key).is_empty() {
                self.focus = None;
            }
        }
        let focus = self.focus.as_ref().map(|(key, _)| key);
        let build = |s: &Snapshot, folds: &Folds| match focus {
            Some(key) => tree::subtree(tree::build(s, folds), key),
            None => tree::build(s, folds),
        };
        self.rows = match (self.snapshot.as_ref(), &search) {
            (None, _) => Vec::new(),
            (Some(s), None) => build(s, &self.folds),
            (Some(s), Some(query)) => {
                let rows = tree::search(build(s, &tree::search_folds(s, &self.folds)), query);
                if rows.is_empty() {
                    vec![tree::note(
                        "",
                        0,
                        format!("nothing matches \"{query}\""),
                        false,
                    )]
                } else {
                    rows
                }
            }
        };
        self.add_log_rows();
        self.repo_rows = self
            .snapshot
            .as_ref()
            .map(tree::repo_rows)
            .unwrap_or_default();
        let index = selected_key
            .and_then(|key| self.rows.iter().position(|r| r.key == key))
            .or(if self.rows.is_empty() { None } else { Some(0) });
        self.list.select(index);
    }

    /// Puts the end of each open step's log right below the step row.
    fn add_log_rows(&mut self) {
        let rows = std::mem::take(&mut self.rows);
        for row in rows {
            let open = match row.key {
                RowKey::Step(run, job, step) if row.expanded => Some((run, job, step)),
                _ => None,
            };
            let depth = row.depth + 1;
            let url = row.url.clone();
            self.rows.push(row);
            let Some((run, job, step)) = open else {
                continue;
            };
            let line = |i: usize, text: String, is_error: bool, hint: Option<String>| Row {
                key: RowKey::Log(run, job, step, i),
                depth,
                status: None,
                text,
                detail: hint,
                is_error,
                url: url.clone(),
                foldable: false,
                expanded: false,
                gauge: None,
                branch: None,
                lead: None,
                health: None,
            };
            match self.step_log(run, job, step) {
                StepLog::Loading => self.rows.push(line(0, "loading log…".into(), true, None)),
                StepLog::Failed(err) => {
                    self.rows
                        .push(line(0, format!("cannot load log: {err}"), true, None))
                }
                StepLog::Lines(lines) => {
                    let total = lines.len();
                    let shown = logs::tail(lines, TREE_LOG_LINES);
                    let n = shown.len();
                    for (i, text) in shown.into_iter().enumerate() {
                        self.rows.push(line(i, text, false, None));
                    }
                    let more = if total > n {
                        format!("{} more above · {total} lines", total - n)
                    } else {
                        format!("{total} lines")
                    };
                    self.rows
                        .push(line(n, more, false, Some("full log".into())));
                }
            }
        }
    }

    /// Repo slug, job id and step spans for a job row's address.
    fn job_ref(&self, run_id: u64, job_idx: usize) -> Option<(String, u64, Vec<logs::StepSpan>)> {
        let snapshot = self.snapshot.as_ref()?;
        snapshot.repos.iter().find_map(|repo| {
            let wf = repo
                .workflows
                .as_ref()
                .ok()?
                .iter()
                .find(|w| w.run_id == run_id)?;
            let job = wf.jobs.as_ref()?.as_ref().ok()?.get(job_idx)?;
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
            Some((repo.slug.clone(), job.id, spans))
        })
    }

    /// The step's log from the cache; asks for the job log when it is missing.
    fn step_log(&mut self, run: u64, job: usize, step: u32) -> StepLog {
        let Some((slug, job_id, spans)) = self.job_ref(run, job) else {
            return StepLog::Failed("job not found".into());
        };
        match self.log_cache.get(&job_id) {
            None => {
                if self.log_asked.insert(job_id) {
                    self.log_wanted.push((slug, job_id));
                }
                StepLog::Loading
            }
            Some(Err(err)) => StepLog::Failed(err.clone()),
            Some(Ok(log)) => StepLog::Lines(
                step_lines(log, &spans, step).unwrap_or_else(|| logs::whole_job(log)),
            ),
        }
    }

    /// The last lines of the selected step's log, for the details sidebar.
    fn panel_log(&mut self) -> Option<Vec<String>> {
        let (run, job, step) = match self.selected()?.key {
            RowKey::Step(run, job, step) | RowKey::Log(run, job, step, _) => (run, job, step),
            _ => return None,
        };
        match self.step_log(run, job, step) {
            StepLog::Lines(lines) => Some(logs::tail(lines, PANEL_LOG_LINES)),
            StepLog::Loading => Some(vec!["loading log…".into()]),
            StepLog::Failed(err) => Some(vec![format!("cannot load log: {err}")]),
        }
    }

    /// Logs of open steps whose job is still running grow; fetch them again
    /// (the old text stays on screen until the new one arrives).
    fn refetch_live_logs(&mut self) {
        let live: Vec<(String, u64)> = self
            .folds
            .open_steps
            .iter()
            .filter_map(|&(run, job, _)| {
                let snapshot = self.snapshot.as_ref()?;
                let status = snapshot.repos.iter().find_map(|repo| {
                    let wf = repo
                        .workflows
                        .as_ref()
                        .ok()?
                        .iter()
                        .find(|w| w.run_id == run)?;
                    Some(wf.jobs.as_ref()?.as_ref().ok()?.get(job)?.status)
                })?;
                let (slug, id, _) = self.job_ref(run, job)?;
                matches!(status, Status::Running | Status::Queued).then_some((slug, id))
            })
            .collect();
        for (slug, id) in live {
            if self.log_cache.contains_key(&id) && !self.log_wanted.iter().any(|(_, j)| *j == id) {
                self.log_wanted.push((slug, id));
            }
        }
    }

    /// Whether any open step belongs to this job.
    fn shows_job_log(&self, job_id: u64) -> bool {
        self.folds.open_steps.iter().any(|&(run, job, _)| {
            self.job_ref(run, job)
                .is_some_and(|(_, id, _)| id == job_id)
        })
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

    /// Moves to the next (or previous) top-level row: a group or a repo
    /// outside groups. Wraps around: past the last one comes the first.
    fn jump_repo(&mut self, forward: bool) {
        let tops: Vec<usize> = (0..self.rows.len())
            .filter(|&i| self.rows[i].depth == 0)
            .collect();
        let (Some(&first), Some(&last)) = (tops.first(), tops.last()) else {
            return;
        };
        let current = self.list.selected().unwrap_or(0);
        let target = if forward {
            tops.iter().copied().find(|&i| i > current).unwrap_or(first)
        } else {
            tops.iter()
                .rev()
                .copied()
                .find(|&i| i < current)
                .unwrap_or(last)
        };
        self.list.select(Some(target));
    }

    /// Toggles the selected row. Returns true when new data must be fetched.
    fn toggle(&mut self) -> bool {
        let Some(row) = self.selected().cloned() else {
            return false;
        };
        let mut needs_fetch = false;
        match &row.key {
            RowKey::Group(name) => {
                if !self.folds.collapsed_groups.remove(name) {
                    self.folds.collapsed_groups.insert(name.clone());
                }
            }
            RowKey::Calm(name) => {
                if !self.folds.open_calm.remove(name) {
                    self.folds.open_calm.insert(name.clone());
                }
            }
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
            RowKey::Step(run, job, step) | RowKey::Log(run, job, step, _) => {
                let key = (*run, *job, *step);
                if !self.folds.open_steps.remove(&key) {
                    self.folds.open_steps.insert(key);
                }
                // Keep the step selected when its log lines go away.
                let step_key = RowKey::Step(*run, *job, *step);
                self.rebuild();
                if let Some(i) = self.rows.iter().position(|r| r.key == step_key) {
                    self.list.select(Some(i));
                }
                return false;
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
            RowKey::Step(run, job, step) | RowKey::Log(run, job, step, _) => (run, job, Some(step)),
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

    /// `f`: narrows the tree to the top-level group or repo of the selected
    /// row, or goes back to the full view.
    fn toggle_focus(&mut self) {
        if self.focus.take().is_some() {
            self.message = Some("full view".into());
        } else {
            let Some(i) = self.list.selected() else {
                return;
            };
            let Some(top) = self.rows[..=i.min(self.rows.len().saturating_sub(1))]
                .iter()
                .rev()
                .find(|r| r.depth == 0)
            else {
                return;
            };
            if !matches!(top.key, RowKey::Group(_) | RowKey::Repo(_)) {
                return;
            }
            self.focus = Some((top.key.clone(), top.text.clone()));
        }
        self.rebuild();
    }

    /// `a`: closes everything when anything is open, otherwise opens every
    /// group, repo and workflow (fetching the jobs of the quiet ones).
    /// Returns whether a refresh is needed.
    fn toggle_all(&mut self) -> bool {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return false;
        };
        let groups: HashSet<String> = snapshot
            .repos
            .iter()
            .filter_map(|r| r.group.clone())
            .collect();
        let anything_open = self.rows.iter().any(|r| r.depth > 0);
        let needs_fetch = if anything_open {
            self.folds = Folds {
                collapsed_groups: groups,
                collapsed_repos: snapshot.repos.iter().map(|r| r.slug.clone()).collect(),
                collapsed_workflows: snapshot
                    .repos
                    .iter()
                    .flat_map(workflows)
                    .map(|w| w.run_id)
                    .collect(),
                ..Folds::default()
            };
            self.expand.all = false;
            self.expand.pinned.clear();
            self.open_steps_when_loaded.clear();
            self.message = Some("closed everything · a opens it all".into());
            false
        } else {
            self.folds = Folds {
                open_calm: groups,
                ..Folds::default()
            };
            self.expand.all = true;
            self.message = Some("opened everything · a closes it all".into());
            true
        };
        self.rebuild();
        needs_fetch
    }

    /// `A`: opens or closes the selected row and everything under it, down to
    /// the steps' logs. Returns whether a refresh is needed.
    fn toggle_subtree(&mut self) -> bool {
        let Some(row) = self.selected().cloned() else {
            return false;
        };
        let Some(snapshot) = self.snapshot.as_ref() else {
            return false;
        };
        // What the row covers: repos, and within them one run or one job.
        let (slugs, run, job): (Vec<String>, Option<u64>, Option<usize>) = match &row.key {
            RowKey::Group(name) | RowKey::Calm(name) => (
                snapshot
                    .repos
                    .iter()
                    .filter(|r| r.group.as_deref() == Some(name.as_str()))
                    .map(|r| r.slug.clone())
                    .collect(),
                None,
                None,
            ),
            RowKey::Repo(slug) => (vec![slug.clone()], None, None),
            RowKey::Workflow(run) => (Vec::new(), Some(*run), None),
            RowKey::Job(run, job) => (Vec::new(), Some(*run), Some(*job)),
            RowKey::Step(..) | RowKey::Log(..) => return self.toggle(),
            RowKey::Note(..) => return false,
        };
        let in_scope =
            |slug: &str, w: &WorkflowView| slugs.iter().any(|s| s == slug) || run == Some(w.run_id);
        let scoped: Vec<(String, &WorkflowView)> = snapshot
            .repos
            .iter()
            .flat_map(|r| workflows(r).map(move |w| (r.slug.clone(), w)))
            .filter(|(slug, w)| in_scope(slug, w))
            .collect();
        let steps: Vec<(u64, usize, u32)> = scoped
            .iter()
            .flat_map(|(_, w)| foldable_steps(w))
            .filter(|&(_, j, _)| job.is_none_or(|want| want == j))
            .collect();
        let closing = match row.key {
            RowKey::Job(..) => steps.iter().any(|s| self.folds.open_steps.contains(s)),
            _ => row.expanded,
        };

        let mut needs_fetch = false;
        if closing {
            for (_, w) in &scoped {
                self.open_steps_when_loaded.remove(&w.run_id);
            }
            for s in &steps {
                self.folds.open_steps.remove(s);
            }
            if job.is_none() {
                for (_, w) in &scoped {
                    self.folds.collapsed_workflows.insert(w.run_id);
                    self.expand.pinned.remove(&w.run_id);
                }
                for slug in &slugs {
                    self.folds.collapsed_repos.insert(slug.clone());
                }
            }
            match &row.key {
                RowKey::Group(name) => {
                    self.folds.collapsed_groups.insert(name.clone());
                }
                RowKey::Calm(name) => {
                    self.folds.open_calm.remove(name);
                }
                _ => {}
            }
        } else {
            match &row.key {
                RowKey::Group(name) | RowKey::Calm(name) => {
                    self.folds.collapsed_groups.remove(name);
                    self.folds.open_calm.insert(name.clone());
                }
                _ => {}
            }
            for slug in &slugs {
                self.folds.collapsed_repos.remove(slug);
            }
            for (_, w) in &scoped {
                self.folds.collapsed_workflows.remove(&w.run_id);
                if w.jobs.is_none() {
                    self.expand.pinned.insert(w.run_id);
                    self.open_steps_when_loaded.insert(w.run_id);
                    needs_fetch = true;
                }
            }
            self.folds.open_steps.extend(steps);
        }
        let key = row.key.clone();
        self.rebuild();
        if let Some(i) = self.rows.iter().position(|r| r.key == key) {
            self.list.select(Some(i));
        }
        needs_fetch
    }

    /// Opens the steps of runs opened with `A` whose jobs just arrived.
    fn open_loaded_steps(&mut self) {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return;
        };
        for w in snapshot.repos.iter().flat_map(workflows) {
            if w.jobs.is_some() && self.open_steps_when_loaded.remove(&w.run_id) {
                self.folds.open_steps.extend(foldable_steps(w));
            }
        }
    }

    fn has_jobs(&self, run_id: u64) -> bool {
        self.snapshot.iter().flat_map(|s| &s.repos).any(|repo| {
            repo.workflows
                .as_ref()
                .is_ok_and(|wfs| wfs.iter().any(|w| w.run_id == run_id && w.jobs.is_some()))
        })
    }
}

/// The workflows of a repo, or none when it failed to load.
fn workflows(repo: &RepoView) -> impl Iterator<Item = &WorkflowView> {
    repo.workflows.iter().flatten()
}

/// Steps of a workflow that can show a log (run, job index, step number).
fn foldable_steps(w: &WorkflowView) -> impl Iterator<Item = (u64, usize, u32)> + '_ {
    let jobs = match &w.jobs {
        Some(Ok(jobs)) => jobs.as_slice(),
        _ => &[],
    };
    jobs.iter().enumerate().flat_map(move |(j, job)| {
        job.steps
            .iter()
            .filter(|s| !matches!(s.status, Status::Queued | Status::Skipped))
            .map(move |s| (w.run_id, j, s.number))
    })
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
        repo_rows: Vec::new(),
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
        log_wanted: Vec::new(),
        log_asked: Default::default(),
        theme,
        bar: config.settings.bar,
        started: Instant::now(),
        details: false,
        list_area: Rect::default(),
        open_steps_when_loaded: HashSet::new(),
        focus: None,
    };

    let mut terminal = ratatui::init();
    // Clicks fold and unfold rows; hold Shift to select text as usual.
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let result = ui_loop(
        &mut terminal,
        &mut app,
        client.clone(),
        &mut snap_rx,
        &refresh_tx,
        &expand_tx,
    );
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
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

    // Applies `text` as the filter: a search narrows the tree at once; a
    // `#PR` filter refetches runs. Notifications pause while a PR filter is
    // on (the filtered view is not the real "latest run" of each workflow) and
    // resume silently when it is cleared.
    let apply_filter = |app: &mut App, text: &str| {
        app.filter_text = text.trim().to_owned();
        let filter = RunFilter::parse(text);
        if filter != app.expand.filter {
            app.expand.filter = filter;
            if let Some(watcher) = app.watcher.as_mut() {
                watcher.reset();
            }
            request_refresh(app);
        }
        app.rebuild();
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
            app.open_loaded_steps();
            app.loading = false;
            app.last_refresh = Some(Instant::now());
            app.refetch_live_logs();
            app.rebuild();
        }

        for (slug, job_id) in std::mem::take(&mut app.log_wanted) {
            let (client, tx) = (client.clone(), log_tx.clone());
            tokio::spawn(async move {
                let log = client
                    .job_log(&slug, job_id)
                    .await
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send((job_id, log));
            });
        }

        while let Ok((job_id, log)) = log_rx.try_recv() {
            app.log_cache.insert(job_id, log);
            if app.shows_job_log(job_id) {
                app.rebuild();
            }
            if let Some(view) = app.log_view.as_mut().filter(|v| v.job_id == job_id) {
                view.fill(&app.log_cache);
            }
        }

        terminal.draw(|frame| draw(frame, app, &client))?;

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let key = match event::read()? {
            Event::Key(key) => key,
            Event::Mouse(mouse) => {
                if on_mouse(app, mouse) {
                    request_refresh(app);
                }
                continue;
            }
            _ => continue,
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
            KeyCode::Esc if !app.filter_text.is_empty() => apply_filter(app, ""),
            KeyCode::Esc if app.focus.is_some() => app.toggle_focus(),
            KeyCode::Char('f') => app.toggle_focus(),
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
                if app.toggle_all() {
                    request_refresh(app);
                }
            }
            KeyCode::Char('A') => {
                if app.toggle_subtree() {
                    request_refresh(app);
                }
            }
            KeyCode::Char('r') => request_refresh(app),
            KeyCode::Char('d') => app.details = !app.details,
            KeyCode::Right => app.details = true,
            KeyCode::Left if app.details => app.details = false,
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
                    if !app.log_cache.contains_key(&view.job_id)
                        && app.log_asked.insert(view.job_id)
                    {
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
    let portrait = detail::portrait(area.width, area.height);
    let tag = Style::new()
        .fg(theme.on_accent)
        .bg(theme.accent)
        .add_modifier(Modifier::BOLD);
    let mut spans = paint::hints(
        &[
            ("↑↓", "move"),
            ("⏎", "fold"),
            ("⇥", "next repo"),
            ("f", "focus"),
            ("/", "filter"),
            ("l", "log"),
            ("d", "details"),
            ("t", "theme"),
            ("a", "all"),
            ("A", "item"),
            ("r", "refresh"),
            ("o", "open"),
            ("q", "quit"),
        ],
        &look,
    );
    if let Some((_, name)) = &app.focus {
        spans.push(Span::styled(format!(" focus: {name} "), tag));
        spans.push(Span::styled(" f/Esc: all ", Style::new().fg(theme.muted)));
    }
    if !app.filter_text.is_empty() {
        spans.push(Span::styled(format!(" filter: {} ", app.filter_text), tag));
        spans.push(Span::styled(" Esc clears ", Style::new().fg(theme.muted)));
    }
    if let Some(msg) = &app.message {
        spans.push(Span::styled(format!("  {msg}"), Style::new().fg(theme.ink)));
    }
    // On a tall, narrow terminal the hints take two lines instead of being cut.
    let footer_lines = paint::fit_hints(spans, area.width as usize, if portrait { 2 } else { 1 });
    let [head, _, main, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(footer_lines.len().max(1) as u16),
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
        Paragraph::new(paint::header(&app.repo_rows, &info, &look, head.width))
            .style(Style::new().bg(theme.surface)),
        head,
    );

    let (main, side) = match (app.details, detail::placement(area.width, area.height)) {
        (false, _) => (main, None),
        (true, detail::Placement::Right(w)) => {
            let [list, side] =
                Layout::horizontal([Constraint::Min(1), Constraint::Length(w)]).areas(main);
            (list, Some(side))
        }
        // A tall, narrow terminal (a monitor turned upright): the panel goes
        // under the tree, full width.
        (true, detail::Placement::Below(h)) => {
            let [list, side] =
                Layout::vertical([Constraint::Min(1), Constraint::Length(h)]).areas(main);
            (list, Some(side))
        }
        // Too small for both: the panel covers the list.
        (true, detail::Placement::Cover) => (main, Some(main)),
    };
    app.list_area = main;
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
    if let Some(side) = side {
        draw_details(frame, app, &look, side);
    }

    let bar = Style::new().bg(theme.surface_raised);
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

    frame.render_widget(
        Paragraph::new(footer_lines).style(Style::new().bg(theme.surface_raised)),
        footer,
    );
}

fn draw_details(frame: &mut ratatui::Frame, app: &mut App, look: &Look, area: Rect) {
    let theme = look.theme;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let inner_w = area.width.saturating_sub(4) as usize;
    let log = app.panel_log();
    let lines = match (app.snapshot.as_ref(), app.selected()) {
        (Some(snap), Some(row)) => detail::lines(snap, row, look, inner_w, now, log.as_deref()),
        _ => vec![Line::from(Span::styled(
            "select a row to see its details",
            Style::new().fg(theme.muted),
        ))],
    };
    let title = Line::from(vec![
        Span::styled(
            " ◆ ",
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "details ",
            Style::new().fg(theme.ink).add_modifier(Modifier::BOLD),
        ),
    ]);
    let panel = Paragraph::new(lines)
        .block(
            Block::bordered()
                .border_style(Style::new().fg(theme.line))
                .title(title)
                .padding(ratatui::widgets::Padding::horizontal(1)),
        )
        .style(Style::new().fg(theme.ink).bg(theme.surface));
    frame.render_widget(ratatui::widgets::Clear, area);
    frame.render_widget(panel, area);
}

/// A click selects a row and folds or unfolds it; the wheel moves the
/// selection (or scrolls the log). Returns true when new data must be fetched.
fn on_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if let Some(view) = app.log_view.as_mut() {
        let current = view.scroll.unwrap_or(usize::MAX);
        match mouse.kind {
            MouseEventKind::ScrollDown => view.scroll = Some(current.saturating_add(3)),
            MouseEventKind::ScrollUp => view.scroll = Some(current.saturating_sub(3)),
            _ => {}
        }
        return false;
    }
    match mouse.kind {
        MouseEventKind::ScrollDown => app.move_by(3),
        MouseEventKind::ScrollUp => app.move_by(-3),
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(index) = clicked_row(app.list_area, app.list.offset(), mouse)
                && index < app.rows.len()
            {
                app.list.select(Some(index));
                if app.rows[index].foldable {
                    return app.toggle();
                }
            }
        }
        _ => {}
    }
    false
}

/// Row index under the mouse, if it is inside the list.
fn clicked_row(area: Rect, offset: usize, mouse: MouseEvent) -> Option<usize> {
    let inside = mouse.column >= area.x
        && mouse.column < area.x + area.width
        && mouse.row >= area.y
        && mouse.row < area.y + area.height;
    inside.then(|| offset + (mouse.row - area.y) as usize)
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

    let lines = log_lines(&view.lines, theme);
    let scope = if view.step.is_none() || view.whole {
        "job"
    } else {
        "step"
    };
    let mut title = vec![Span::styled(
        " ◆ log ",
        Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
    )];
    let parts: Vec<&str> = view.title.split(" › ").collect();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            title.push(Span::styled(" › ", Style::new().fg(theme.faint)));
        }
        let style = if i + 1 == parts.len() {
            Style::new().fg(theme.ink).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.muted)
        };
        title.push(Span::styled(part.to_string(), style));
    }
    title.push(Span::raw(" "));
    let chip = Line::from(vec![
        Span::styled(
            format!(" {scope} "),
            Style::new()
                .fg(theme.on_accent)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ])
    .right_aligned();
    let paragraph = Paragraph::new(lines)
        .block(
            Block::bordered()
                .border_style(Style::new().fg(theme.line))
                .title(Line::from(title))
                .title(chip),
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
    let shown = (scroll + height).min(view.lines.len());
    let errors = view
        .lines
        .iter()
        .filter(|l| l.starts_with("##[error]"))
        .count();
    spans.push(Span::styled(
        format!("{shown}/{} lines", view.lines.len()),
        Style::new().fg(theme.muted),
    ));
    if errors > 0 {
        spans.push(Span::styled(
            format!("  ✗ {errors} error{}", if errors == 1 { "" } else { "s" }),
            Style::new().fg(theme.failure),
        ));
    }
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

/// Draws log lines with a line-number gutter and GitHub's markers
/// (`##[error]`, `##[warning]`, `##[group]`, `##[command]`, …) as glyphs.
/// Lines inside a group are indented. One output line per input line, so
/// scrolling stays in step with `view.lines`.
fn log_lines(lines: &[String], theme: &Theme) -> Vec<Line<'static>> {
    let digits = lines.len().max(1).to_string().len();
    let mut depth = 0usize;
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let (mark, text, style) = paint::log_marker(line, theme);
            if line.starts_with("##[endgroup]") {
                depth = depth.saturating_sub(1);
            }
            let indent = "  ".repeat(depth);
            if line.starts_with("##[group]") {
                depth += 1;
            }
            let gutter_style = match mark {
                Some((_, s)) => s,
                None => Style::new().fg(theme.faint),
            };
            let mut spans = vec![
                Span::styled(format!("{:>digits$} ", i + 1), gutter_style),
                Span::styled("│ ", Style::new().fg(theme.line)),
                Span::raw(indent),
            ];
            if let Some((glyph, s)) = mark {
                spans.push(Span::styled(format!("{glyph} "), s));
            }
            spans.push(Span::styled(text, style));
            Line::from(spans)
        })
        .collect()
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
            group: name.starts_with("brabo").then(|| "brabo".to_owned()),
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
                repo(
                    "brabo-contracts",
                    vec![wf(7, "CI", Status::Success, 4000, 3800, None)],
                ),
                repo(
                    "brabo-qa",
                    vec![wf(8, "CI", Status::Success, 5000, 4700, None)],
                ),
                repo("hud", vec![wf(9, "CI", Status::Success, 700, 520, None)]),
            ],
        }
    }

    fn sample_log() -> LogView {
        let lines = [
            "##[group]Run cargo test --locked",
            "##[command]cargo test --locked",
            "    Finished `test` profile [unoptimized + debuginfo] target(s) in 41.20s",
            "     Running unittests src/main.rs (target/debug/deps/api-3f2c)",
            "running 42 tests",
            "test auth::rejects_expired_token ... ok",
            "test billing::rounds_half_even ... ok",
            "test orders::cancels_after_payment ... FAILED",
            "test orders::lists_by_customer ... ok",
            "##[endgroup]",
            "failures:",
            "---- orders::cancels_after_payment stdout ----",
            "thread 'orders::cancels_after_payment' panicked at src/orders.rs:118:9:",
            "assertion `left == right` failed",
            "  left: Refunded",
            " right: Cancelled",
            "##[warning]1 test was ignored",
            "test result: FAILED. 41 passed; 1 failed; 0 ignored",
            "##[error]Process completed with exit code 101.",
        ];
        LogView {
            title: "api › CI › test › 3. Run tests".into(),
            job_id: 7,
            step: None,
            whole: false,
            lines: lines.iter().map(|s| s.to_string()).collect(),
            note: None,
            scroll: None,
        }
    }

    #[test]
    fn log_lines_keep_one_line_per_input_and_show_markers() {
        let theme = Theme::builtin("noite").unwrap();
        let view = sample_log();
        let lines = log_lines(&view.lines, &theme);
        assert_eq!(lines.len(), view.lines.len());
        let text = |l: &Line| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        assert_eq!(text(&lines[0]), " 1 │ ▾ Run cargo test --locked");
        assert_eq!(text(&lines[1]), " 2 │   $ cargo test --locked");
        assert_eq!(text(&lines[9]), "10 │ ");
        assert_eq!(
            text(&lines[18]),
            "19 │ ✗ Process completed with exit code 101."
        );
        assert_eq!(lines[18].spans[0].style.fg, Some(theme.failure));
    }

    fn demo_app(theme: &str) -> App {
        let mut app = App {
            snapshot: Some(demo()),
            rows: Vec::new(),
            repo_rows: Vec::new(),
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
            log_wanted: Vec::new(),
            log_asked: Default::default(),
            theme: Theme::builtin(theme).unwrap(),
            bar: BarStyle::Line,
            started: Instant::now(),
            details: false,
            list_area: Rect::default(),
            open_steps_when_loaded: HashSet::new(),
            focus: None,
        };
        app.rebuild();
        app.list.select(Some(3));
        app
    }

    #[test]
    fn tab_wraps_around_top_level_rows() {
        let mut app = demo_app("noite");
        let tops: Vec<usize> = (0..app.rows.len())
            .filter(|&i| app.rows[i].depth == 0)
            .collect();
        assert!(tops.len() >= 2);
        let (first, last) = (tops[0], *tops.last().unwrap());
        app.list.select(Some(last));
        app.jump_repo(true);
        assert_eq!(app.list.selected(), Some(first));
        app.jump_repo(false);
        assert_eq!(app.list.selected(), Some(last));
        // From inside the last repo, Tab also goes back to the first.
        app.list.select(Some(app.rows.len() - 1));
        app.jump_repo(true);
        assert_eq!(app.list.selected(), Some(first));
    }

    #[test]
    fn filter_shows_only_matching_rows() {
        let mut app = demo_app("noite");
        let all = app.rows.len();
        app.filter_text = "Release".into();
        app.rebuild();
        // engine sits in the folded calm line; the search still finds it.
        let keys: Vec<&RowKey> = app.rows.iter().map(|r| &r.key).collect();
        assert!(keys.contains(&&RowKey::Workflow(6)), "{keys:?}");
        assert!(keys.contains(&&RowKey::Repo("o/brabo-engine".into())));
        assert!(!keys.contains(&&RowKey::Repo("o/hud".into())));
        assert!(app.rows.len() < all);

        app.filter_text = "zzz".into();
        app.rebuild();
        assert_eq!(app.rows.len(), 1);
        assert!(app.rows[0].text.contains("nothing matches"));

        app.filter_text.clear();
        app.rebuild();
        assert_eq!(app.rows.len(), all);
    }

    fn select_key(app: &mut App, key: RowKey) {
        let i = app.rows.iter().position(|r| r.key == key).unwrap();
        app.list.select(Some(i));
    }

    #[test]
    fn a_closes_everything_then_opens_everything() {
        let mut app = demo_app("noite");
        assert!(app.rows.iter().any(|r| r.depth > 0));
        assert!(!app.toggle_all());
        assert!(app.rows.iter().all(|r| r.depth == 0), "everything closed");
        assert!(!app.expand.all);

        assert!(app.toggle_all(), "opening fetches the quiet workflows");
        assert!(app.expand.all);
        // The calm repos are open too: engine's Release shows.
        assert!(app.rows.iter().any(|r| r.key == RowKey::Workflow(6)));
        assert!(app.rows.iter().any(|r| r.key == RowKey::Job(2, 1)));
    }

    #[test]
    fn shift_a_toggles_a_row_and_everything_under_it() {
        let mut app = demo_app("noite");

        // A job: opens every step that has a log, then closes them.
        select_key(&mut app, RowKey::Job(3, 0));
        assert!(!app.toggle_subtree());
        assert!(app.folds.open_steps.contains(&(3, 0, 1)));
        assert!(app.folds.open_steps.contains(&(3, 0, 2)));
        assert!(
            !app.folds.open_steps.contains(&(3, 0, 3)),
            "skipped step has no log"
        );
        assert!(matches!(
            app.selected().map(|r| &r.key),
            Some(RowKey::Job(3, 0))
        ));
        app.toggle_subtree();
        assert!(!app.folds.open_steps.iter().any(|s| s.0 == 3));

        // A workflow with jobs: closes, then opens with all its steps.
        select_key(&mut app, RowKey::Workflow(2));
        app.toggle_subtree();
        assert!(!app.rows.iter().any(|r| r.key == RowKey::Job(2, 0)));
        app.toggle_subtree();
        assert!(app.rows.iter().any(|r| r.key == RowKey::Job(2, 1)));
        assert!(app.folds.open_steps.contains(&(2, 1, 2)));
        assert!(!app.folds.open_steps.contains(&(2, 1, 3)), "queued step");

        // The group: closes, then opens every repo, fetching quiet workflows
        // and opening their steps once the jobs arrive.
        select_key(&mut app, RowKey::Group("brabo".into()));
        assert!(!app.toggle_subtree());
        let g = app.list.selected().unwrap();
        assert!(
            app.rows.get(g + 1).is_none_or(|r| r.depth == 0),
            "group folded"
        );
        assert!(app.toggle_subtree());
        assert!(app.expand.pinned.contains(&6));
        assert!(app.rows.iter().any(|r| r.key == RowKey::Workflow(6)));
        let snap = app.snapshot.as_mut().unwrap();
        let engine = snap
            .repos
            .iter_mut()
            .find(|r| r.name == "brabo-engine")
            .unwrap();
        let release = engine
            .workflows
            .as_mut()
            .unwrap()
            .iter_mut()
            .find(|w| w.run_id == 6)
            .unwrap();
        release.jobs = Some(Ok(vec![crate::snapshot::JobView {
            id: 60,
            name: "publish".into(),
            status: Status::Success,
            url: None,
            steps: vec![crate::snapshot::StepView {
                number: 1,
                name: "Publish".into(),
                status: Status::Success,
                started_at: None,
                completed_at: None,
            }],
        }]));
        app.open_loaded_steps();
        assert!(app.folds.open_steps.contains(&(6, 0, 1)));
        assert!(!app.open_steps_when_loaded.contains(&6));
    }

    #[test]
    fn f_focuses_the_top_level_row_and_back() {
        let mut app = demo_app("noite");
        let all = app.rows.len();
        // From a step deep inside brabo-api, f focuses the whole brabo group.
        select_key(&mut app, RowKey::Job(3, 0));
        app.toggle_focus();
        assert_eq!(
            app.focus.as_ref().map(|f| &f.0),
            Some(&RowKey::Group("brabo".into()))
        );
        assert_eq!(app.rows[0].key, RowKey::Group("brabo".into()));
        assert_eq!(app.rows.iter().filter(|r| r.depth == 0).count(), 1);
        assert!(
            !app.rows
                .iter()
                .any(|r| r.key == RowKey::Repo("o/hud".into()))
        );
        assert!(matches!(
            app.selected().map(|r| &r.key),
            Some(RowKey::Job(3, 0))
        ));

        // Search works inside the focus: hud's CI is outside it.
        app.filter_text = "ci".into();
        app.rebuild();
        assert!(app.rows.iter().any(|r| r.key == RowKey::Workflow(3)));
        assert!(!app.rows.iter().any(|r| r.key == RowKey::Workflow(9)));
        app.filter_text.clear();

        app.toggle_focus();
        assert!(app.focus.is_none());
        assert_eq!(app.rows.len(), all);

        // A repo outside any group focuses alone.
        select_key(&mut app, RowKey::Repo("o/hud".into()));
        app.toggle_focus();
        assert_eq!(app.rows[0].key, RowKey::Repo("o/hud".into()));
        assert!(app.rows.iter().skip(1).all(|r| r.depth > 0));
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
            // POPIER_DETAILS=<row> opens the sidebar on that row.
            if let Some(row) = std::env::var("POPIER_DETAILS")
                .ok()
                .and_then(|v| v.parse().ok())
            {
                app.details = true;
                app.list.select(Some(row));
            }
            // POPIER_STEPLOG=1 opens the failed step's log in the tree.
            if let Ok(v) = std::env::var("POPIER_STEPLOG") {
                open_failed_step(&mut app);
                app.details = v == "panel";
            }
            // POPIER_LOG=1 opens the log panel on a sample failing step.
            if std::env::var("POPIER_LOG").is_ok() {
                app.log_view = Some(sample_log());
            }
            // POPIER_SIZE=<w>x<h> renders another terminal size (e.g. 90x60).
            let (w, h) = std::env::var("POPIER_SIZE")
                .ok()
                .and_then(|s| {
                    let (w, h) = s.split_once('x')?;
                    Some((w.parse().ok()?, h.parse().ok()?))
                })
                .unwrap_or((140, 26));
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

    #[test]
    fn click_maps_to_row_under_the_mouse() {
        use crossterm::event::KeyModifiers;
        let area = Rect::new(0, 3, 80, 10);
        let at = |column, row| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(clicked_row(area, 0, at(5, 3)), Some(0));
        assert_eq!(clicked_row(area, 4, at(5, 5)), Some(6));
        assert_eq!(clicked_row(area, 0, at(5, 2)), None);
        assert_eq!(clicked_row(area, 0, at(80, 5)), None);
    }

    #[test]
    fn clicking_a_repo_folds_it_and_the_panel_draws() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut app = demo_app("noite");
        app.list_area = Rect::new(0, 3, 120, 18);
        let repo = app
            .rows
            .iter()
            .position(|r| matches!(r.key, RowKey::Repo(_)))
            .unwrap();
        let before = app.rows.len();
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 4,
            row: 3 + repo as u16,
            modifiers: KeyModifiers::NONE,
        };
        on_mouse(&mut app, click);
        assert!(app.rows.len() < before, "the repo folded");

        let client = Client::new("test".into()).unwrap();
        for width in [60, 100, 160] {
            for i in 0..app.rows.len() {
                app.details = true;
                app.list.select(Some(i));
                let mut term = Terminal::new(TestBackend::new(width, 24)).unwrap();
                term.draw(|f| draw(f, &mut app, &client)).unwrap();
                let buf = term.backend().buffer();
                let text: String = (0..24)
                    .flat_map(|y| (0..width).map(move |x| (x, y)))
                    .map(|(x, y)| buf[(x, y)].symbol().to_owned())
                    .collect();
                assert!(text.contains("details"), "panel at width {width}, row {i}");
            }
        }
    }

    /// Opens the first failed step of the demo with a sample log in the cache.
    fn open_failed_step(app: &mut App) -> usize {
        let step = app
            .rows
            .iter()
            .position(|r| matches!(r.key, RowKey::Step(..)) && r.status == Some(Status::Failure))
            .expect("demo has a failed step");
        let RowKey::Step(run, job, _) = app.rows[step].key else {
            unreachable!()
        };
        let (_, job_id, _) = app.job_ref(run, job).unwrap();
        let log: String = sample_log().lines.join("\n");
        app.log_cache.insert(job_id, Ok(log));
        app.list.select(Some(step));
        app.toggle();
        step
    }

    #[test]
    fn opening_a_step_shows_its_log_below_and_closing_hides_it() {
        let mut app = demo_app("noite");
        let before = app.rows.len();
        let step = open_failed_step(&mut app);
        assert!(app.rows[step].expanded);
        let logs: Vec<&Row> = app
            .rows
            .iter()
            .skip(step + 1)
            .take_while(|r| matches!(r.key, RowKey::Log(..)))
            .collect();
        assert_eq!(logs.len(), TREE_LOG_LINES + 1, "tail plus the hint line");
        assert!(logs.last().unwrap().detail.is_some());
        assert!(logs.iter().all(|r| r.depth == app.rows[step].depth + 1));

        let theme = Theme::builtin("noite").unwrap();
        let look = Look {
            theme: &theme,
            bar: BarStyle::Line,
            tick: 0,
        };
        let lines = paint::rows(&app.rows, &look, 120);
        let text = |l: &Line| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        assert!(text(&lines[step]).contains("▾ "), "{}", text(&lines[step]));
        assert!(text(&lines[step + TREE_LOG_LINES]).contains("✗ Process completed"));
        assert!(text(&lines[step + TREE_LOG_LINES + 1]).contains("l full log"));

        // Enter on a log line closes its step and keeps the step selected.
        app.list.select(Some(step + 3));
        app.toggle();
        assert_eq!(app.rows.len(), before);
        assert_eq!(app.list.selected(), Some(step));
        assert!(!app.rows[step].expanded);
    }

    #[test]
    fn an_open_step_without_its_log_asks_for_it_once() {
        let mut app = demo_app("noite");
        let step = app
            .rows
            .iter()
            .position(|r| matches!(r.key, RowKey::Step(..)) && r.foldable)
            .unwrap();
        app.list.select(Some(step));
        app.toggle();
        assert_eq!(app.log_wanted.len(), 1);
        assert_eq!(app.rows[step + 1].text, "loading log…");
        app.rebuild();
        assert_eq!(app.log_wanted.len(), 1, "not asked twice");
    }

    #[test]
    fn the_details_panel_shows_the_step_log() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut app = demo_app("noite");
        open_failed_step(&mut app);
        app.details = true;
        let client = Client::new("test".into()).unwrap();
        let mut term = Terminal::new(TestBackend::new(140, 40)).unwrap();
        term.draw(|f| draw(f, &mut app, &client)).unwrap();
        let buf = term.backend().buffer();
        let text: String = (0..40)
            .flat_map(|y| (0..140).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].symbol().to_owned())
            .collect();
        assert!(
            text.contains("LOG (LAST 10)"),
            "panel shows the log section"
        );
    }
}
