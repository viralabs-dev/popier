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
    pub started: Option<String>,
    pub completed: Option<String>,
    /// Skipped steps print nothing and must not claim a marker.
    pub skipped: bool,
}

const MARKER: &str = "##[group]Run ";

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
    let markers: Vec<(usize, &str)> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| {
            let second = line_second(l)?;
            strip_timestamp(l)
                .starts_with(MARKER)
                .then_some((i, second))
        })
        .collect();

    // Assign markers to steps in order: each step takes the first unused
    // marker at or after its start and no later than its end.
    let mut assigned: Vec<(u32, usize)> = Vec::new();
    let mut next = 0;
    for step in steps.iter().filter(|s| !s.skipped) {
        let Some(start) = step.started.as_deref().and_then(|s| s.get(..19)) else {
            continue;
        };
        let end = step.completed.as_deref().and_then(|s| s.get(..19));
        let found = markers[next..]
            .iter()
            .position(|(_, sec)| *sec >= start)
            .map(|p| p + next)
            .filter(|&p| end.is_none_or(|end| markers[p].1 <= end));
        if let Some(p) = found {
            assigned.push((step.number, markers[p].0));
            next = p + 1;
        }
    }

    let pos = assigned.iter().position(|(n, _)| *n == number)?;
    let from = assigned[pos].1;
    let to = match assigned.get(pos + 1) {
        Some(&(_, line)) => line,
        // Last step with a marker: stop at the first line after its end time.
        None => {
            let end = steps
                .iter()
                .find(|s| s.number == number)
                .and_then(|s| s.completed.as_deref())
                .and_then(|s| s.get(..19));
            lines
                .iter()
                .enumerate()
                .skip(from + 1)
                .find(|(_, l)| matches!((line_second(l), end), (Some(sec), Some(end)) if sec > end))
                .map_or(lines.len(), |(i, _)| i)
        }
    };
    Some(
        lines[from..to]
            .iter()
            .map(|l| strip_timestamp(l).to_owned())
            .collect(),
    )
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
        StepSpan {
            number,
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
            span(1, "21:54:20", "21:54:22"),
            span(2, "21:54:23", "21:54:23"),
            span(3, "21:54:23", "21:54:23"),
            span(4, "21:54:23", "21:54:51"),
            skipped,
            span(6, "21:54:52", "21:54:53"),
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
