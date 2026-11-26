//! Flattens a snapshot into display rows, shared by the TUI and the plain printer.

use std::collections::HashSet;

use crate::snapshot::{JobView, RepoView, Snapshot, WorkflowView};
use crate::status::Status;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowKey {
    /// A `[[group]]` of repositories, by name.
    Group(String),
    /// The `✓ N repos ok` line of a group's calm repos, by group name.
    Calm(String),
    Repo(String),
    Workflow(u64),
    Job(u64, usize),
    Step(u64, usize, u32),
    /// A line of an open step's log: run, job, step and line index.
    Log(u64, usize, u32, usize),
    Note(String, usize),
}

#[derive(Debug, Clone)]
pub struct Row {
    pub key: RowKey,
    pub depth: u8,
    pub status: Option<Status>,
    pub text: String,
    /// Secondary text, rendered dimmed (branch, step number, hints).
    pub detail: Option<String>,
    pub is_error: bool,
    pub url: Option<String>,
    /// Whether this row can be expanded or collapsed.
    pub foldable: bool,
    pub expanded: bool,
    /// Progress and timing for the dashboard (repo, workflow, job and step rows).
    pub gauge: Option<Gauge>,
    /// Branch of a workflow row (or of a repo row's lead pipeline), shortened.
    pub branch: Option<String>,
    /// Repo rows: the pipeline the row summarizes (`CI`, `3 pipelines`).
    pub lead: Option<String>,
    /// Group rows: repos per state and the reasons for attention.
    pub health: Option<Health>,
}

/// Progress and timing shown to the right of a row.
#[derive(Debug, Clone, PartialEq)]
pub struct Gauge {
    pub status: Status,
    /// Steps finished up to the first failure or cancellation.
    pub done: u32,
    /// Steps in total; 0 when unknown (jobs not fetched).
    pub total: u32,
    pub elapsed: Option<Elapsed>,
    /// Short note after the bar: failing `job › N. step`, running step, key hint.
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elapsed {
    /// How long it ran (or has been running), in seconds.
    Took(u64),
    /// How long ago it finished, in seconds.
    Ago(u64),
}

impl Gauge {
    /// Bar fill in 0..=1, or `None` when the bar should not be drawn.
    pub fn fraction(&self) -> Option<f64> {
        if self.total > 0 {
            return Some(f64::from(self.done) / f64::from(self.total));
        }
        match self.status {
            Status::Running => None,
            Status::Queued => Some(0.0),
            _ => Some(1.0),
        }
    }
}

/// What the user folded by hand.
#[derive(Debug, Clone, Default)]
pub struct Folds {
    pub collapsed_groups: HashSet<String>,
    /// Groups whose calm-repos line the user opened.
    pub open_calm: HashSet<String>,
    pub collapsed_repos: HashSet<String>,
    pub collapsed_workflows: HashSet<u64>,
    /// Steps whose log the user opened in the tree: run, job, step.
    pub open_steps: HashSet<(u64, usize, u32)>,
}

pub fn build(snapshot: &Snapshot, folds: &Folds) -> Vec<Row> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    build_at(snapshot, folds, now)
}

/// Like [`build`], with `now` in Unix seconds for elapsed times.
pub fn build_at(snapshot: &Snapshot, folds: &Folds, now: i64) -> Vec<Row> {
    let mut rows = Vec::new();
    let repos = &snapshot.repos;
    let mut i = 0;
    while i < repos.len() {
        let Some(group) = repos[i].group.as_deref() else {
            push_repo(&mut rows, &repos[i], &repos[i].name, folds, now);
            i += 1;
            continue;
        };
        let end = i + repos[i..]
            .iter()
            .take_while(|r| r.group.as_deref() == Some(group))
            .count();
        push_group(&mut rows, group, &repos[i..end], folds, now);
        i = end;
    }
    rows
}

/// Every repo row with its group, also inside folded groups and the calm
/// line, for the header's totals and pulse strip.
pub fn repo_rows(snapshot: &Snapshot) -> Vec<(Option<String>, Row)> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let folds = Folds {
        collapsed_repos: snapshot.repos.iter().map(|r| r.slug.clone()).collect(),
        ..Folds::default()
    };
    snapshot
        .repos
        .iter()
        .map(|repo| {
            let mut rows = Vec::new();
            push_repo(&mut rows, repo, &repo.name, &folds, now);
            (repo.group.clone(), rows.swap_remove(0))
        })
        .collect()
}

/// Lead pipeline name, its branch, and the gauge a summary row shows.
type Summary = (String, Option<String>, Gauge);

