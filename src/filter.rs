//! Run filter typed in the dashboard (`/`): part of a branch name, or `#123` for a PR.

use crate::github::RunDto;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunFilter {
    /// Case-insensitive substring of the head branch.
    Branch(String),
    /// Pull request number (`#152`).
    PullRequest(u64),
}

impl RunFilter {
    /// Parses the text typed by the user. Blank text means no filter.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        if let Some(number) = text.strip_prefix('#').and_then(|n| n.parse().ok()) {
            return Some(Self::PullRequest(number));
        }
        Some(Self::Branch(text.to_lowercase()))
    }

    pub fn matches(&self, run: &RunDto) -> bool {
        match self {
            Self::Branch(part) => run
                .head_branch
                .as_deref()
                .is_some_and(|b| b.to_lowercase().contains(part)),
            Self::PullRequest(number) => run.pull_requests.iter().any(|pr| pr.number == *number),
        }
    }
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
        assert_eq!(
            RunFilter::parse("Docs/"),
            Some(RunFilter::Branch("docs/".into()))
        );
        // "#" followed by text is a branch search, not a PR.
        assert_eq!(
            RunFilter::parse("#abc"),
            Some(RunFilter::Branch("#abc".into()))
        );
    }

    #[test]
    fn matches_branch_part_case_insensitive() {
        let f = RunFilter::parse("REFS").unwrap();
        assert!(f.matches(&run("docs/refs-e-ancoras", &[])));
        assert!(!f.matches(&run("main", &[])));
    }

    #[test]
    fn matches_pull_request() {
        let f = RunFilter::parse("#152").unwrap();
        assert!(f.matches(&run("feat/x", &[152])));
        assert!(!f.matches(&run("feat/x", &[15])));
    }
}
