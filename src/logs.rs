//! Cuts one step out of a job log.
//!
//! GitHub only serves logs per job (plain text, one line per output line,
//! each prefixed with an RFC 3339 timestamp). Step times have second
//! precision and neighbouring steps often start in the same second, so times
//! alone leak lines between steps. Each step that runs a command or action
//! opens its output with a `##[group]Run …` line: those markers are matched
//! to the steps in order (the first marker at or after each step's start), and
//! a step's lines run from its marker to the next step's marker.
//!
//! The runner's own steps have no marker. "Set up job" is everything before
//! the first marker. Each post step (`Post Run actions/checkout@v4`) opens
//! with its own `Post job cleanup.` line, in the order the steps are listed,
//! and "Complete job" starts at `Cleaning up orphan processes`. Times are
//! only the fallback when a step can't be placed this way.

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
const COMPLETE_JOB: &str = "Cleaning up orphan processes";

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

/// A log line made safe to print: ANSI escape sequences (colors, cursor
/// moves, window titles, hyperlinks) and other control characters removed,
/// tabs turned into spaces. Logs are written by whatever a workflow runs, so
/// they must never drive the terminal.
pub fn clean(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                // CSI: parameters and intermediates, then one final byte.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC, DCS, SOS, PM, APC: up to BEL or ESC \.
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                // Two-byte escapes (ESC 7, ESC c, ...).
                _ => {}
            },
            // C1 CSI.
            '\u{9b}' => {
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            '\t' => out.push_str("    "),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
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
            out.push(clean(strip_timestamp(line)));
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
    let index = steps.iter().position(|s| s.number == number)?;
    if let Some(range) = runner_step(&lines, &markers, steps, index) {
        return Some(
            lines[range]
                .iter()
                .map(|l| clean(strip_timestamp(l)))
                .collect(),
        );
    }

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
            .map(|l| clean(strip_timestamp(l)))
            .collect(),
    )
}

