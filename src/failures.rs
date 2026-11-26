//! `popier once --logs`: the end of the log of every failing step, printed
//! after the tree, so a CI failure can be read without opening the dashboard.

use futures::future::join_all;

use crate::github::Client;
use crate::logs::{self, StepSpan};
use crate::snapshot::{JobView, Snapshot};
use crate::status::Status;

/// Default number of log lines shown per failing step.
pub const DEFAULT_LINES: usize = 30;

/// One failing step (or a failing job with no failing step) to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// `api › CI › test › 2. Run tests`
    pub title: String,
    pub slug: String,
    pub job_id: u64,
    /// Step number and the spans of every step of the job; `None` for the
    /// whole job.
    pub step: Option<(u32, Vec<StepSpan>)>,
}

fn spans(job: &JobView) -> Vec<StepSpan> {
    job.steps
        .iter()
        .map(|s| StepSpan {
            number: s.number,
            name: s.name.clone(),
            started: s.started_at.clone(),
            completed: s.completed_at.clone(),
            skipped: s.status == Status::Skipped,
        })
        .collect()
}

/// Every failed job of the snapshot whose jobs were fetched, with its first
/// failing step.
pub fn collect(snapshot: &Snapshot) -> Vec<Failure> {
    let mut out = Vec::new();
    for repo in &snapshot.repos {
        let Ok(workflows) = &repo.workflows else {
            continue;
        };
        for wf in workflows {
            let Some(Ok(jobs)) = &wf.jobs else {
                continue;
            };
            for job in jobs.iter().filter(|j| j.status == Status::Failure) {
                let step = job.steps.iter().find(|s| s.status == Status::Failure);
                let mut title = format!("{} › {} › {}", repo.name, wf.name, job.name);
                if let Some(step) = step {
                    title.push_str(&format!(" › {}. {}", step.number, step.name));
                }
                out.push(Failure {
                    title,
                    slug: repo.slug.clone(),
                    job_id: job.id,
                    step: step.map(|s| (s.number, spans(job))),
                });
            }
        }
    }
    out
}

/// The last `n` lines of the failure: the step's own lines, else the end of
/// the job log.
pub fn lines_of(log: &str, failure: &Failure, n: usize) -> Vec<String> {
    let step = failure.step.as_ref().and_then(|(number, spans)| {
        logs::cut_step(log, spans, *number).or_else(|| {
            let span = spans.iter().find(|s| s.number == *number)?;
            logs::slice_step(log, span.started.as_deref(), span.completed.as_deref())
        })
    });
    logs::tail(step.unwrap_or_else(|| logs::whole_job(log)), n)
}

/// Downloads the logs (concurrently) and renders a section per failure.
pub async fn render(client: &Client, failures: &[Failure], n: usize, color: bool) -> String {
    let logs = join_all(failures.iter().map(|f| client.job_log(&f.slug, f.job_id))).await;
    let paint = |code: &str, s: &str| {
        if color {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_owned()
        }
    };
    let mut out = String::new();
    for (failure, log) in failures.iter().zip(logs) {
        out.push('\n');
        out.push_str(&paint("1;31", &format!("── ✗ {}", failure.title)));
        out.push('\n');
        match log {
            Ok(log) => {
                for line in lines_of(&log, failure, n) {
                    let error = line.starts_with("##[error]");
                    let line = line.strip_prefix("##[error]").unwrap_or(&line);
                    out.push_str("   ");
                    if error {
                        out.push_str(&paint("31", line));
                    } else {
                        out.push_str(line);
                    }
                    out.push('\n');
                }
            }
            Err(err) => {
                out.push_str(&paint("31", &format!("   cannot load log: {err:#}")));
                out.push('\n');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{RepoView, StepView, WorkflowView};

    fn snap() -> Snapshot {
        let step = |number, name: &str, status| StepView {
            number,
            name: name.into(),
            status,
            started_at: None,
            completed_at: None,
        };
        let job = |id, name: &str, status, steps| JobView {
            id,
            name: name.into(),
            status,
            url: None,
            steps,
        };
        Snapshot {
            repos: vec![RepoView {
                name: "api".into(),
                slug: "o/api".into(),
                group: None,
                workflows: Ok(vec![WorkflowView {
                    workflow_id: 1,
                    run_id: 1,
                    name: "CI".into(),
                    branch: Some("main".into()),
                    status: Status::Failure,
                    url: String::new(),
                    started_at: None,
                    updated_at: None,
                    jobs: Some(Ok(vec![
                        job(
                            10,
                            "test",
                            Status::Failure,
                            vec![
                                step(1, "Set up job", Status::Success),
                                step(2, "Run tests", Status::Failure),
                            ],
                        ),
                        job(11, "lint", Status::Success, vec![]),
                        job(12, "deploy", Status::Failure, vec![]),
                    ])),
                }]),
            }],
        }
    }

    #[test]
    fn collects_failed_jobs_and_their_step() {
        let failures = collect(&snap());
        let titles: Vec<&str> = failures.iter().map(|f| f.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["api › CI › test › 2. Run tests", "api › CI › deploy"]
        );
        assert_eq!(failures[0].step.as_ref().unwrap().0, 2);
        assert!(failures[1].step.is_none());
    }

    #[test]
    fn falls_back_to_the_end_of_the_job() {
        let failure = &collect(&snap())[1];
        let log = "2026-11-25T10:00:00.0Z a\n2026-11-25T10:00:01.0Z b\n2026-11-25T10:00:02.0Z c";
        assert_eq!(lines_of(log, failure, 2), vec!["b", "c"]);
    }
}
