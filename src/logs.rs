//! Cuts one step out of a job log.
//!
//! GitHub only serves logs per job (plain text, one line per output line,
//! each prefixed with an RFC 3339 timestamp). Step times have second
//! precision and neighbouring steps often start in the same second, so times
//! alone leak lines between steps. Each step that runs a command or action
//! opens its output with a `##[group]Run …` line: those markers are matched
//! to the steps in order (the first marker at or after each step's start), and
//! a step's lines run from its marker to the next step's marker. Times are
//! only the fallback when a step has no marker.

/// Most recent lines shown for a step.
pub const STEP_TAIL: usize = 200;

/// Start and end of a step, as reported by the jobs API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepSpan {
    pub number: u32,
    pub name: String,
    pub started: Option<String>,
    pub completed: Option<String>,
    /// Skipped steps print nothing and must not claim a marker.
    pub skipped: bool,
}

const MARKER: &str = "##[group]Run ";
const MARKER_TEXT: &str = "Run ";
const POST_JOB: &str = "Post job cleanup.";

/// `2026-11-25T22:19:44` part of a timestamped log line, if it has one.
fn line_second(line: &str) -> Option<&str> {
    let line = line.strip_prefix('\u{feff}').unwrap_or(line);
    let stamp = line.split(' ').next()?;
    let b = stamp.as_bytes();
    let looks_like =
        b.len() >= 20 && b[4] == b'-' && b[7] == b'-' && b[10] == b'T' && b[13] == b':';
    looks_like.then(|| &stamp[..19])
}

/// The line without its timestamp prefix (and without a leading BOM).
pub fn strip_timestamp(line: &str) -> &str {
    let line = line.strip_prefix('\u{feff}').unwrap_or(line);
    match line_second(line) {
        Some(_) => line.split_once(' ').map_or("", |(_, rest)| rest),
        None => line,
    }
}

/// Lines of `log` that belong to the step between `started` and `completed`
/// (GitHub timestamps, second precision). `None` when nothing matched, so
/// the caller can fall back to the whole job.
pub fn slice_step(
    log: &str,
    started: Option<&str>,
    completed: Option<&str>,
) -> Option<Vec<String>> {
    let start = started.and_then(|s| s.get(..19))?;
    let end = completed.and_then(|s| s.get(..19));
    let mut inside = false;
    let mut out = Vec::new();
    for line in log.lines() {
        if let Some(second) = line_second(line) {
            inside = second >= start && end.is_none_or(|end| second <= end);
        }
        if inside {
            out.push(strip_timestamp(line).to_owned());
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Lines of `step` in `log`, using `##[group]Run` markers as boundaries
/// (see the module docs). `steps` are all steps of the job, in order.
/// `None` when the step can't be located, so the caller can fall back.
pub fn cut_step(log: &str, steps: &[StepSpan], number: u32) -> Option<Vec<String>> {
    let lines: Vec<&str> = log.lines().collect();
    let markers = top_level_markers(&lines);

    // Assign markers to steps in order. A step named "Run …" takes the first
    // unused marker with the same text; any other step takes the first unused
    // marker at or after its start and no later than its end.
    let mut assigned: Vec<(u32, usize)> = Vec::new();
    let mut next = 0;
    for (index, step) in steps.iter().enumerate() {
        if !claims_marker(index, step) {
            continue;
        }
        let Some(start) = step.started.as_deref().and_then(|s| s.get(..19)) else {
            continue;
        };
        let end = step.completed.as_deref().and_then(|s| s.get(..19));
        let in_window = |m: &Marker| m.second >= start && end.is_none_or(|end| m.second <= end);
        let by_name = step.name.starts_with(MARKER_TEXT).then(|| {
            markers[next..]
                .iter()
                .position(|m| in_window(m) && same_command(&m.text, &step.name))
        });
        let found = by_name
            .flatten()
            .or_else(|| markers[next..].iter().position(|m| m.second >= start))
            .map(|p| p + next)
            .filter(|&p| in_window(&markers[p]));
        if let Some(p) = found {
            assigned.push((step.number, markers[p].line));
            next = p + 1;
        }
    }

    let pos = assigned.iter().position(|(n, _)| *n == number)?;
    let from = assigned[pos].1;
    let end = steps
        .iter()
        .find(|s| s.number == number)
        .and_then(|s| s.completed.as_deref())
        .and_then(|s| s.get(..19));
    let limit = assigned.get(pos + 1).map_or(lines.len(), |&(_, line)| line);
    // Stop early at the post-job section, and (for the last step) at the
    // first line after the step's end time.
    let to = lines[from + 1..limit]
        .iter()
        .position(|l| {
            strip_timestamp(l).starts_with(POST_JOB)
                || (assigned.get(pos + 1).is_none()
                    && matches!((line_second(l), end), (Some(sec), Some(end)) if sec > end))
        })
        .map_or(limit, |p| from + 1 + p);
    Some(
        lines[from..to]
            .iter()
            .map(|l| strip_timestamp(l).to_owned())
            .collect(),
    )
}

struct Marker<'a> {
    line: usize,
    second: &'a str,
    /// `Run …` text after `##[group]`.
    text: String,
}

/// `##[group]Run` lines outside composite actions (`##[start-action]` …
/// `##[end-action]` wrap the inner steps of a composite action, whose own
/// `Run` markers belong to the outer step).
fn top_level_markers<'a>(lines: &[&'a str]) -> Vec<Marker<'a>> {
    let mut depth = 0usize;
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let text = strip_timestamp(l);
        if text.starts_with("##[start-action") {
            depth += 1;
        } else if text.starts_with("##[end-action") {
            depth = depth.saturating_sub(1);
        } else if depth == 0
            && text.starts_with(MARKER)
            && let Some(second) = line_second(l)
        {
            let text = text.trim_start_matches("##[group]").trim().to_owned();
            out.push(Marker {
                line: i,
                second,
                text,
            });
        }
    }
    out
}

