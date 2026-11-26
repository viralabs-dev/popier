//! Minimal GitHub REST client for the Actions endpoints popier needs.
//!
//! Responses are cached by URL with their ETag. A conditional request that
//! comes back `304 Not Modified` does not count against the rate limit.

use std::collections::HashMap;
use std::process::Command;
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{StatusCode, header};
use serde::Deserialize;
use serde::de::DeserializeOwned;

const API: &str = "https://api.github.com";

/// Raw JSON body kept alongside its ETag.
type Body = std::sync::Arc<Vec<u8>>;

#[derive(Debug, Clone, Deserialize)]
pub struct RunsPage {
    pub workflow_runs: Vec<RunDto>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunDto {
    pub id: u64,
    pub name: Option<String>,
    pub workflow_id: u64,
    pub head_branch: Option<String>,
    pub status: Option<String>,
    pub conclusion: Option<String>,
    pub html_url: String,
    /// When the run (or its latest attempt) started.
    #[serde(default)]
    pub run_started_at: Option<String>,
    /// Last change; for a finished run, roughly when it ended.
    #[serde(default)]
    pub updated_at: Option<String>,
    /// PRs whose head is this run's commit (empty for runs not tied to a PR).
    #[serde(default)]
    pub pull_requests: Vec<PullRequestRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PullRequestRef {
    pub number: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowsPage {
    pub workflows: Vec<WorkflowDto>,
}

/// A workflow definition. Its `name` is stable, unlike a run's title.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowDto {
    pub id: u64,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JobsPage {
    pub jobs: Vec<JobDto>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JobDto {
    pub id: u64,
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub html_url: Option<String>,
    #[serde(default)]
    pub steps: Vec<StepDto>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StepDto {
    pub number: u32,
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// Rate limit as reported by the last response.
#[derive(Debug, Clone, Copy, Default)]
pub struct RateLimit {
    pub remaining: Option<u32>,
    pub limit: Option<u32>,
}

pub struct Client {
    http: reqwest::Client,
    /// Same client without automatic redirects, for log downloads.
    http_no_redirect: reqwest::Client,
    token: String,
    cache: Mutex<HashMap<String, (String, Body)>>,
    rate: Mutex<RateLimit>,
}

impl Client {
    pub fn new(token: String) -> Result<Self> {
        let agent = concat!("popier/", env!("CARGO_PKG_VERSION"));
        let http = reqwest::Client::builder().user_agent(agent).build()?;
        let http_no_redirect = reqwest::Client::builder()
            .user_agent(agent)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            http,
            http_no_redirect,
            token,
            cache: Mutex::new(HashMap::new()),
            rate: Mutex::new(RateLimit::default()),
        })
    }

    pub fn rate_limit(&self) -> RateLimit {
        *self.rate.lock().unwrap()
    }

    /// Recent runs of a repository, newest first.
    pub async fn list_runs(
        &self,
        slug: &str,
        branch: Option<&str>,
        per_page: u8,
    ) -> Result<Vec<RunDto>> {
        let mut url = format!("{API}/repos/{slug}/actions/runs?per_page={per_page}");
        if let Some(branch) = branch {
            url.push_str("&branch=");
            url.push_str(&urlencode(branch));
        }
        Ok(self.get_json::<RunsPage>(&url).await?.workflow_runs)
    }

    /// Workflow definitions of a repository (id and stable name).
    pub async fn list_workflows(&self, slug: &str) -> Result<Vec<WorkflowDto>> {
        let url = format!("{API}/repos/{slug}/actions/workflows?per_page=100");
        Ok(self.get_json::<WorkflowsPage>(&url).await?.workflows)
    }

    /// Jobs (with steps) of the latest attempt of a run.
    pub async fn list_jobs(&self, slug: &str, run_id: u64) -> Result<Vec<JobDto>> {
        let url = format!("{API}/repos/{slug}/actions/runs/{run_id}/jobs?per_page=100");
        Ok(self.get_json::<JobsPage>(&url).await?.jobs)
    }

    /// Plain-text log of a job. GitHub answers with a redirect to a signed
    /// storage URL; that URL is fetched without the token, so the token never
    /// leaves api.github.com.
    pub async fn job_log(&self, slug: &str, job_id: u64) -> Result<String> {
        let url = format!("{API}/repos/{slug}/actions/jobs/{job_id}/logs");
        let resp = self
            .http_no_redirect
            .get(&url)
            .bearer_auth(&self.token)
            .header(header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .with_context(|| format!("request failed: {url}"))?;
        self.record_rate_limit(resp.headers());

        let status = resp.status();
        if status.is_redirection() {
            let location = resp
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| anyhow!("log redirect without a location"))?
                .to_owned();
            let log = self.http.get(&location).send().await?.error_for_status()?;
            return Ok(log.text().await?);
        }
        if status.is_success() {
            return Ok(resp.text().await?);
        }
        let text = resp.text().await.unwrap_or_default();
        bail!("GitHub API {}: {}", status.as_u16(), api_message(&text));
    }

    async fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T> {
        let cached = self.cache.lock().unwrap().get(url).cloned();

        let mut req = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .header(header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some((etag, _)) = &cached {
            req = req.header(header::IF_NONE_MATCH, etag);
        }

        let resp = req
            .send()
            .await
            .with_context(|| format!("request failed: {url}"))?;
        self.record_rate_limit(resp.headers());

        let body = match resp.status() {
            StatusCode::NOT_MODIFIED => cached
                .map(|(_, body)| body)
                .ok_or_else(|| anyhow!("304 without a cached body"))?,
            status if status.is_success() => {
                let etag = resp
                    .headers()
                    .get(header::ETAG)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                let body = std::sync::Arc::new(resp.bytes().await?.to_vec());
                if let Some(etag) = etag {
                    self.cache
                        .lock()
                        .unwrap()
                        .insert(url.to_owned(), (etag, body.clone()));
                }
                body
            }
            status => {
                let text = resp.text().await.unwrap_or_default();
                bail!("GitHub API {}: {}", status.as_u16(), api_message(&text));
            }
        };

        serde_json::from_slice(&body).with_context(|| format!("unexpected response from {url}"))
    }

    fn record_rate_limit(&self, headers: &header::HeaderMap) {
        let read = |name: &str| headers.get(name)?.to_str().ok()?.parse().ok();
        let mut rate = self.rate.lock().unwrap();
        if let Some(remaining) = read("x-ratelimit-remaining") {
            rate.remaining = Some(remaining);
        }
        if let Some(limit) = read("x-ratelimit-limit") {
            rate.limit = Some(limit);
        }
    }
}

/// The `message` field of a GitHub error body, or the raw body.
fn api_message(text: &str) -> String {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_owned))
        .unwrap_or_else(|| text.to_owned())
}

/// Finds a token: `GITHUB_TOKEN`, then `GH_TOKEN`, then `gh auth token`.
pub fn resolve_token() -> Result<String> {
    for var in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(token) = std::env::var(var)
            && !token.trim().is_empty()
        {
            return Ok(token.trim().to_owned());
        }
    }
    let output = Command::new("gh")
        .args(["auth", "token"])
        .output()
        .context("no GITHUB_TOKEN set and the `gh` CLI was not found")?;
    if !output.status.success() {
        bail!(
            "no GITHUB_TOKEN set and `gh auth token` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let token = String::from_utf8(output.stdout)?.trim().to_owned();
    if token.is_empty() {
        bail!("`gh auth token` returned nothing; run `gh auth login`");
    }
    Ok(token)
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_branch_names() {
        assert_eq!(urlencode("main"), "main");
        assert_eq!(urlencode("feat/x y"), "feat%2Fx%20y");
    }

    #[test]
    fn parses_runs_payload() {
        let json = r#"{"total_count":1,"workflow_runs":[{"id":7,"name":"CI","workflow_id":3,
            "head_branch":"main","status":"completed","conclusion":"success",
            "html_url":"https://github.com/o/r/actions/runs/7","extra":true}]}"#;
        let page: RunsPage = serde_json::from_str(json).unwrap();
        assert_eq!(page.workflow_runs[0].workflow_id, 3);
    }

    #[test]
    fn parses_workflows_payload() {
        let json = r#"{"total_count":1,"workflows":[{"id":3,"name":"CI","path":".github/workflows/ci.yml","state":"active"}]}"#;
        let page: WorkflowsPage = serde_json::from_str(json).unwrap();
        assert_eq!(page.workflows[0].name, "CI");
    }

    #[test]
    fn parses_jobs_payload() {
        let json = r#"{"total_count":1,"jobs":[{"id":11,"name":"build","status":"in_progress",
            "conclusion":null,"html_url":null,
            "steps":[{"number":1,"name":"Checkout","status":"completed","conclusion":"success"}]}]}"#;
        let page: JobsPage = serde_json::from_str(json).unwrap();
        assert_eq!(page.jobs[0].steps[0].name, "Checkout");
    }
}
