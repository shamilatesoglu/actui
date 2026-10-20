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
    #[allow(dead_code)] // nothing mutates yet
    pub node_id: String,
    pub title: String,
    pub author: String,
    pub updated_at: DateTime<Utc>,
    pub draft: bool,
    pub checks: ChecksState,
    pub review: ReviewDecision,
}

/// Everything the PR view shows about one pull request.
#[derive(Clone, Debug)]
pub struct PrDetail {
    /// GraphQL node id, used by mutations.
    #[allow(dead_code)] // nothing mutates yet
    pub node_id: String,
    pub title: String,
    pub body: String,
    pub author: String,
    #[allow(dead_code)] // the Files and Conversation tabs link to it
    pub url: String,
    pub head: HeadRef,
    pub base_branch: String,
    pub draft: bool,
    pub labels: Vec<String>,
    pub merge_status: MergeStatus,
    pub reviewers: Vec<Reviewer>,
    pub checks: Vec<Check>,
    pub comments: Vec<Comment>,
    pub reviews: Vec<Review>,
    pub threads: Vec<ReviewThread>,
    /// The methods the repo allows, the viewer's default first.
    #[allow(dead_code)] // the merge action offers them
    pub merge_methods: Vec<MergeMethod>,
    pub queue: QueueState,
}

#[derive(Clone, Debug)]
pub struct HeadRef {
    pub branch: String,
    /// `owner/name` of the repo the branch lives in; a fork differs from the PR's repo.
    pub repo: String,
    #[allow(dead_code)] // review comments are placed on this commit
    pub oid: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MergeStatus {
    Clean,
    Behind,
    Blocked,
    Conflicts,
    Unstable,
    Draft,
    /// GitHub is still working it out, or sent a state this doesn't know.
    Unknown,
}

impl MergeStatus {
    /// From GraphQL's `mergeStateStatus`.
    pub fn from_api(status: &str) -> Self {
        match status {
            "CLEAN" | "HAS_HOOKS" => MergeStatus::Clean,
            "BEHIND" => MergeStatus::Behind,
            "BLOCKED" => MergeStatus::Blocked,
            "DIRTY" => MergeStatus::Conflicts,
            "UNSTABLE" => MergeStatus::Unstable,
            "DRAFT" => MergeStatus::Draft,
            _ => MergeStatus::Unknown,
        }
    }

    pub fn words(self) -> &'static str {
        match self {
            MergeStatus::Clean => "ready to merge",
            MergeStatus::Behind => "behind the base branch",
            MergeStatus::Blocked => "blocked",
            MergeStatus::Conflicts => "has conflicts",
            MergeStatus::Unstable => "some checks are not passing",
            MergeStatus::Draft => "draft",
            MergeStatus::Unknown => "checking",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Reviewer {
    /// A user's login, or a team's slug.
    pub login: String,
    pub verdict: Verdict,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// Asked to review, hasn't yet.
    Requested,
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Check {
    pub name: String,
    pub state: CheckState,
    pub target: CheckTarget,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CheckState {
    Pending,
    Passed,
    Failed,
    Skipped,
    Neutral,
}

/// Where a check leads when opened.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CheckTarget {
    ActionsRun { run_id: u64 },
    Link(String),
    Nothing,
}

#[allow(dead_code)] // the Conversation tab shows them
#[derive(Clone, Debug)]
pub struct Comment {
    /// The REST `databaseId`.
    pub id: u64,
    pub author: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

#[allow(dead_code)] // the Conversation tab shows them
#[derive(Clone, Debug)]
pub struct Review {
    pub author: String,
    pub verdict: Verdict,
    pub body: String,
    pub submitted_at: DateTime<Utc>,
}

#[allow(dead_code)] // the Files and Conversation tabs show them
#[derive(Clone, Debug)]
pub struct ReviewThread {
    pub id: String,
    pub resolved: bool,
    pub outdated: bool,
    pub path: String,
    /// `None` when the lines the thread was left on are gone.
    pub line: Option<u32>,
    pub start_line: Option<u32>,
    pub side: Side,
    pub comments: Vec<Comment>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    /// The name the REST API takes for `merge_method`.
    #[allow(dead_code)] // the merge action sends it
    pub fn api_name(self) -> &'static str {
        match self {
            MergeMethod::Merge => "merge",
            MergeMethod::Squash => "squash",
            MergeMethod::Rebase => "rebase",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QueueState {
    /// The base branch has no merge queue.
    NoQueue,
    NotQueued,
    Queued { position: u32 },
}

/// Why a blocked PR can't merge, in the order a person would fix things.
pub fn blocked_reason(d: &PrDetail) -> Option<String> {
    if d.merge_status != MergeStatus::Blocked {
        return None;
    }
    let reviewer = |v| d.reviewers.iter().any(|r| r.verdict == v);
    let reason = if reviewer(Verdict::ChangesRequested) {
        "changes requested"
    } else if d.checks.iter().any(|c| c.state == CheckState::Failed) {
        "checks failing"
    } else if !reviewer(Verdict::Approved) {
        "review required"
    } else {
        "branch protection"
    };
    Some(reason.to_string())
}
