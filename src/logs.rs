//! Cuts one step out of a job log.
//!
//! GitHub only serves logs per job (plain text, one line per output line,
//! each prefixed with an RFC 3339 timestamp). A step is recovered by keeping
//! the lines whose timestamp falls between the step's `started_at` and
//! `completed_at`. Lines without a timestamp belong to the previous line.

/// Most recent lines shown for a step.
pub const STEP_TAIL: usize = 200;

/// `2026-11-25T22:19:44` part of a timestamped log line, if it has one.
fn line_second(line: &str) -> Option<&str> {
    let stamp = line.split(' ').next()?;
    let b = stamp.as_bytes();
    let looks_like =
        b.len() >= 20 && b[4] == b'-' && b[7] == b'-' && b[10] == b'T' && b[13] == b':';
    looks_like.then(|| &stamp[..19])
}

/// The line without its timestamp prefix.
pub fn strip_timestamp(line: &str) -> &str {
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
