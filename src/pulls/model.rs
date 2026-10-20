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
    pub node_id: String,
    pub title: String,
    pub body: String,
    pub author: String,
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
    pub merge_methods: Vec<MergeMethod>,
    pub queue: QueueState,
}

#[derive(Clone, Debug)]
pub struct HeadRef {
    pub branch: String,
    /// `owner/name` of the repo the branch lives in; a fork differs from the PR's repo.
    pub repo: String,
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

#[derive(Clone, Debug)]
pub struct Comment {
    /// The REST `databaseId`.
    pub id: u64,
    pub author: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct Review {
    pub author: String,
    pub verdict: Verdict,
    pub body: String,
    pub submitted_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct ReviewThread {
    pub id: String,
    pub resolved: bool,
    pub outdated: bool,
    pub path: String,
    /// The lines the thread was left on; `None` when they are gone.
    pub lines: Option<LineSpan>,
    pub side: Side,
    pub comments: Vec<Comment>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    /// The name the APIs take.
    pub fn api_name(self) -> &'static str {
        match self {
            Side::Left => "LEFT",
            Side::Right => "RIGHT",
        }
    }
}

/// The lines a review comment covers, on one side of the diff.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineSpan {
    One(u32),
    Range(LineRange),
}

/// Two or more lines, `start` before `end`. Only `LineSpan::new` builds one,
/// so the order always holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LineRange {
    start: u32,
    end: u32,
}

impl LineSpan {
    /// The span between two lines, in either order.
    pub fn new(a: u32, b: u32) -> LineSpan {
        match a.cmp(&b) {
            std::cmp::Ordering::Equal => LineSpan::One(a),
            std::cmp::Ordering::Less => LineSpan::Range(LineRange { start: a, end: b }),
            std::cmp::Ordering::Greater => LineSpan::Range(LineRange { start: b, end: a }),
        }
    }

    /// The last line, which is where GitHub shows the comment.
    pub fn end(self) -> u32 {
        match self {
            LineSpan::One(line) | LineSpan::Range(LineRange { end: line, .. }) => line,
        }
    }

    /// The first line of a range; a single line has none.
    pub fn start(self) -> Option<u32> {
        match self {
            LineSpan::One(_) => None,
            LineSpan::Range(range) => Some(range.start),
        }
    }

    /// `12`, or `11-12` for a range.
    pub fn label(self) -> String {
        match self {
            LineSpan::One(line) => line.to_string(),
            LineSpan::Range(LineRange { start, end }) => format!("{start}-{end}"),
        }
    }
}

/// A line comment written into a pending review and not sent yet.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DraftComment {
    pub path: String,
    pub side: Side,
    pub span: LineSpan,
    pub body: String,
}

impl DraftComment {
    /// `src/a.rs:11-12`, with `(old)` for the removed side.
    pub fn place(&self) -> String {
        let old = if self.side == Side::Left { " (old)" } else { "" };
        format!("{}:{}{old}", self.path, self.span.label())
    }
}

/// A reply to a review thread, written into a pending review and not sent yet.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DraftReply {
    pub thread_id: String,
    pub body: String,
}

/// The verdict a review is submitted with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReviewEvent {
    Approve,
    RequestChanges,
    Comment,
}

impl ReviewEvent {
    /// In the order the verdict picker lists them.
    pub const ALL: [ReviewEvent; 3] = [ReviewEvent::Approve, ReviewEvent::RequestChanges, ReviewEvent::Comment];

    /// The name GraphQL's `PullRequestReviewEvent` takes.
    pub fn api_name(self) -> &'static str {
        match self {
            ReviewEvent::Approve => "APPROVE",
            ReviewEvent::RequestChanges => "REQUEST_CHANGES",
            ReviewEvent::Comment => "COMMENT",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            ReviewEvent::Approve => "Approve",
            ReviewEvent::RequestChanges => "Request changes",
            ReviewEvent::Comment => "Comment",
        }
    }
}

/// Everything a review sends at once.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ReviewSubmission {
    /// The commit the review is placed on: the head when its first draft was
    /// written, so the drafts' line numbers match, or the current head for a
    /// verdict without drafts.
    pub head_oid: String,
    pub event: ReviewEvent,
    pub body: String,
    pub comments: Vec<DraftComment>,
    pub replies: Vec<DraftReply>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    /// The name the REST API takes for `merge_method`.
    pub fn api_name(self) -> &'static str {
        match self {
            MergeMethod::Merge => "merge",
            MergeMethod::Squash => "squash",
            MergeMethod::Rebase => "rebase",
        }
    }

    /// What the merge box calls it.
    pub fn title(self) -> &'static str {
        match self {
            MergeMethod::Merge => "Create a merge commit",
            MergeMethod::Squash => "Squash and merge",
            MergeMethod::Rebase => "Rebase and merge",
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

/// How a pull request changes one file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileStatus {
    Added,
    Removed,
    Modified,
    Renamed,
    Copied,
    Changed,
    Unchanged,
}

impl FileStatus {
    /// Reads the `status` of a file from the REST API. Anything unknown counts
    /// as a plain change.
    pub fn parse(s: &str) -> Self {
        match s {
            "added" => FileStatus::Added,
            "removed" => FileStatus::Removed,
            "modified" => FileStatus::Modified,
            "renamed" => FileStatus::Renamed,
            "copied" => FileStatus::Copied,
            "unchanged" => FileStatus::Unchanged,
            _ => FileStatus::Changed,
        }
    }

    pub fn letter(self) -> char {
        match self {
            FileStatus::Added => 'A',
            FileStatus::Removed => 'D',
            FileStatus::Modified => 'M',
            FileStatus::Renamed => 'R',
            FileStatus::Copied => 'C',
            FileStatus::Changed => 'M',
            FileStatus::Unchanged => '=',
        }
    }
}

/// One file a pull request changes.
#[derive(Clone, Debug)]
pub struct PrFile {
    pub path: String,
    /// Where a renamed file was before.
    pub previous_path: Option<String>,
    pub status: FileStatus,
    pub additions: u32,
    pub deletions: u32,
    /// The unified diff. GitHub leaves it out for binary and very large files.
    pub patch: Option<String>,
}

impl PrFile {
    /// The path as shown: `old → new` for a renamed or copied file.
    pub fn display_path(&self) -> String {
        match (&self.previous_path, self.status) {
            (Some(old), FileStatus::Renamed | FileStatus::Copied) => format!("{old} → {}", self.path),
            _ => self.path.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_span_is_one_line_or_a_sorted_range() {
        assert_eq!(LineSpan::new(12, 12), LineSpan::One(12));
        assert_eq!(LineSpan::new(12, 10), LineSpan::new(10, 12));
        assert_eq!(LineSpan::new(12, 10).label(), "10-12");
        assert_eq!((LineSpan::new(12, 10).start(), LineSpan::new(12, 10).end()), (Some(10), 12));
        assert_eq!(LineSpan::One(3).start(), None);
        let draft = DraftComment { path: "a.rs".into(), side: Side::Left, span: LineSpan::new(4, 2), body: String::new() };
        assert_eq!(draft.place(), "a.rs:2-4 (old)");
    }
}
