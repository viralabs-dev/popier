//! Point-in-time view of every watched repository: repo → workflow → job → step.

use std::collections::{HashMap, HashSet};

use futures::future::join_all;

use crate::config::{Config, ExpandMode, RepoConfig};
use crate::filter::RunFilter;
use crate::github::{Client, JobDto, RunDto};
use crate::status::Status;

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub repos: Vec<RepoView>,
}

#[derive(Debug, Clone)]
pub struct RepoView {
    pub name: String,
    pub slug: String,
    /// Latest run of each workflow, or the error that prevented loading them.
    pub workflows: Result<Vec<WorkflowView>, String>,
}

#[derive(Debug, Clone)]
pub struct WorkflowView {
    pub workflow_id: u64,
    pub run_id: u64,
    pub name: String,
    pub branch: Option<String>,
    pub status: Status,
    pub url: String,
    /// `None` when jobs were not fetched (collapsed workflow).
    pub jobs: Option<Result<Vec<JobView>, String>>,
}

#[derive(Debug, Clone)]
pub struct JobView {
    pub id: u64,
    pub name: String,
    pub status: Status,
    pub url: Option<String>,
    pub steps: Vec<StepView>,
}

#[derive(Debug, Clone)]
pub struct StepView {
    pub number: u32,
    pub name: String,
    pub status: Status,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// Decides which workflows get their jobs fetched.
#[derive(Debug, Clone, Default)]
pub struct ExpandRequest {
    /// Expand everything, regardless of status.
    pub all: bool,
    /// Run ids the user explicitly expanded in the TUI.
    pub pinned: HashSet<u64>,
    /// Only consider runs matching this filter (typed with `/` in the TUI).
    pub filter: Option<RunFilter>,
}

impl ExpandRequest {
    pub fn from_mode(mode: ExpandMode) -> Self {
        Self {
            all: mode == ExpandMode::All,
            pinned: HashSet::new(),
            filter: None,
        }
    }

    fn wants(&self, run_id: u64, status: Status) -> bool {
        self.all || status.needs_attention() || self.pinned.contains(&run_id)
    }
}

/// Fetches all repositories concurrently; results keep the config order.
pub async fn take(client: &Client, config: &Config, expand: &ExpandRequest) -> Snapshot {
    let repos = join_all(
        config
            .repos
            .iter()
            .map(|repo| load_repo(client, config, repo, expand)),
    )
    .await;
    Snapshot { repos }
}

async fn load_repo(
    client: &Client,
    config: &Config,
    repo: &RepoConfig,
    expand: &ExpandRequest,
) -> RepoView {
    let (runs, definitions) = futures::join!(
        client.list_runs(
            &repo.slug,
            repo.branch.as_deref(),
            config.settings.runs_per_repo,
        ),
        client.list_workflows(&repo.slug),
    );
    // Stable workflow names by id. If this call fails, fall back to run titles.
    let names: HashMap<u64, String> = definitions
        .map(|defs| defs.into_iter().map(|d| (d.id, d.name)).collect())
        .unwrap_or_default();
    let names = &names;

    let workflows = match runs {
        Err(err) => Err(format!("{err:#}")),
        Ok(runs) => {
            let runs = match &expand.filter {
                Some(filter) => runs.into_iter().filter(|r| filter.matches(r)).collect(),
                None => runs,
            };
            let latest = latest_per_workflow(runs);
            let views = latest.into_iter().map(|run| async move {
                let status = run_status(&run);
                let jobs = if expand.wants(run.id, status) {
                    Some(
                        client
                            .list_jobs(&repo.slug, run.id)
                            .await
                            .map(|jobs| jobs.into_iter().map(job_view).collect())
                            .map_err(|err| format!("{err:#}")),
                    )
                } else {
                    None
                };
                WorkflowView {
                    workflow_id: run.workflow_id,
                    run_id: run.id,
                    name: workflow_label(&run, names),
                    branch: run.head_branch.clone(),
                    status,
                    url: run.html_url.clone(),
                    jobs,
                }
            });
            let mut views = join_all(views).await;
            views.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(views)
        }
    };

    RepoView {
        name: repo.display_name().to_owned(),
        slug: repo.slug.clone(),
        workflows,
    }
}

/// Stable label for a run: the workflow's own name, not the run title.
fn workflow_label(run: &RunDto, names: &HashMap<u64, String>) -> String {
    names
        .get(&run.workflow_id)
        .cloned()
        .or_else(|| run.name.clone())
        .unwrap_or_else(|| format!("workflow {}", run.workflow_id))
}

/// Keeps the newest run of each workflow.
pub fn latest_per_workflow(runs: Vec<RunDto>) -> Vec<RunDto> {
    let mut latest: Vec<RunDto> = Vec::new();
    for run in runs {
        match latest.iter_mut().find(|r| r.workflow_id == run.workflow_id) {
            Some(existing) if existing.id >= run.id => {}
            Some(existing) => *existing = run,
            None => latest.push(run),
        }
    }
    latest
}

fn run_status(run: &RunDto) -> Status {
    Status::from_github(
        run.status.as_deref().unwrap_or("queued"),
        run.conclusion.as_deref(),
    )
}

fn job_view(job: JobDto) -> JobView {
    JobView {
        status: Status::from_github(&job.status, job.conclusion.as_deref()),
        id: job.id,
        name: job.name,
        url: job.html_url,
        steps: job
            .steps
            .into_iter()
            .map(|s| StepView {
                status: Status::from_github(&s.status, s.conclusion.as_deref()),
                number: s.number,
                name: s.name,
                started_at: s.started_at,
                completed_at: s.completed_at,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: u64, workflow_id: u64, name: &str) -> RunDto {
        RunDto {
            id,
            name: Some(name.into()),
            workflow_id,
            head_branch: Some("main".into()),
            status: Some("completed".into()),
            conclusion: Some("success".into()),
            html_url: String::new(),
            pull_requests: Vec::new(),
        }
    }

    #[test]
    fn keeps_newest_run_per_workflow() {
        let runs = vec![
            run(5, 1, "CI"),
            run(9, 2, "Deploy"),
            run(7, 1, "CI"),
            run(3, 2, "Deploy"),
        ];
        let latest = latest_per_workflow(runs);
        let ids: Vec<u64> = latest.iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![7, 9]);
    }

    #[test]
    fn labels_use_workflow_name() {
        let mut names = HashMap::new();
        names.insert(1, "Release".to_string());
        assert_eq!(
            workflow_label(&run(5, 1, "release v2.4.2 ← tag"), &names),
            "Release"
        );
        assert_eq!(workflow_label(&run(6, 2, "CI"), &names), "CI");
    }

    #[test]
    fn expands_attention_and_pinned() {
        let mut req = ExpandRequest::from_mode(ExpandMode::Attention);
        assert!(req.wants(1, Status::Failure));
        assert!(!req.wants(1, Status::Success));
        req.pinned.insert(1);
        assert!(req.wants(1, Status::Success));
        assert!(ExpandRequest::from_mode(ExpandMode::All).wants(2, Status::Success));
    }
}