/// Line range of a step the runner adds around the job's own steps: "Set up
/// job", a post step or "Complete job". `None` for any other step, or when
/// the log doesn't have the expected shape.
fn runner_step(
    lines: &[&str],
    markers: &[Marker],
    steps: &[StepSpan],
    index: usize,
) -> Option<std::ops::Range<usize>> {
    let step = &steps[index];
    let starts = |i: usize, text: &str| strip_timestamp(lines[i]).starts_with(text);
    let post_lines: Vec<usize> = (0..lines.len()).filter(|&i| starts(i, POST_JOB)).collect();
    let complete = (0..lines.len()).find(|&i| starts(i, COMPLETE_JOB));
    let after_posts = complete.unwrap_or(lines.len());

    if index == 0 && step.name == "Set up job" {
        let end = markers
            .first()
            .map(|m| m.line)
            .into_iter()
            .chain(post_lines.first().copied())
            .chain(complete)
            .min()
            .unwrap_or(lines.len());
        return (end > 0).then_some(0..end);
    }
    if step.name == "Complete job" {
        return complete.map(|c| c..lines.len());
    }
    if step.name.starts_with("Post ") && !step.skipped {
        // Post steps that ran, in order, each opening with "Post job cleanup.".
        let posts: Vec<u32> = steps
            .iter()
            .filter(|s| s.name.starts_with("Post ") && !s.skipped)
            .map(|s| s.number)
            .collect();
        let post_lines: Vec<usize> = post_lines
            .into_iter()
            .filter(|&l| l < after_posts)
            .collect();
        if posts.len() != post_lines.len() {
            return None;
        }
        let k = posts.iter().position(|&n| n == step.number)?;
        let end = post_lines.get(k + 1).copied().unwrap_or(after_posts);
        return Some(post_lines[k]..end);
    }
    None
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
    log.lines().map(|l| clean(strip_timestamp(l))).collect()
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
    fn runner_steps_are_cut_by_their_own_lines() {
        let steps = neighbour_steps();
        assert_eq!(
            cut_step(NEIGHBOURS, &steps, 1).unwrap(),
            vec![
                "Current runner version: '2.338.2'",
                "##[group]Runner Image Provisioner",
                "##[endgroup]"
            ]
        );
        // A skipped step without a name prints nothing and isn't found.
        assert!(cut_step(NEIGHBOURS, &steps, 5).is_none());
        assert_eq!(
            cut_step(NEIGHBOURS, &steps, 6).unwrap(),
            vec!["Cleaning up orphan processes"]
        );
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
        let setup = cut_step(REAL_SHAPE, &steps, 1).unwrap();
        assert_eq!(setup.last().unwrap(), "Complete job name: Drift");
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

    // Tail of a real job (popier CI, 26/11): two post steps, each opening
    // with "Post job cleanup.", then the runner's own cleanup.
    const POST_LOG: &str = "\
2026-11-25T03:52:00.04Z Current runner version: '2.330.0'
2026-11-25T03:52:00.04Z ##[group]Runner Image
2026-11-25T03:52:00.04Z Image: ubuntu-24.04
2026-11-25T03:52:00.04Z ##[endgroup]
2026-11-25T03:52:01.04Z Complete job name: check
2026-11-25T03:52:01.09Z ##[group]Run actions/checkout@v4
2026-11-25T03:52:01.09Z with:
2026-11-25T03:52:29.59Z ##[group]Run cargo test
2026-11-25T03:52:37.41Z test result: ok.
2026-11-25T03:52:37.42Z Post job cleanup.
2026-11-25T03:52:37.51Z Cache up-to-date.
2026-11-25T03:52:37.52Z Post job cleanup.
2026-11-25T03:52:37.60Z [command]/usr/bin/git version
2026-11-25T03:52:37.77Z Cleaning up orphan processes
2026-11-25T03:52:37.79Z ##[warning]Node.js 20 is deprecated.";

    fn post_steps() -> Vec<StepSpan> {
        vec![
            named(1, "Set up job", "03:52:00", "03:52:01"),
            named(2, "Run actions/checkout@v4", "03:52:01", "03:52:01"),
            named(7, "Test", "03:52:29", "03:52:37"),
            named(
                13,
                "Post Run Swatinem/rust-cache@v2",
                "03:52:37",
                "03:52:37",
            ),
            named(14, "Post Run actions/checkout@v4", "03:52:37", "03:52:37"),
            named(15, "Complete job", "03:52:37", "03:52:37"),
        ]
    }

    #[test]
    fn set_up_job_is_everything_before_the_first_marker() {
        let lines = cut_step(POST_LOG, &post_steps(), 1).unwrap();
        assert_eq!(lines.first().unwrap(), "Current runner version: '2.330.0'");
        assert_eq!(lines.last().unwrap(), "Complete job name: check");
    }

    #[test]
    fn each_post_step_gets_its_own_cleanup() {
        let cache = cut_step(POST_LOG, &post_steps(), 13).unwrap();
        assert_eq!(cache, vec!["Post job cleanup.", "Cache up-to-date."]);
        let checkout = cut_step(POST_LOG, &post_steps(), 14).unwrap();
        assert_eq!(
            checkout,
            vec!["Post job cleanup.", "[command]/usr/bin/git version"]
        );
        let complete = cut_step(POST_LOG, &post_steps(), 15).unwrap();
        assert_eq!(complete[0], "Cleaning up orphan processes");
        assert_eq!(complete.len(), 2);
        // The last job step still stops before the post section.
        let test = cut_step(POST_LOG, &post_steps(), 7).unwrap();
        assert_eq!(test.last().unwrap(), "test result: ok.");
    }

    #[test]
    fn post_steps_fall_back_when_counts_differ() {
        let mut steps = post_steps();
        steps.insert(5, named(16, "Post Run extra@v1", "03:52:37", "03:52:37"));
        assert_eq!(cut_step(POST_LOG, &steps, 13), None);
    }

    #[test]
    fn cleans_escape_sequences_and_controls() {
        assert_eq!(
            clean("\u{1b}[36;1mbash scripts/x.sh\u{1b}[0m"),
            "bash scripts/x.sh"
        );
        assert_eq!(clean("a\u{1b}]0;evil title\u{7}b"), "ab");
        assert_eq!(
            clean("\u{1b}]8;;https://x.test\u{1b}\\link\u{1b}]8;;\u{1b}\\"),
            "link"
        );
        assert_eq!(clean("\u{1b}[2J\u{1b}[Hclear"), "clear");
        assert_eq!(clean("tab\there\r"), "tab    here");
        assert_eq!(clean("bell\u{7} \u{9b}31mred"), "bell red");
        assert_eq!(clean("plain ✓ texto"), "plain ✓ texto");
        let log = "2026-11-25T10:00:00.0Z \u{1b}[31merror\u{1b}[0m";
        assert_eq!(whole_job(log), vec!["error"]);
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
