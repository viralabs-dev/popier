//! The details sidebar (`d`): everything popier knows about the selected row.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::paint::{self, Look};
use crate::snapshot::{JobView, RepoView, Snapshot, StepView, WorkflowView};
use crate::status::Status;
use crate::theme;
use crate::tree::{self, Elapsed, Row, RowKey};

/// Short word for a state, as the panel title shows it.
pub fn word(status: Status) -> &'static str {
    match status {
        Status::Running => "running",
        Status::Queued => "queued",
        Status::Success => "ok",
        Status::Failure => "failed",
        Status::Skipped => "skipped",
        Status::Cancelled => "cancelled",
    }
}

/// Lines for the panel, `width` cells wide (inside its border).
pub fn lines(
    snapshot: &Snapshot,
    row: &Row,
    look: &Look,
    width: usize,
    now: i64,
    log: Option<&[String]>,
) -> Vec<Line<'static>> {
    let mut p = Panel {
        look,
        width,
        now,
        out: Vec::new(),
    };
    match &row.key {
        RowKey::Group(name) | RowKey::Calm(name) => {
            let members: Vec<&RepoView> = snapshot
                .repos
                .iter()
                .filter(|r| r.group.as_deref() == Some(name.as_str()))
                .collect();
            p.group(name, row, &members, matches!(row.key, RowKey::Calm(_)));
        }
        RowKey::Repo(slug) => {
            if let Some(repo) = snapshot.repos.iter().find(|r| &r.slug == slug) {
                p.repo(repo, row);
            }
        }
        RowKey::Workflow(run) => {
            if let Some((repo, wf)) = find_run(snapshot, *run) {
                p.workflow(repo, wf);
            }
        }
        RowKey::Job(run, j) => {
            if let Some((repo, wf)) = find_run(snapshot, *run)
                && let Some(job) = jobs(wf).and_then(|jobs| jobs.get(*j))
            {
                p.job(repo, wf, job);
            }
        }
        RowKey::Step(run, j, n) | RowKey::Log(run, j, n, _) => {
            if let Some((repo, wf)) = find_run(snapshot, *run)
                && let Some(job) = jobs(wf).and_then(|jobs| jobs.get(*j))
                && let Some(step) = job.steps.iter().find(|s| s.number == *n)
            {
                p.step(repo, wf, job, step);
                if let Some(log) = log {
                    p.log(log);
                }
                p.hints(&[("⏎", "log here"), ("l", "full log"), ("d", "close")]);
            }
        }
        RowKey::Note(..) => {
            let style = if row.is_error {
                p.fg(look.theme.failure)
            } else {
                p.fg(look.theme.muted)
            };
            for chunk in wrap(&row.text, width) {
                p.out.push(Line::from(Span::styled(chunk, style)));
            }
        }
    }
    if p.out.is_empty() {
        p.out.push(Line::from(Span::styled(
            "nothing to show for this row",
            p.fg(look.theme.muted),
        )));
    }
    p.out
}

fn find_run(snapshot: &Snapshot, run: u64) -> Option<(&RepoView, &WorkflowView)> {
    snapshot.repos.iter().find_map(|repo| {
        let wf = repo
            .workflows
            .as_ref()
            .ok()?
            .iter()
            .find(|w| w.run_id == run)?;
        Some((repo, wf))
    })
}

fn jobs(wf: &WorkflowView) -> Option<&Vec<JobView>> {
    wf.jobs.as_ref()?.as_ref().ok()
}

struct Panel<'a, 'b> {
    look: &'a Look<'b>,
    width: usize,
    now: i64,
    out: Vec<Line<'static>>,
}

impl Panel<'_, '_> {
    fn fg(&self, c: ratatui::style::Color) -> Style {
        Style::new().fg(c)
    }

    fn blank(&mut self) {
        self.out.push(Line::default());
    }