/// How many repos of a group are in each state, and why it needs attention.
#[derive(Debug, Clone, PartialEq)]
pub struct Health {
    /// Repos per state, in [`HEALTH_ORDER`], states with no repo left out.
    pub counts: Vec<(Status, usize)>,
    /// Up to two `repo › pipeline` notes for repos that need attention,
    /// with the progress of a running one.
    pub reasons: Vec<(Status, String)>,
}

/// Order of the segments of a group's health bar: worst first.
pub const HEALTH_ORDER: [Status; 6] = [
    Status::Failure,
    Status::Running,
    Status::Queued,
    Status::Success,
    Status::Cancelled,
    Status::Skipped,
];

/// Most urgent first, for the order of repos inside a group: failed,
/// running, queued, cancelled, then the calm ones.
fn group_rank(s: Option<Status>) -> u8 {
    match s {
        Some(Status::Failure) => 0,
        Some(Status::Running) => 1,
        Some(Status::Queued) => 2,
        Some(Status::Cancelled) => 3,
        None => 4,
        Some(Status::Success | Status::Skipped) => 5,
    }
}

/// Fewest calm repos that fold into one `✓ N repos ok` line.
const CALM_MIN: usize = 3;

/// The prefix every member shares (`brabo` for `brabo`, `brabo-api`, ...),
/// with the separator that follows it, or `None` when they don't share one.
fn shared_prefix(names: &[&str]) -> Option<String> {
    let first = names.first()?;
    let p = first.split(['-', '_', '.']).next()?;
    if p.is_empty() || p.len() == first.len() && names.len() == 1 {
        return None;
    }
    let shares = |n: &str| {
        n == p
            || n.strip_prefix(p)
                .and_then(|rest| rest.chars().next())
                .is_some_and(|c| matches!(c, '-' | '_' | '.'))
    };
    names.iter().all(|n| shares(n)).then(|| p.to_owned())
}

/// `brabo-api` → `api` when the group shares `brabo`; the repo named like
/// the prefix keeps its name.
fn short_name(name: &str, prefix: Option<&str>) -> String {
    prefix
        .and_then(|p| name.strip_prefix(p))
        .and_then(|rest| rest.get(1..))
        .filter(|rest| !rest.is_empty())
        .unwrap_or(name)
        .to_owned()
}

/// The group row and, when open, its repos: those needing attention first,
/// then the calm ones, folded into one line when there are enough of them.
fn push_group(rows: &mut Vec<Row>, name: &str, members: &[RepoView], folds: &Folds, now: i64) {
    let summaries: Vec<Option<Summary>> = members
        .iter()
        .map(|m| m.workflows.as_ref().ok().and_then(|w| repo_summary(w, now)))
        .collect();
    let status_of = |i: usize| summaries[i].as_ref().map(|(_, _, g)| g.status);
    let names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
    let prefix = shared_prefix(&names);
    let short = |i: usize| short_name(&members[i].name, prefix.as_deref());

    let counts: Vec<(Status, usize)> = HEALTH_ORDER
        .iter()
        .map(|&s| {
            (
                s,
                (0..members.len())
                    .filter(|&i| status_of(i) == Some(s))
                    .count(),
            )
        })
        .filter(|&(_, n)| n > 0)
        .collect();
    let mut order: Vec<usize> = (0..members.len()).collect();
    order.sort_by_key(|&i| group_rank(status_of(i)));
    let reasons: Vec<(Status, String)> = order
        .iter()
        .filter_map(|&i| {
            let (lead, _, gauge) = summaries[i].as_ref()?;
            if attention_rank(gauge.status) >= 3 {
                return None;
            }
            let mut text = format!("{} › {lead}", short(i));
            if gauge.status == Status::Running
                && let Some(f) = gauge.fraction()
            {
                text.push_str(&format!(" {}%", (f * 100.0).floor() as u32));
            }
            Some((gauge.status, text))
        })
        .take(2)
        .collect();
    let worst = order.first().and_then(|&i| status_of(i));
    let open = !folds.collapsed_groups.contains(name);
    let repos_word = |n: usize| format!("{n} repo{}", if n == 1 { "" } else { "s" });
    let mut detail = repos_word(members.len());
    for (s, n) in &counts {
        detail.push_str(&format!("  {} {n}", s.icon()));
    }
    rows.push(Row {
        key: RowKey::Group(name.to_owned()),
        depth: 0,
        status: worst,
        text: name.to_owned(),
        detail: Some(detail),
        is_error: false,
        url: None,
        foldable: true,
        expanded: open,
        gauge: None,
        branch: None,
        lead: Some(repos_word(members.len())),
        health: Some(Health { counts, reasons }),
    });
    if !open {
        return;
    }

    let start = rows.len();
    let calm: Vec<usize> = order
        .iter()
        .copied()
        .filter(|&i| group_rank(status_of(i)) == 5)
        .collect();
    let fold_calm = calm.len() >= CALM_MIN;
    for &i in &order {
        if !(fold_calm && calm.contains(&i)) {
            push_repo(rows, &members[i], &short(i), folds, now);
        }
    }
    if fold_calm {
        let calm_open = folds.open_calm.contains(name);
        let ago = calm
            .iter()
            .filter_map(|&i| match summaries[i].as_ref()?.2.elapsed {
                Some(Elapsed::Ago(s)) => Some(s),
                _ => None,
            })
            .min()
            .map(Elapsed::Ago);
        let mut lead: Vec<String> = calm.iter().take(3).map(|&i| short(i)).collect();
        if calm.len() > 3 {
            lead.push(format!("+{}", calm.len() - 3));
        }
        rows.push(Row {
            key: RowKey::Calm(name.to_owned()),
            depth: 0,
            status: Some(Status::Success),
            text: format!("{} ok", repos_word(calm.len())),
            detail: None,
            is_error: false,
            url: None,
            foldable: true,
            expanded: calm_open,
            gauge: Some(Gauge {
                status: Status::Success,
                done: 0,
                total: 0,
                elapsed: ago,
                hint: None,
            }),
            branch: None,
            lead: Some(lead.join(" · ")),
            health: None,
        });
        if calm_open {
            let inner = rows.len();
            for &i in &calm {
                push_repo(rows, &members[i], &short(i), folds, now);
            }
            rows[inner..].iter_mut().for_each(|r| r.depth += 1);
        }
    }
    rows[start..].iter_mut().for_each(|r| r.depth += 1);
}

