//! Unified status for runs, jobs and steps.

/// Status derived from GitHub's `status` + `conclusion` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Running,
    Queued,
    Success,
    Failure,
    Skipped,
    Cancelled,
}

impl Status {
    /// Maps GitHub's `status` and `conclusion` fields (shared by runs, jobs and steps).
    pub fn from_github(status: &str, conclusion: Option<&str>) -> Self {
        match status {
            "in_progress" => Self::Running,
            "completed" => match conclusion {
                Some("success") => Self::Success,
                Some("skipped" | "neutral") => Self::Skipped,
                Some("cancelled") => Self::Cancelled,
                _ => Self::Failure,
            },
            // queued, waiting, requested, pending
            _ => Self::Queued,
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Running => "●",
            Self::Queued => "○",
            Self::Success => "✓",
            Self::Failure => "✗",
            Self::Skipped => "–",
            Self::Cancelled => "⊘",
        }
    }

    /// ANSI color code used by the plain-text printer.
    pub fn ansi(self) -> &'static str {
        match self {
            Self::Running => "33",
            Self::Success => "32",
            Self::Failure => "31",
            Self::Queued | Self::Skipped | Self::Cancelled => "90",
        }
    }

    /// True when the item needs a look: still going, or it did not end well.
    pub fn needs_attention(self) -> bool {
        matches!(self, Self::Running | Self::Queued | Self::Failure)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_github_pairs() {
        assert_eq!(Status::from_github("in_progress", None), Status::Running);
        assert_eq!(Status::from_github("queued", None), Status::Queued);
        assert_eq!(Status::from_github("waiting", None), Status::Queued);
        assert_eq!(
            Status::from_github("completed", Some("success")),
            Status::Success
        );
        assert_eq!(
            Status::from_github("completed", Some("neutral")),
            Status::Skipped
        );
        assert_eq!(
            Status::from_github("completed", Some("cancelled")),
            Status::Cancelled
        );
        assert_eq!(
            Status::from_github("completed", Some("timed_out")),
            Status::Failure
        );
        assert_eq!(Status::from_github("completed", None), Status::Failure);
    }
}