    /// Glyph, name in bold and the state word at the right edge.
    fn title(&mut self, glyph: &str, status: Option<Status>, name: &str) {
        let t = self.look.theme;
        let color = status.map_or(t.muted, |s| t.status(s));
        let right = status.map(word).unwrap_or_default();
        let room = self.width.saturating_sub(right.len() + 3);
        let name = cut(name, room);
        let pad = self
            .width
            .saturating_sub(2 + name.chars().count() + right.len());
        self.out.push(Line::from(vec![
            Span::styled(format!("{glyph} "), self.fg(color)),
            Span::styled(name, self.fg(t.ink).add_modifier(Modifier::BOLD)),
            Span::raw(" ".repeat(pad)),
            Span::styled(
                right.to_owned(),
                self.fg(color).add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    fn sub(&mut self, text: String) {
        let style = self.fg(self.look.theme.muted);
        self.out
            .push(Line::from(Span::styled(cut(&text, self.width), style)));
    }

    fn section(&mut self, label: &str) {
        self.blank();
        let style = self.fg(self.look.theme.muted).add_modifier(Modifier::BOLD);
        self.out
            .push(Line::from(Span::styled(label.to_uppercase(), style)));
    }

    /// The end of the step's log, one panel line per log line, cut to width.
    fn log(&mut self, lines: &[String]) {
        let t = self.look.theme;
        self.section(&format!("log (last {})", lines.len()));
        for line in lines {
            let (mark, text, style) = paint::log_marker(line, t);
            let mut spans = Vec::new();
            if let Some((glyph, s)) = mark {
                spans.push(Span::styled(format!("{glyph} "), s));
            }
            let used: usize = spans.iter().map(|s| s.width()).sum();
            spans.push(Span::styled(
                cut(&text, self.width.saturating_sub(used).max(4)),
                style,
            ));
            self.out.push(Line::from(spans));
        }
    }

    /// `label   value` with the label in a fixed column.
    fn field(&mut self, label: &str, value: String, style: Style) {
        let t = self.look.theme;
        let room = self.width.saturating_sub(11);
        self.out.push(Line::from(vec![
            Span::styled(format!("{label:<11}"), self.fg(t.faint)),
            Span::styled(cut(&value, room), style),
        ]));
    }

    /// A bar as wide as the panel, with the percent after it.
    fn wide_bar(&mut self, fraction: f64, status: Status) {
        let t = self.look.theme;
        let w = self.width.saturating_sub(6).max(4);
        let mut spans: Vec<Span> = theme::bar(fraction, w, self.look.bar, status)
            .into_iter()
            .map(|(text, filled)| {
                let color = if filled { t.status(status) } else { t.track };
                Span::styled(text, self.fg(color))
            })
            .collect();
        spans.push(Span::styled(
            format!("{:>5}%", (fraction * 100.0).floor() as u32),
            self.fg(t.ink),
        ));
        self.out.push(Line::from(spans));
    }

    fn ago(&self, ts: Option<&str>) -> Option<String> {
        let t = tree::parse_time(ts?)?;
        Some(
            paint::elapsed(Some(Elapsed::Ago((self.now - t).max(0) as u64)))
                .trim()
                .to_owned(),
        )
    }

    /// One child item: glyph, name, a short bar, count and time.
    fn item(&mut self, status: Status, name: &str, fraction: Option<f64>, right: String) {
        let t = self.look.theme;
        let bar_w = if self.width >= 44 { 10 } else { 0 };
        let right_w = 12;
        let name_w = self
            .width
            .saturating_sub(2 + right_w + if bar_w > 0 { bar_w + 1 } else { 0 });
        let name = cut(name, name_w);
        let mut spans = vec![
            Span::styled(format!("{} ", status.icon()), self.fg(t.status(status))),
            Span::styled(format!("{name:<name_w$}"), self.fg(t.ink)),
        ];
        if bar_w > 0 {
            spans.push(Span::raw(" "));
            match fraction {
                Some(f) => {
                    spans.extend(theme::bar(f, bar_w, self.look.bar, status).into_iter().map(
                        |(text, filled)| {
                            let c = if filled { t.status(status) } else { t.track };
                            Span::styled(text, self.fg(c))
                        },
                    ))
                }
                None => spans.push(Span::raw(" ".repeat(bar_w))),
            }
        }
        spans.push(Span::styled(format!("{right:>right_w$}"), self.fg(t.muted)));
        self.out.push(Line::from(spans));
    }

    fn hints(&mut self, keys: &[(&str, &str)]) {
        self.blank();
        let mut spans = paint::hints(keys, self.look);
        if let Some(first) = spans.first_mut() {
            *first = Span::raw("");
        }
        self.out.push(Line::from(spans));
    }

    fn group(&mut self, name: &str, row: &Row, members: &[&RepoView], calm_only: bool) {
        let t = self.look.theme;
        self.title("◼", row.status, name);
        self.sub(format!("{} repos", members.len()));
        if let Some(health) = &row.health {
            let w = self.width.saturating_sub(1).max(4);
            let ch = match self.look.bar {
                theme::BarStyle::Line => "━",
                theme::BarStyle::Block => "█",
            };
            self.blank();
            let spans: Vec<Span> = paint::health_segments(&health.counts, w)
                .into_iter()
                .map(|(s, n)| Span::styled(ch.repeat(n), self.fg(t.status(s))))
                .collect();
            self.out.push(Line::from(spans));
            let mut tally = Vec::new();
            for (s, n) in &health.counts {
                tally.push(Span::styled(
                    format!("{} {n} {}   ", s.icon(), word(*s)),
                    self.fg(t.status(*s)),
                ));
            }
            self.out.push(Line::from(tally));
            if !health.reasons.is_empty() {
                self.section("needs attention");
                for (s, text) in &health.reasons {
                    let style = if *s == Status::Failure {
                        self.fg(t.failure)
                    } else {
                        self.fg(t.ink)
                    };
                    self.out.push(Line::from(vec![
                        Span::styled(format!("{} ", s.icon()), self.fg(t.status(*s))),
                        Span::styled(cut(text, self.width.saturating_sub(2)), style),
                    ]));
                }
            }
        }
        self.section(if calm_only { "calm repos" } else { "repos" });
        for repo in members {
            let summary = repo_line(repo, self.now);
            if calm_only && summary.0 != Status::Success {
                continue;
            }
            let (status, lead, fraction, time) = summary;
            self.item(status, &format!("{}  {lead}", repo.name), fraction, time);
        }
        self.hints(&[("⏎", "fold"), ("d", "close")]);
    }

    fn repo(&mut self, repo: &RepoView, row: &Row) {
        let t = self.look.theme;
        self.title("●", row.status, &repo.name);
        self.sub(repo.slug.clone());
        if let Some(g) = &row.gauge
            && g.total > 0
            && let Some(f) = g.fraction()
        {
            self.blank();
            self.wide_bar(f, g.status);
            self.sub(format!(
                "{} · {}/{} steps",
                row.lead.clone().unwrap_or_default(),
                g.done,
                g.total
            ));
        }
        match &repo.workflows {
            Err(err) => {
                self.section("error");
                for chunk in wrap(err, self.width).into_iter().take(6) {
                    self.out
                        .push(Line::from(Span::styled(chunk, self.fg(t.failure))));
                }
            }
            Ok(wfs) => {
                self.section(&format!("workflows ({})", wfs.len()));
                for wf in wfs {
                    let branch = wf.branch.as_deref().unwrap_or("-");
                    let time = self.ago(wf.updated_at.as_deref()).unwrap_or_default();
                    let fraction = progress(wf);
                    self.item(wf.status, &format!("{}  {branch}", wf.name), fraction, time);
                }
            }
        }
        self.section("link");
        self.sub(format!("github.com/{}/actions", repo.slug));
        self.hints(&[("⏎", "fold"), ("o", "open"), ("d", "close")]);
    }

    fn workflow(&mut self, repo: &RepoView, wf: &WorkflowView) {
        let t = self.look.theme;
        self.title(wf.status.icon(), Some(wf.status), &wf.name);
        self.sub(format!(
            "{} · {}",
            repo.name,
            wf.branch.as_deref().unwrap_or("-")
        ));
        let (done, total) = jobs(wf).map_or((0, 0), |jobs| {
            jobs.iter().fold((0, 0), |(d, t), j| {
                let (jd, jt) = steps_done(j);
                (d + jd, t + jt)
            })
        });
        if total > 0 {
            self.blank();
            self.wide_bar(f64::from(done) / f64::from(total), wf.status);
        }
        self.section("run");
        if total > 0 {
            self.field("steps", format!("{done} of {total} done"), self.fg(t.ink));
        }
        if let Some(a) = self.ago(wf.started_at.as_deref()) {
            self.field("started", a, self.fg(t.ink));
        }
        let start = wf.started_at.as_deref().and_then(tree::parse_time);
        let end = match wf.status {
            Status::Running | Status::Queued => Some(self.now),
            _ => wf.updated_at.as_deref().and_then(tree::parse_time),
        };
        if let (Some(a), Some(b)) = (start, end) {
            let took = paint::elapsed(Some(Elapsed::Took((b - a).max(0) as u64)));
            self.field("took", took.trim().to_owned(), self.fg(t.ink));
        }
        self.field("run id", wf.run_id.to_string(), self.fg(t.muted));
        if let Some(jobs) = jobs(wf) {
            if let Some((job, step)) = jobs.iter().find_map(|j| {
                (j.status == Status::Failure)
                    .then(|| (j, j.steps.iter().find(|s| s.status == Status::Failure)))
            }) {
                let at = match step {
                    Some(s) => format!("{} › {}. {}", job.name, s.number, s.name),
                    None => job.name.clone(),
                };
                self.field("failed at", at, self.fg(t.failure));
            }
            self.section(&format!("jobs ({})", jobs.len()));
            for job in jobs {
                let (d, n) = steps_done(job);
                let fraction = (n > 0).then(|| f64::from(d) / f64::from(n));
                self.item(job.status, &job.name, fraction, format!("{d}/{n}"));
            }
        } else {
            self.section("jobs");
            self.sub("not loaded yet: ⏎ to unfold fetches them".into());
        }
        self.hints(&[("⏎", "fold"), ("o", "open"), ("d", "close")]);
    }

    fn job(&mut self, repo: &RepoView, wf: &WorkflowView, job: &JobView) {
        let t = self.look.theme;
        self.title(job.status.icon(), Some(job.status), &job.name);
        self.sub(format!("{} › {}", repo.name, wf.name));
        let (done, total) = steps_done(job);
        if total > 0 {
            self.blank();
            self.wide_bar(f64::from(done) / f64::from(total), job.status);
            self.field("steps", format!("{done} of {total} done"), self.fg(t.ink));
        }
        self.section(&format!("steps ({total})"));
        for step in &job.steps {
            let time = step_took(step, self.now).unwrap_or_default();
            self.item(
                step.status,
                &format!("{}. {}", step.number, step.name),
                None,
                time,
            );
        }
        self.hints(&[("l", "log"), ("o", "open"), ("d", "close")]);
    }

    fn step(&mut self, repo: &RepoView, wf: &WorkflowView, job: &JobView, step: &StepView) {
        let t = self.look.theme;
        self.title(
            step.status.icon(),
            Some(step.status),
            &format!("{}. {}", step.number, step.name),
        );
        self.sub(format!("{} › {} › {}", repo.name, wf.name, job.name));
        self.section("step");
        self.field(
            "position",
            format!("{} of {}", step.number, job.steps.len()),
            self.fg(t.ink),
        );
        if let Some(a) = self.ago(step.started_at.as_deref()) {
            self.field("started", a, self.fg(t.ink));
        }
        if let Some(took) = step_took(step, self.now) {
            self.field("took", took, self.fg(t.ink));
        }
        let style = self.fg(t.status(step.status));
        self.field("state", word(step.status).into(), style);
    }
}

/// State, lead pipeline, its progress and when it last changed, for a repo.
fn repo_line(repo: &RepoView, now: i64) -> (Status, String, Option<f64>, String) {
    let Ok(wfs) = &repo.workflows else {
        return (Status::Failure, "cannot load".into(), None, String::new());
    };
    let rank = |s: Status| match s {
        Status::Failure => 0,
        Status::Running => 1,
        Status::Queued => 2,
        _ => 3,
    };
    let Some(lead) = wfs.iter().min_by(|a, b| {
        rank(a.status)
            .cmp(&rank(b.status))
            .then_with(|| b.updated_at.cmp(&a.updated_at))
    }) else {
        return (Status::Skipped, "no runs".into(), None, String::new());
    };
    let ago = lead
        .updated_at
        .as_deref()
        .and_then(tree::parse_time)
        .map(|t| {
            paint::elapsed(Some(Elapsed::Ago((now - t).max(0) as u64)))
                .trim()
                .to_owned()
        })
        .unwrap_or_default();
    (lead.status, lead.name.clone(), progress(lead), ago)
}

fn progress(wf: &WorkflowView) -> Option<f64> {
    let (d, n) = jobs(wf)?
        .iter()
        .map(steps_done)
        .fold((0, 0), |(d, t), (jd, jt)| (d + jd, t + jt));
    if n > 0 {
        Some(f64::from(d) / f64::from(n))
    } else {
        match wf.status {
            Status::Running => None,
            Status::Queued => Some(0.0),
            _ => Some(1.0),
        }
    }
}

/// Same rule as the tree: count up to the first failure or cancellation.
fn steps_done(job: &JobView) -> (u32, u32) {
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

fn step_took(step: &StepView, now: i64) -> Option<String> {
    let a = tree::parse_time(step.started_at.as_deref()?)?;
    let b = match step.status {
        Status::Running => now,
        _ => tree::parse_time(step.completed_at.as_deref()?)?,
    };
    Some(
        paint::elapsed(Some(Elapsed::Took((b - a).max(0) as u64)))
            .trim()
            .to_owned(),
    )
}

fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn wrap(s: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let chars: Vec<char> = s.chars().collect();
    chars.chunks(width).map(|c| c.iter().collect()).collect()
}

/// Panel width for a screen `width` cells wide, or `None` to cover the list.
pub fn panel_width(width: u16) -> Option<u16> {
    (width >= 90).then(|| (width * 2 / 5).clamp(38, 64))
}

/// A terminal taller than it is wide on screen (a cell is about twice as
/// tall as it is wide), as on a monitor turned upright.
pub fn portrait(width: u16, height: u16) -> bool {
    height >= 30 && u32::from(height) * 2 >= u32::from(width)
}

/// Where the details panel goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// To the right of the tree, this many columns wide.
    Right(u16),
    /// Under the tree, full width, this many rows tall.
    Below(u16),
    /// Over the tree.
    Cover,
}

/// Right on a wide terminal, below on a portrait one, over the tree when
/// there is room for neither.
pub fn placement(width: u16, height: u16) -> Placement {
    if portrait(width, height) {
        return Placement::Below((height * 2 / 5).clamp(14, 30));
    }
    panel_width(width).map_or(Placement::Cover, Placement::Right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upright_terminals_put_the_panel_below() {
        assert!(portrait(90, 56));
        assert!(portrait(72, 50));
        assert!(!portrait(140, 26));
        assert!(!portrait(120, 40));
        assert!(!portrait(50, 20), "too short to split");
        assert_eq!(placement(90, 56), Placement::Below(22));
        assert_eq!(placement(100, 80), Placement::Below(30));
        assert_eq!(placement(140, 26), Placement::Right(56));
        assert_eq!(placement(80, 24), Placement::Cover);
    }

    #[test]
    fn panel_takes_two_fifths_within_bounds() {
        assert_eq!(panel_width(80), None);
        assert_eq!(panel_width(100), Some(40));
        assert_eq!(panel_width(200), Some(64));
        assert_eq!(panel_width(90), Some(38));
    }

    #[test]
    fn wraps_and_cuts() {
        assert_eq!(wrap("abcdefghij", 8), vec!["abcdefgh", "ij"]);
        assert_eq!(cut("abcdef", 4), "abc…");
        assert_eq!(cut("abc", 4), "abc");
    }
}