/// Failed first, then running, then queued, then the rest.
fn attention_rank(s: Status) -> u8 {
    match s {
        Status::Failure => 0,
        Status::Running => 1,
        Status::Queued => 2,
        _ => 3,
    }
}

/// The repo row and, when open, its workflows, jobs and steps.
fn push_repo(rows: &mut Vec<Row>, repo: &RepoView, name: &str, folds: &Folds, now: i64) {
    let repo_open = !folds.collapsed_repos.contains(&repo.slug);
    let repo_url = format!("https://github.com/{}/actions", repo.slug);
    let summary = repo
        .workflows
        .as_ref()
        .ok()
        .and_then(|w| repo_summary(w, now));
    let (lead, branch, gauge) = match summary {
        Some((lead, branch, gauge)) => (Some(lead), branch, Some(gauge)),
        None => (None, None, None),
    };
    rows.push(Row {
        key: RowKey::Repo(repo.slug.clone()),
        depth: 0,
        status: gauge.as_ref().map(|g| g.status),
        text: name.to_owned(),
        detail: None,
        is_error: false,
        url: Some(repo_url),
        foldable: true,
        expanded: repo_open,
        gauge,
        branch,
        lead,
        health: None,
    });
    if !repo_open {
        return;
    }

    let workflows = match &repo.workflows {
        Err(err) => {
            rows.push(note(
                &repo.slug,
                0,
                format!("error: {}", first_line(err)),
                true,
            ));
            return;
        }
        Ok(w) if w.is_empty() => {
            rows.push(note(&repo.slug, 0, "(no runs)".into(), false));
            return;
        }
        Ok(w) => w,
    };

    for wf in workflows {
        let open = wf.jobs.is_some() && !folds.collapsed_workflows.contains(&wf.run_id);
        rows.push(Row {
            key: RowKey::Workflow(wf.run_id),
            depth: 1,
            status: Some(wf.status),
            text: wf.name.clone(),
            detail: branch_detail(&wf.name, wf.branch.as_deref()),
            is_error: false,
            url: Some(wf.url.clone()),
            foldable: true,
            expanded: open,
            gauge: Some(workflow_gauge(wf, now)),
            branch: wf.branch.as_deref().map(|b| shorten_middle(b, MAX_BRANCH)),
            lead: None,
            health: None,
        });
        if !open {
            continue;
        }
        match wf.jobs.as_ref().expect("open implies jobs") {
            Err(err) => rows.push(Row {
                depth: 2,
                ..note(&repo.slug, 1, format!("error: {}", first_line(err)), true)
            }),
            Ok(jobs) => {
                for (j, job) in jobs.iter().enumerate() {
                    rows.push(Row {
                        key: RowKey::Job(wf.run_id, j),
                        depth: 2,
                        status: Some(job.status),
                        text: job.name.clone(),
                        detail: None,
                        is_error: false,
                        url: job.url.clone(),
                        foldable: false,
                        expanded: true,
                        gauge: Some(job_gauge(job, now)),
                        branch: None,
                        lead: None,
                        health: None,
                    });
                    for step in &job.steps {
                        rows.push(Row {
                            key: RowKey::Step(wf.run_id, j, step.number),
                            depth: 3,
                            status: Some(step.status),
                            text: step.name.clone(),
                            detail: Some(format!("{}.", step.number)),
                            is_error: false,
                            url: job.url.clone(),
                            // Opening a step shows the end of its log below it.
                            foldable: !matches!(step.status, Status::Queued | Status::Skipped),
                            expanded: folds.open_steps.contains(&(wf.run_id, j, step.number)),
                            gauge: Some(Gauge {
                                status: step.status,
                                done: 0,
                                total: 0,
                                elapsed: span(
                                    step.started_at.as_deref(),
                                    step.completed_at.as_deref(),
                                    step.status,
                                    now,
                                ),
                                hint: None,
                            }),
                            branch: None,
                            lead: None,
                            health: None,
                        });
                    }
                }
            }
        }
    }
}

