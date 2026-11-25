//! Flattens a snapshot into display rows, shared by the TUI and the plain printer.

use std::collections::HashSet;

use crate::snapshot::Snapshot;
use crate::status::Status;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowKey {
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
}

/// What the user folded by hand.
#[derive(Debug, Clone, Default)]
pub struct Folds {
    pub collapsed_repos: HashSet<String>,
    pub collapsed_workflows: HashSet<u64>,
}

pub fn build(snapshot: &Snapshot, folds: &Folds) -> Vec<Row> {
    let mut rows = Vec::new();
    for repo in &snapshot.repos {
        let repo_open = !folds.collapsed_repos.contains(&repo.slug);
        let repo_url = format!("https://github.com/{}/actions", repo.slug);
        rows.push(Row {
            key: RowKey::Repo(repo.slug.clone()),
            depth: 0,
            status: None,
            text: repo.name.clone(),
            detail: None,
            is_error: false,
            url: Some(repo_url),
            foldable: true,
            expanded: repo_open,
        });
        if !repo_open {
            continue;
        }

        let workflows = match &repo.workflows {
            Err(err) => {
                rows.push(note(
                    &repo.slug,
                    0,
                    format!("error: {}", first_line(err)),
                    true,
                ));
                continue;
            }
            Ok(w) if w.is_empty() => {
                rows.push(note(&repo.slug, 0, "(no runs)".into(), false));
                continue;
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
                detail: Some(format!("({})", wf.branch.as_deref().unwrap_or("-"))),
                is_error: false,
                url: Some(wf.url.clone()),
                foldable: true,
                expanded: open,
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
                            });
                        }
                    }
                }
            }
        }
    }
    rows
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
    }
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
        if let Some(status) = row.status {
            out.push_str(&paint(status.ansi(), status.icon()));
            out.push(' ');
        }
        let detail_first = matches!(row.key, RowKey::Step(..));
        if detail_first && let Some(d) = &row.detail {
            out.push_str(&paint("90", d));
            out.push(' ');
        }
        match (row.depth, row.is_error) {
            (0, _) => out.push_str(&paint("1", &row.text)),
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
    use crate::snapshot::{JobView, RepoView, StepView, WorkflowView};

    fn sample() -> Snapshot {
        Snapshot {
            repos: vec![
                RepoView {
                    name: "api".into(),
                    slug: "o/api".into(),
                    workflows: Ok(vec![
                        WorkflowView {
                            run_id: 1,
                            name: "CI".into(),
                            branch: Some("main".into()),
                            status: Status::Failure,
                            url: String::new(),
                            jobs: Some(Ok(vec![JobView {
                                name: "test".into(),
                                status: Status::Failure,
                                url: None,
                                steps: vec![StepView {
                                    number: 1,
                                    name: "Run tests".into(),
                                    status: Status::Failure,
                                }],
                            }])),
                        },
                        WorkflowView {
                            run_id: 2,
                            name: "Lint".into(),
                            branch: None,
                            status: Status::Success,
                            url: String::new(),
                            jobs: None,
                        },
                    ]),
                },
                RepoView {
                    name: "web".into(),
                    slug: "o/web".into(),
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
    fn respects_folds() {
        let mut folds = Folds::default();
        folds.collapsed_workflows.insert(1);
        folds.collapsed_repos.insert("o/web".into());
        let rows = build(&sample(), &folds);
        assert_eq!(rows.len(), 4); // api, CI, Lint, web
    }

    #[test]
    fn plain_output_without_color() {
        let text = render_plain(&build(&sample(), &Folds::default()), false);
        assert!(text.contains("    ✗ CI (main)\n"));
        assert!(text.contains("            ✗ 1. Run tests\n"));
        assert!(text.contains("    error: GitHub API 404: Not Found\n"));
    }
}
