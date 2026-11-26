//! Filter typed in the dashboard (`/`).
//!
//! `#123` keeps only runs of pull request 123 (applied when runs are fetched).
//! Any other text is a search over the tree: only the rows whose name or
//! branch contains it stay, with their parents (see [`crate::tree::search`]).

use crate::github::RunDto;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunFilter {
    /// Pull request number (`#152`).
    PullRequest(u64),
}

impl RunFilter {
    /// The run filter in the typed text: `#152`, or `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        let number = text.trim().strip_prefix('#')?.parse().ok()?;
        Some(Self::PullRequest(number))
    }

    pub fn matches(&self, run: &RunDto) -> bool {
        match self {
            Self::PullRequest(number) => run.pull_requests.iter().any(|pr| pr.number == *number),
        }
    }
}

/// The tree search in the typed text, lowercased: `None` when the text is
/// blank or is a `#PR` filter.
pub fn search_text(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty() && RunFilter::parse(text).is_none()).then(|| text.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::PullRequestRef;

    fn run(branch: &str, prs: &[u64]) -> RunDto {
        RunDto {
            id: 1,
            name: None,
            workflow_id: 1,
            head_branch: Some(branch.into()),
            status: None,
            conclusion: None,
            html_url: String::new(),
            run_started_at: None,
            updated_at: None,
            pull_requests: prs
                .iter()
                .map(|&number| PullRequestRef { number })
                .collect(),
        }
    }

    #[test]
    fn parses_input() {
        assert_eq!(RunFilter::parse("  "), None);
        assert_eq!(RunFilter::parse("#152"), Some(RunFilter::PullRequest(152)));
        assert_eq!(RunFilter::parse("Docs/"), None);
        assert_eq!(search_text("  "), None);
        assert_eq!(search_text("#152"), None);
        assert_eq!(search_text(" Docs/ ").as_deref(), Some("docs/"));
        // "#" followed by text is a search, not a PR.
        assert_eq!(search_text("#abc").as_deref(), Some("#abc"));
    }

    #[test]
    fn matches_pull_request() {
        let f = RunFilter::parse("#152").unwrap();
        assert!(f.matches(&run("feat/x", &[152])));
        assert!(!f.matches(&run("feat/x", &[15])));
    }
}