/// Keeps the rows whose name or branch contains `query` (lowercase), with
/// everything under them and the rows above them, so a match is shown in place.
pub fn search(rows: Vec<Row>, query: &str) -> Vec<Row> {
    let matches = |row: &Row| {
        row.text.to_lowercase().contains(query)
            || row
                .branch
                .as_deref()
                .is_some_and(|b| b.to_lowercase().contains(query))
    };
    let mut keep = vec![false; rows.len()];
    // Down: a match keeps its whole subtree.
    let mut inside: Option<u8> = None;
    for (i, row) in rows.iter().enumerate() {
        if inside.is_some_and(|d| row.depth <= d) {
            inside = None;
        }
        if inside.is_some() {
            keep[i] = true;
        } else if matches(row) {
            keep[i] = true;
            inside = Some(row.depth);
        }
    }
    // Up: the nearest shallower row above a kept row is its parent.
    // `need`: depth of the last kept row seen; 0 means none (nothing is above depth 0).
    let mut need = 0;
    for (i, row) in rows.iter().enumerate().rev() {
        if row.depth < need {
            keep[i] = true;
        }
        if keep[i] {
            need = row.depth;
        }
    }
    rows.into_iter()
        .zip(keep)
        .filter_map(|(row, keep)| keep.then_some(row))
        .collect()
}

/// Folds for a search: every group, calm line, repo and loaded workflow open,
/// so matches deep in the tree show; open step logs stay as they were.
pub fn search_folds(snapshot: &Snapshot, folds: &Folds) -> Folds {
    Folds {
        open_calm: snapshot
            .repos
            .iter()
            .filter_map(|r| r.group.clone())
            .collect(),
        open_steps: folds.open_steps.clone(),
        ..Folds::default()
    }
}

pub fn note(slug: &str, idx: usize, text: String, is_error: bool) -> Row {
    Row {
        key: RowKey::Note(slug.to_owned(), idx),
        depth: 1,
        status: None,
        text,
        detail: None,
        is_error,
        url: None,
        foldable: false,
        expanded: false,
        gauge: None,
        branch: None,
        lead: None,
        health: None,
    }
}

/// Steps finished, counting up to the first failure or cancellation, and the total.
fn step_progress(job: &JobView) -> (u32, u32) {
    let mut done = 0;
    for step in &job.steps {
        match step.status {
            Status::Running | Status::Queued => break,
            Status::Failure | Status::Cancelled => {
                done += 1;
                break;
            }
            _ => done += 1,
        }
    }
    (done, job.steps.len() as u32)
}

fn job_gauge(job: &JobView, now: i64) -> Gauge {
    let (done, total) = step_progress(job);
    let started = job.steps.iter().find_map(|s| s.started_at.as_deref());
    let completed = job
        .steps
        .iter()
        .rev()
        .find_map(|s| s.completed_at.as_deref());
    let hint = match job.status {
        Status::Running => job
            .steps
            .iter()
            .find(|s| s.status == Status::Running)
            .map(|s| format!("{}. {}", s.number, s.name)),
        Status::Failure => Some("l log".into()),
        _ => None,
    };
    Gauge {
        status: job.status,
        done,
        total,
        elapsed: span(started, completed, job.status, now),
        hint,
    }
}

