//! Pull request data types.

use chrono::{DateTime, Utc};

/// A pull request, identified by repository and number.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct PrRef {
    /// `owner/name`.
    pub repo: String,
    pub number: u64,
}

impl PrRef {
    pub fn url(&self) -> String {
        format!("https://github.com/{}/pull/{}", self.repo, self.number)
    }

    /// Short form, `owner/name#12`.
    pub fn label(&self) -> String {
        format!("{}#{}", self.repo, self.number)
    }
}

/// A list of pull requests the PRs screen shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrGroup {
    ReviewRequested,
    Authored,
}

impl PrGroup {
    pub const ALL: [PrGroup; 2] = [PrGroup::ReviewRequested, PrGroup::Authored];

    /// The GitHub search query that lists this group.
    pub fn query(self) -> &'static str {
        match self {
            PrGroup::ReviewRequested => {
                "is:pr is:open archived:false review-requested:@me sort:updated-desc"
            }
            PrGroup::Authored => "is:pr is:open archived:false author:@me sort:updated-desc",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            PrGroup::ReviewRequested => "Review requested",
            PrGroup::Authored => "Opened by you",
        }
    }
}

/// Combined state of a pull request's latest commit checks.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ChecksState {
    Pending,
    Passing,
    Failing,
    NoChecks,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
    /// The repo doesn't require reviews.
    NotRequired,
}

/// One row of a pull request list.
#[derive(Clone, Debug)]
pub struct PrSummary {
    pub pr: PrRef,
    /// GraphQL node id, used by mutations.
    pub node_id: String,
    pub title: String,
    pub author: String,
    pub updated_at: DateTime<Utc>,
    pub draft: bool,
    pub checks: ChecksState,
    pub review: ReviewDecision,
}
