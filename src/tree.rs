//! Flattens a snapshot into display rows, shared by the TUI and the plain printer.

use std::collections::HashSet;

use crate::snapshot::{JobView, RepoView, Snapshot, WorkflowView};
use crate::status::Status;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowKey {
    /// A `[[group]]` of repositories, by name.
    Group(String),
    Repo(String),
    Workflow(u64),
    Job(u64, usize),
    Step(u64, usize, u32),
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
    pub collapsed_repos: HashSet<String>,
    pub collapsed_workflows: HashSet<u64>,
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
            push_repo(&mut rows, &repos[i], folds, now);
            i += 1;
            continue;
        };
        let end = i + repos[i..]
            .iter()
            .take_while(|r| r.group.as_deref() == Some(group))
            .count();
        let members = &repos[i..end];
        let open = !folds.collapsed_groups.contains(group);
        rows.push(group_row(group, members, open, now));
        if open {
            let start = rows.len();
            for repo in members {
                push_repo(&mut rows, repo, folds, now);
            }
            rows[start..].iter_mut().for_each(|r| r.depth += 1);
        }
        i = end;
    }
    rows
}

/// Only the repo rows, every repo included even inside a folded group
/// (for the header's totals).
pub fn repo_rows(snapshot: &Snapshot) -> Vec<Row> {
    let folds = Folds {
        collapsed_repos: snapshot.repos.iter().map(|r| r.slug.clone()).collect(),
        ..Folds::default()
    };
    build(snapshot, &folds)
        .into_iter()
        .filter(|r| matches!(r.key, RowKey::Repo(_)))
        .collect()
}

/// Lead pipeline name, its branch, and the gauge a summary row shows.
type Summary = (String, Option<String>, Gauge);

/// A group row: the worst state among its repos, with the repo and pipeline
/// that cause it, or how many repos it holds when all is calm.
fn group_row(name: &str, members: &[RepoView], open: bool, now: i64) -> Row {
    let summaries: Vec<(&RepoView, Summary)> = members
        .iter()
        .filter_map(|m| {
            let s = m
                .workflows
                .as_ref()
                .ok()
                .and_then(|w| repo_summary(w, now))?;
            Some((m, s))
        })
        .collect();
    let worst = summaries
        .iter()
        .min_by_key(|(_, (_, _, g))| attention_rank(g.status));
    let count = format!(
        "{} repo{}",
        members.len(),
        if members.len() == 1 { "" } else { "s" }
    );
    let (lead, branch, gauge) = match worst {
        Some((repo, (lead, branch, gauge))) if attention_rank(gauge.status) < 3 => (
            format!("{} · {lead}", repo.name),
            branch.clone(),
            Some(gauge.clone()),
        ),
        Some((_, (_, _, gauge))) => {
            // All calm: when the most recent pipeline of the group ended.
            let ago = summaries
                .iter()
                .filter_map(|(_, (_, _, g))| match g.elapsed {
                    Some(Elapsed::Ago(s)) => Some(s),
                    _ => None,
                })
                .min()
                .map(Elapsed::Ago);
            let calm = Gauge {
                status: gauge.status,
                done: 0,
                total: 0,
                elapsed: ago,
                hint: None,
            };
            (count.clone(), None, Some(calm))
        }
        None => (count.clone(), None, None),
    };
    Row {
        key: RowKey::Group(name.to_owned()),
        depth: 0,
        status: gauge.as_ref().map(|g| g.status),
        text: name.to_owned(),
        detail: None,
        is_error: false,
        url: None,
        foldable: true,
        expanded: open,
        gauge,
        branch,
        lead: Some(lead),
    }
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
fn push_repo(rows: &mut Vec<Row>, repo: &RepoView, folds: &Folds, now: i64) {
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
        text: repo.name.clone(),
        detail: None,
        is_error: false,
        url: Some(repo_url),
        foldable: true,
        expanded: repo_open,
        gauge,
        branch,
        lead,
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
                            foldable: false,
                            expanded: false,
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
                        });
                    }
                }
            }
        }
    }
}

fn note(slug: &str, idx: usize, text: String, is_error: bool) -> Row {
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
        assert_eq!(rows[0].lead.as_deref(), Some("api · CI"));
        let depths: Vec<u8> = rows.iter().map(|r| r.depth).collect();
        assert_eq!(depths, vec![0, 1, 2, 3, 4, 2, 1, 2]);

        let mut folds = Folds::default();
        folds.collapsed_groups.insert("proj".into());
        assert_eq!(build(&snap, &folds).len(), 1);
        // The header still sees every repo of a folded group.
        assert_eq!(repo_rows(&snap).len(), 2);
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
}