fn workflow_gauge(wf: &WorkflowView, now: i64) -> Gauge {
    let jobs = wf.jobs.as_ref().and_then(|j| j.as_ref().ok());
    let (done, total) = jobs.map_or((0, 0), |jobs| {
        jobs.iter()
            .map(step_progress)
            .fold((0, 0), |(d, t), (jd, jt)| (d + jd, t + jt))
    });
    let hint = match wf.status {
        Status::Failure => jobs.and_then(|jobs| {
            let job = jobs.iter().find(|j| j.status == Status::Failure)?;
            Some(
                match job.steps.iter().find(|s| s.status == Status::Failure) {
                    Some(step) => format!("{} › {}. {}", job.name, step.number, step.name),
                    None => job.name.clone(),
                },
            )
        }),
        Status::Queued => Some("waiting".into()),
        _ => None,
    };
    Gauge {
        status: wf.status,
        done,
        total,
        elapsed: span(
            wf.started_at.as_deref(),
            wf.updated_at.as_deref(),
            wf.status,
            now,
        ),
        hint,
    }
}

/// The pipeline a repo row stands for: failed, else running, else queued,
/// else the most recently updated one.
fn repo_summary(workflows: &[WorkflowView], now: i64) -> Option<Summary> {
    let rank = attention_rank;
    let lead = workflows.iter().min_by(|a, b| {
        rank(a.status)
            .cmp(&rank(b.status))
            .then_with(|| b.updated_at.cmp(&a.updated_at))
    })?;
    let branch = lead
        .branch
        .as_deref()
        .map(|b| shorten_middle(b, MAX_BRANCH));
    if rank(lead.status) < 3 {
        return Some((lead.name.clone(), branch, workflow_gauge(lead, now)));
    }
    // Nothing needs attention: say how many pipelines and when the last one ended.
    let ago = workflows
        .iter()
        .filter_map(|w| w.updated_at.as_deref().and_then(parse_time))
        .max()
        .map(|t| Elapsed::Ago((now - t).max(0) as u64));
    let (lead_name, branch) = if workflows.len() == 1 {
        (lead.name.clone(), branch)
    } else {
        (format!("{} pipelines", workflows.len()), None)
    };
    let gauge = Gauge {
        status: lead.status,
        done: 0,
        total: 0,
        elapsed: ago,
        hint: None,
    };
    Some((lead_name, branch, gauge))
}

/// Elapsed time from `start` to `end`, or to `now` while still going.
fn span(start: Option<&str>, end: Option<&str>, status: Status, now: i64) -> Option<Elapsed> {
    let start = parse_time(start?)?;
    let end = match status {
        Status::Running | Status::Queued => now,
        _ => end.and_then(parse_time)?,
    };
    Some(Elapsed::Took((end - start).max(0) as u64))
}

/// Parses an RFC 3339 timestamp (`2026-11-26T02:38:00Z`, fractions and
/// offsets allowed) into Unix seconds.
pub fn parse_time(s: &str) -> Option<i64> {
    let (date, rest) = s.split_once('T')?;
    let mut d = date.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let num = |a: usize, b: usize| rest.get(a..b)?.parse::<i64>().ok();
    let (hh, mm, ss) = (num(0, 2)?, num(3, 5)?, num(6, 8)?);
    let mut tz = &rest[8..];
    if let Some(frac) = tz.strip_prefix('.') {
        tz = frac.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let offset = match tz {
        "Z" | "z" | "" => 0,
        _ => {
            let sign = if tz.starts_with('-') { -1 } else { 1 };
            let h = tz.get(1..3)?.parse::<i64>().ok()?;
            let m = tz.get(4..6)?.parse::<i64>().ok()?;
            sign * (h * 3600 + m * 60)
        }
    };
    // Days from civil (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss - offset)
}

/// Longest branch name shown in full; longer ones are cut in the middle.
const MAX_BRANCH: usize = 40;

/// `(branch)` suffix for a workflow row, skipped when the label already ends with it.
fn branch_detail(label: &str, branch: Option<&str>) -> Option<String> {
    let branch = branch.unwrap_or("-");
    if label.ends_with(&format!("({branch})")) {
        return None;
    }
    Some(format!("({})", shorten_middle(branch, MAX_BRANCH)))
}

/// Cuts `s` to at most `max` characters, keeping its start and end around `…`.
fn shorten_middle(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_owned();
    }
    let keep = max - 1;
    let head = keep / 2;
    let tail = keep - head;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(&chars[chars.len() - tail..]);
    out
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s)
}