/// Whether a step can own a `Run` marker: not skipped, not the runner's own
/// "Set up job" (always first), not post steps or "Complete job".
fn claims_marker(index: usize, step: &StepSpan) -> bool {
    !step.skipped
        && index > 0
        && step.name != "Set up job"
        && step.name != "Complete job"
        && !step.name.starts_with("Post ")
}

/// GitHub names an unnamed `run:` step after the start of its command, and
/// may cut either side, so compare by prefix both ways.
fn same_command(marker: &str, step_name: &str) -> bool {
    let (a, b) = (marker.trim(), step_name.trim());
    !a.is_empty() && !b.is_empty() && (a.starts_with(b) || b.starts_with(a))
}

/// Every line of the job log, timestamps removed.
pub fn whole_job(log: &str) -> Vec<String> {
    log.lines().map(|l| strip_timestamp(l).to_owned()).collect()
}

/// Keeps the last `n` lines.
pub fn tail(mut lines: Vec<String>, n: usize) -> Vec<String> {
    if lines.len() > n {
        lines.drain(..lines.len() - n);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "\
2026-11-25T22:19:40.1000000Z ##[group]Run actions/checkout@v4
2026-11-25T22:19:41.2000000Z Syncing repository
2026-11-25T22:19:44.0000000Z ##[group]Run cargo test
2026-11-25T22:19:45.0000000Z running 3 tests
continuation without stamp
2026-11-25T22:19:50.5000000Z ##[error]Process completed with exit code 101.
2026-11-25T22:19:55.0000000Z Post job cleanup.";

    #[test]
    fn slices_by_step_times() {
        let lines = slice_step(
            LOG,
            Some("2026-11-25T22:19:44Z"),
            Some("2026-11-25T22:19:50Z"),
        )
        .unwrap();
        assert_eq!(
            lines,
            vec![
                "##[group]Run cargo test",
                "running 3 tests",
                "continuation without stamp",
                "##[error]Process completed with exit code 101.",
            ]
        );
    }

    #[test]
    fn running_step_has_no_end() {
        let lines = slice_step(LOG, Some("2026-11-25T22:19:50Z"), None).unwrap();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn no_match_or_no_times() {
        assert!(slice_step(LOG, Some("2026-11-26T00:00:00Z"), None).is_none());
        assert!(slice_step(LOG, None, None).is_none());
    }

    fn span(number: u32, start: &str, end: &str) -> StepSpan {
        named(number, &format!("step {number}"), start, end)
    }

    fn named(number: u32, name: &str, start: &str, end: &str) -> StepSpan {
        StepSpan {
            number,
            name: name.to_owned(),
            started: Some(format!("2026-11-25T{start}Z")),
            completed: Some(format!("2026-11-25T{end}Z")),
            skipped: false,
        }
    }

    // Steps 2, 3 and 4 all start within the same second.
    const NEIGHBOURS: &str = "\u{feff}2026-11-25T21:54:20.1Z Current runner version: '2.338.2'
2026-11-25T21:54:20.2Z ##[group]Runner Image Provisioner
2026-11-25T21:54:20.3Z ##[endgroup]
2026-11-25T21:54:23.1Z ##[group]Run pnpm lint
2026-11-25T21:54:23.2Z lint ok
2026-11-25T21:54:23.3Z ##[group]Run pnpm build
2026-11-25T21:54:23.4Z build ok
2026-11-25T21:54:23.5Z ##[group]Run pnpm test:scripts
2026-11-25T21:54:24.0Z running
2026-11-25T21:54:51.0Z ##[error]AssertionError
2026-11-25T21:54:51.1Z ##[error]Process completed with exit code 1.
2026-11-25T21:54:52.0Z Post job cleanup.
2026-11-25T21:54:53.0Z Cleaning up orphan processes";

    fn neighbour_steps() -> Vec<StepSpan> {
        let mut skipped = span(5, "21:54:51", "21:54:51");
        skipped.skipped = true;
        vec![
            named(1, "Set up job", "21:54:20", "21:54:22"),
            span(2, "21:54:23", "21:54:23"),
            span(3, "21:54:23", "21:54:23"),
            span(4, "21:54:23", "21:54:51"),
            skipped,
            named(6, "Complete job", "21:54:52", "21:54:53"),
        ]
    }

    #[test]
    fn cuts_steps_that_start_in_the_same_second() {
        let steps = neighbour_steps();
        assert_eq!(
            cut_step(NEIGHBOURS, &steps, 2).unwrap(),
            vec!["##[group]Run pnpm lint", "lint ok"]
        );
        assert_eq!(
            cut_step(NEIGHBOURS, &steps, 4).unwrap(),
            vec![
                "##[group]Run pnpm test:scripts",
                "running",
                "##[error]AssertionError",
                "##[error]Process completed with exit code 1.",
            ]
        );
    }

    #[test]
    fn steps_without_marker_are_not_found() {
        let steps = neighbour_steps();
        assert!(cut_step(NEIGHBOURS, &steps, 1).is_none());
        assert!(cut_step(NEIGHBOURS, &steps, 5).is_none());
        assert!(cut_step(NEIGHBOURS, &steps, 6).is_none());
    }

    // Shape of a real job (brabo, 25/11): "Set up job" ends in the same second
    // the checkout marker appears; a composite action has an inner Run marker;
    // the failing step shares its end second with the post-job cleanup.
    const REAL_SHAPE: &str = "2026-11-25T21:53:29.0Z Current runner version: '2.338.2'
2026-11-25T21:53:29.1Z ##[group]Runner Image Provisioner
2026-11-25T21:53:31.0Z Complete job name: Drift
2026-11-25T21:53:31.5Z ##[group]Run actions/checkout@v4
2026-11-25T21:53:32.0Z checked out
2026-11-25T21:53:40.0Z ##[group]Run viralabs-dev/brabo-ci/.github/actions/vinculos@main
2026-11-25T21:53:40.1Z ##[start-action display=vinculos id=v]
2026-11-25T21:53:40.2Z ##[group]Run node vinculos.js
2026-11-25T21:53:41.0Z vinculos ok
2026-11-25T21:53:41.1Z ##[end-action id=v]
2026-11-25T21:54:23.5Z ##[group]Run pnpm test:scripts
2026-11-25T21:54:40.0Z ##[error]AssertionError
2026-11-25T21:54:51.0Z ##[error]Process completed with exit code 1.
2026-11-25T21:54:51.2Z Post job cleanup.
2026-11-25T21:54:51.3Z [command]/usr/bin/git version
2026-11-25T21:54:51.9Z Cleaning up orphan processes";

    fn real_steps() -> Vec<StepSpan> {
        vec![
            named(1, "Set up job", "21:53:29", "21:53:31"),
            named(2, "Run actions/checkout@v4", "21:53:31", "21:53:39"),
            named(3, "vinculos", "21:53:40", "21:53:42"),
            named(4, "Specs dos scripts", "21:54:23", "21:54:51"),
            named(5, "Post Run actions/checkout@v4", "21:54:51", "21:54:51"),
            named(6, "Complete job", "21:54:51", "21:54:52"),
        ]
    }

    #[test]
    fn set_up_job_does_not_steal_the_next_marker() {
        let steps = real_steps();
        assert!(cut_step(REAL_SHAPE, &steps, 1).is_none());
        assert_eq!(
            cut_step(REAL_SHAPE, &steps, 2).unwrap(),
            vec!["##[group]Run actions/checkout@v4", "checked out"]
        );
    }

    #[test]
    fn composite_inner_markers_stay_with_their_step() {
        let lines = cut_step(REAL_SHAPE, &real_steps(), 3).unwrap();
        assert_eq!(
            lines.first().unwrap(),
            "##[group]Run viralabs-dev/brabo-ci/.github/actions/vinculos@main"
        );
        assert_eq!(lines.last().unwrap(), "##[end-action id=v]");
    }

    #[test]
    fn failing_step_stops_before_post_job_cleanup() {
        assert_eq!(
            cut_step(REAL_SHAPE, &real_steps(), 4).unwrap(),
            vec![
                "##[group]Run pnpm test:scripts",
                "##[error]AssertionError",
                "##[error]Process completed with exit code 1.",
            ]
        );
    }

    #[test]
    fn run_steps_match_by_text() {
        assert!(same_command(
            "Run pnpm test:scripts",
            "Run pnpm test:scripts"
        ));
        assert!(same_command(
            "Run pnpm test:scripts --reporter dot",
            "Run pnpm test:scripts"
        ));
        assert!(!same_command("Run pnpm lint", "Run pnpm build"));
    }

    #[test]
    fn handles_bom_on_first_line() {
        let first = NEIGHBOURS.lines().next().unwrap();
        assert_eq!(strip_timestamp(first), "Current runner version: '2.338.2'");
        assert_eq!(line_second(first), Some("2026-11-25T21:54:20"));
    }

    #[test]
    fn strips_and_tails() {
        assert_eq!(
            strip_timestamp("2026-11-25T22:19:40.1000000Z hello"),
            "hello"
        );
        assert_eq!(strip_timestamp("plain"), "plain");
        assert_eq!(whole_job(LOG).len(), 7);
        let t = tail(whole_job(LOG), 2);
        assert_eq!(
            t,
            vec![
                "##[error]Process completed with exit code 101.",
                "Post job cleanup."
            ]
        );
    }
}