/// Plain-text rendering with ANSI colors, like the original bash prototype.
pub fn render_plain(rows: &[Row], color: bool) -> String {
    let paint = |code: &str, s: &str| {
        if color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_owned()
        }
    };
    let mut out = String::new();
    for (i, row) in rows.iter().enumerate() {
        if row.depth == 0 && i > 0 {
            out.push('\n');
        }
        out.push_str(&"    ".repeat(row.depth as usize));
        if let Some(status) = row.status.filter(|_| row.depth > 0) {
            out.push_str(&paint(status.ansi(), status.icon()));
            out.push(' ');
        }
        let detail_first = matches!(row.key, RowKey::Step(..));
        if detail_first && let Some(d) = &row.detail {
            out.push_str(&paint("90", d));
            out.push(' ');
        }
        let heading = matches!(row.key, RowKey::Group(_) | RowKey::Repo(_));
        match (heading, row.is_error) {
            (true, _) => out.push_str(&paint("1", &row.text)),
            (_, true) => out.push_str(&paint("31", &row.text)),
            _ => out.push_str(&row.text),
        }
        if !detail_first && let Some(d) = &row.detail {
            out.push(' ');
            out.push_str(&paint("90", d));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{RepoView, StepView};

    fn sample() -> Snapshot {
        Snapshot {
            repos: vec![
                RepoView {
                    name: "api".into(),
                    slug: "o/api".into(),
                    group: None,
                    workflows: Ok(vec![
                        WorkflowView {
                            workflow_id: 10,
                            run_id: 1,
                            name: "CI".into(),
                            branch: Some("main".into()),
                            status: Status::Failure,
                            url: String::new(),
                            started_at: None,
                            updated_at: None,
                            jobs: Some(Ok(vec![JobView {
                                id: 100,
                                name: "test".into(),
                                status: Status::Failure,
                                url: None,
                                steps: vec![StepView {
                                    number: 1,
                                    name: "Run tests".into(),
                                    status: Status::Failure,
                                    started_at: None,
                                    completed_at: None,
                                }],
                            }])),
                        },
                        WorkflowView {
                            workflow_id: 20,
                            run_id: 2,
                            name: "Lint".into(),
                            branch: None,
                            status: Status::Success,
                            url: String::new(),
                            started_at: None,
                            updated_at: None,
                            jobs: None,
                        },
                    ]),
                },
                RepoView {
                    name: "web".into(),
                    slug: "o/web".into(),
                    group: None,
                    workflows: Err("GitHub API 404: Not Found".into()),
                },
            ],
        }
    }

    #[test]
    fn builds_full_hierarchy() {
        let rows = build(&sample(), &Folds::default());
        let depths: Vec<u8> = rows.iter().map(|r| r.depth).collect();
        // api, CI, test, step, Lint, web, error
        assert_eq!(depths, vec![0, 1, 2, 3, 1, 0, 1]);
        assert!(rows[6].is_error);
    }

    #[test]
    fn groups_nest_their_repos() {
        let mut snap = sample();
        for repo in &mut snap.repos {
            repo.group = Some("proj".into());
        }
        let rows = build(&snap, &Folds::default());
        assert_eq!(rows[0].key, RowKey::Group("proj".into()));
        // Worst state of the group is the failing CI of api.
        assert_eq!(rows[0].status, Some(Status::Failure));
        let health = rows[0].health.as_ref().unwrap();
        assert_eq!(health.counts, vec![(Status::Failure, 1)]);
        assert_eq!(health.reasons, vec![(Status::Failure, "api › CI".into())]);
        let depths: Vec<u8> = rows.iter().map(|r| r.depth).collect();
        // proj, api, CI, test, step, Lint, web, error
        assert_eq!(depths, vec![0, 1, 2, 3, 4, 2, 1, 2]);

        let mut folds = Folds::default();
        folds.collapsed_groups.insert("proj".into());
        assert_eq!(build(&snap, &folds).len(), 1);
        // The header still sees every repo of a folded group.
        assert_eq!(repo_rows(&snap).len(), 2);
    }

    #[test]
    fn strips_the_shared_prefix() {
        let p = shared_prefix(&["brabo", "brabo-api", "brabo_web"]);
        assert_eq!(p.as_deref(), Some("brabo"));
        assert_eq!(short_name("brabo-api", p.as_deref()), "api");
        assert_eq!(short_name("brabo", p.as_deref()), "brabo");
        assert_eq!(shared_prefix(&["brabo-api", "hud"]), None);
        assert_eq!(shared_prefix(&["brabox", "brabo-api"]), None);
    }

    #[test]
    fn calm_repos_fold_into_one_line() {
        let calm = |name: &str, id: u64| RepoView {
            name: name.into(),
            slug: format!("o/{name}"),
            group: Some("p".into()),
            workflows: Ok(vec![WorkflowView {
                workflow_id: id,
                run_id: id,
                name: "CI".into(),
                branch: Some("main".into()),
                status: Status::Success,
                url: String::new(),
                started_at: None,
                updated_at: None,
                jobs: None,
            }]),
        };
        let mut snap = sample();
        snap.repos.truncate(1);
        snap.repos[0].name = "p-api".into();
        snap.repos[0].group = Some("p".into());
        for (i, n) in ["p-a", "p-b", "p-c", "p-d"].iter().enumerate() {
            snap.repos.push(calm(n, 50 + i as u64));
        }
        let mut folds = Folds::default();
        folds.collapsed_workflows.insert(1);
        let rows = build(&snap, &folds);
        let keys: Vec<&RowKey> = rows.iter().map(|r| &r.key).collect();
        assert_eq!(keys[1], &RowKey::Repo("o/api".into()));
        assert_eq!(rows[1].text, "api");
        assert_eq!(keys.last().unwrap(), &&RowKey::Calm("p".into()));
        let calm_row = rows.last().unwrap();
        assert_eq!(calm_row.text, "4 repos ok");
        assert_eq!(calm_row.lead.as_deref(), Some("a · b · c · +1"));
        assert_eq!(
            rows[0].health.as_ref().unwrap().counts,
            vec![(Status::Failure, 1), (Status::Success, 4)]
        );

        folds.open_calm.insert("p".into());
        let rows = build(&snap, &folds);
        let d = rows
            .iter()
            .find(|r| r.key == RowKey::Repo("o/p-d".into()))
            .unwrap();
        assert_eq!(d.text, "d");
        assert_eq!(d.depth, 2);
    }

    #[test]
    fn respects_folds() {
        let mut folds = Folds::default();
        folds.collapsed_workflows.insert(1);
        folds.collapsed_repos.insert("o/web".into());
        let rows = build(&sample(), &folds);
        assert_eq!(rows.len(), 4); // api, CI, Lint, web
    }

    #[test]
    fn shortens_long_branches() {
        let long = "dependabot/github_actions/dev/viralabs-dev/brabo-ci/workflows/x.yml-1.5.1";
        let short = shorten_middle(long, 40);
        assert_eq!(short.chars().count(), 40);
        assert!(short.starts_with("dependabot/github_a"));
        assert!(short.ends_with("x.yml-1.5.1"));
        assert_eq!(shorten_middle("main", 40), "main");
    }

    #[test]
    fn skips_branch_already_in_label() {
        assert_eq!(branch_detail("refs ← #152 (docs/x)", Some("docs/x")), None);
        assert_eq!(branch_detail("CI", Some("main")).as_deref(), Some("(main)"));
        assert_eq!(branch_detail("CI", None).as_deref(), Some("(-)"));
    }

    #[test]
    fn plain_output_without_color() {
        let text = render_plain(&build(&sample(), &Folds::default()), false);
        assert!(text.contains("    ✗ CI (main)\n"));
        assert!(text.contains("            ✗ 1. Run tests\n"));
        assert!(text.contains("    error: GitHub API 404: Not Found\n"));
    }

    #[test]
    fn parses_rfc3339() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2026-11-26T02:38:00Z"), Some(1_795_660_680));
        assert_eq!(parse_time("2026-11-26T02:38:00.000Z"), Some(1_795_660_680));
        assert_eq!(parse_time("2026-11-25T23:38:00-03:00"), Some(1_795_660_680));
        assert_eq!(parse_time("nope"), None);
    }

    fn step(number: u32, status: Status, start: &str, end: Option<&str>) -> StepView {
        StepView {
            number,
            name: format!("step {number}"),
            status,
            started_at: Some(start.into()),
            completed_at: end.map(Into::into),
        }
    }

    #[test]
    fn progress_stops_at_failure_and_names_it() {
        let mut snap = sample();
        let Ok(wfs) = &mut snap.repos[0].workflows else {
            unreachable!()
        };
        wfs[0].started_at = Some("2026-01-01T00:00:00Z".into());
        wfs[0].updated_at = Some("2026-01-01T00:02:14Z".into());
        let Some(Ok(jobs)) = &mut wfs[0].jobs else {
            unreachable!()
        };
        jobs[0].steps = vec![
            step(
                1,
                Status::Success,
                "2026-01-01T00:00:00Z",
                Some("2026-01-01T00:00:02Z"),
            ),
            step(
                2,
                Status::Failure,
                "2026-01-01T00:00:02Z",
                Some("2026-01-01T00:01:33Z"),
            ),
            step(
                3,
                Status::Skipped,
                "2026-01-01T00:01:33Z",
                Some("2026-01-01T00:01:33Z"),
            ),
        ];
        let rows = build_at(&snap, &Folds::default(), 0);
        let wf = rows[1].gauge.as_ref().unwrap();
        assert_eq!((wf.done, wf.total), (2, 3));
        assert_eq!(wf.hint.as_deref(), Some("test › 2. step 2"));
        assert_eq!(wf.elapsed, Some(Elapsed::Took(134)));
        let job = rows[2].gauge.as_ref().unwrap();
        assert_eq!(job.elapsed, Some(Elapsed::Took(93)));
        assert_eq!(job.hint.as_deref(), Some("l log"));
        // The repo row summarizes its failed pipeline.
        assert_eq!(rows[0].lead.as_deref(), Some("CI"));
        assert_eq!(rows[0].status, Some(Status::Failure));
    }

    #[test]
    fn running_job_counts_to_now_and_names_current_step() {
        let job = JobView {
            id: 1,
            name: "deploy".into(),
            status: Status::Running,
            url: None,
            steps: vec![
                step(
                    1,
                    Status::Success,
                    "2026-01-01T00:00:00Z",
                    Some("2026-01-01T00:00:03Z"),
                ),
                step(2, Status::Running, "2026-01-01T00:00:03Z", None),
                StepView {
                    number: 3,
                    name: "step 3".into(),
                    status: Status::Queued,
                    started_at: None,
                    completed_at: None,
                },
            ],
        };
        let now = parse_time("2026-01-01T00:00:24Z").unwrap();
        let gauge = job_gauge(&job, now);
        assert_eq!((gauge.done, gauge.total), (1, 3));
        assert_eq!(gauge.elapsed, Some(Elapsed::Took(24)));
        assert_eq!(gauge.hint.as_deref(), Some("2. step 2"));
        assert!((gauge.fraction().unwrap() - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn calm_repo_summarizes_pipelines() {
        let wf = |id, name: &str, updated: &str| WorkflowView {
            workflow_id: id,
            run_id: id,
            name: name.into(),
            branch: Some("main".into()),
            status: Status::Success,
            url: String::new(),
            started_at: None,
            updated_at: Some(updated.into()),
            jobs: None,
        };
        let wfs = vec![
            wf(1, "CI", "2026-01-01T00:00:00Z"),
            wf(2, "Release", "2026-01-01T01:00:00Z"),
        ];
        let now = parse_time("2026-01-01T01:12:00Z").unwrap();
        let (lead, branch, gauge) = repo_summary(&wfs, now).unwrap();
        assert_eq!(lead, "2 pipelines");
        assert_eq!(branch, None);
        assert_eq!(gauge.elapsed, Some(Elapsed::Ago(720)));
        assert_eq!(gauge.fraction(), Some(1.0));
    }
    #[test]
    fn search_keeps_matches_with_parents_and_children() {
        let rows = build(&sample(), &Folds::default());
        let keys = |rows: &[Row]| rows.iter().map(|r| r.key.clone()).collect::<Vec<_>>();

        // A step name: its job, workflow and repo stay above it.
        let found = search(rows.clone(), "run tests");
        assert_eq!(
            keys(&found),
            vec![
                RowKey::Repo("o/api".into()),
                RowKey::Workflow(1),
                RowKey::Job(1, 0),
                RowKey::Step(1, 0, 1),
            ]
        );

        // A workflow name keeps only that workflow, not its sibling.
        let found = search(rows.clone(), "lint");
        assert_eq!(
            keys(&found),
            vec![RowKey::Repo("o/api".into()), RowKey::Workflow(2)]
        );

        // A repo name keeps everything under it.
        let found = search(rows.clone(), "web");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].key, RowKey::Repo("o/web".into()));

        // A branch matches its workflow.
        let found = search(rows.clone(), "mai");
        assert_eq!(found[1].key, RowKey::Workflow(1));

        // Two matches in different repos each keep their own parents.
        let found = search(rows.clone(), "test");
        assert!(found.iter().any(|r| r.key == RowKey::Step(1, 0, 1)));
        assert_eq!(found[0].key, RowKey::Repo("o/api".into()));

        assert!(search(rows, "nothing like this").is_empty());
    }

    #[test]
    fn search_opens_folded_parents() {
        let mut snap = sample();
        for repo in &mut snap.repos {
            repo.group = Some("proj".into());
        }
        let mut folds = Folds::default();
        folds.collapsed_groups.insert("proj".into());
        folds.collapsed_repos.insert("o/api".into());
        let rows = build(&snap, &search_folds(&snap, &folds));
        let found = search(rows, "run tests");
        assert_eq!(
            found.first().map(|r| &r.key),
            Some(&RowKey::Group("proj".into()))
        );
        assert_eq!(found.last().map(|r| &r.key), Some(&RowKey::Step(1, 0, 1)));
    }
}
