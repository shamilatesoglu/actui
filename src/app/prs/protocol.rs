//! What the PRs screens ask the workers for, and what comes back.

use crate::github::Run;
use crate::pulls::model::{
    DraftComment, MergeMethod, PrDetail, PrFile, PrGroup, PrRef, PrSummary, ReviewSubmission,
};

/// Work the PRs screens request from the GitHub worker.
pub enum PrCommand {
    /// Read both lists again.
    FetchList,
    /// Read one PR's detail again.
    FetchDetail(PrRef),
    /// Read one Actions run that a check points at.
    FetchRun { repo: String, run_id: u64 },
    /// Read the files one PR changes, with their diffs.
    FetchFiles(PrRef),
    /// Change something on one PR.
    Act { pr: PrRef, action: PrAction },
}

/// A change to a PR.
#[derive(Clone, Debug)]
pub enum PrAction {
    Comment { body: String },
    SetDraft { node_id: String, draft: bool },
    UpdateBranch { head_oid: String },
    RerunFailed { run_ids: Vec<u64> },
    /// Merges the PR, then deletes the named branch if one is given. The two
    /// are answered apart, so a failed deletion doesn't hide the merge.
    Merge { method: MergeMethod, head_oid: String, delete_branch: Option<String> },
    Enqueue { node_id: String, head_oid: String },
    Dequeue { node_id: String },
    /// Posts one line comment on its own, outside any review.
    LineComment { head_oid: String, comment: DraftComment },
    /// Answers a review thread at once; `comment_id` is the thread's first comment.
    Reply { comment_id: u64, body: String },
    SubmitReview { node_id: String, submission: ReviewSubmission },
    ResolveThread { thread_id: String, resolved: bool },
}

/// Which change an answer is about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrActionKind {
    Comment,
    MarkReady,
    ConvertToDraft,
    UpdateBranch,
    RerunFailed,
    Merge,
    /// The branch deletion asked for with a merge, once the merge is in.
    DeleteBranch,
    Enqueue,
    Dequeue,
    LineComment,
    Reply,
    SubmitReview,
    ResolveThread,
    UnresolveThread,
}

impl PrAction {
    pub fn kind(&self) -> PrActionKind {
        match self {
            PrAction::Comment { .. } => PrActionKind::Comment,
            PrAction::SetDraft { draft: false, .. } => PrActionKind::MarkReady,
            PrAction::SetDraft { draft: true, .. } => PrActionKind::ConvertToDraft,
            PrAction::UpdateBranch { .. } => PrActionKind::UpdateBranch,
            PrAction::RerunFailed { .. } => PrActionKind::RerunFailed,
            PrAction::Merge { .. } => PrActionKind::Merge,
            PrAction::Enqueue { .. } => PrActionKind::Enqueue,
            PrAction::Dequeue { .. } => PrActionKind::Dequeue,
            PrAction::LineComment { .. } => PrActionKind::LineComment,
            PrAction::Reply { .. } => PrActionKind::Reply,
            PrAction::SubmitReview { .. } => PrActionKind::SubmitReview,
            PrAction::ResolveThread { resolved: true, .. } => PrActionKind::ResolveThread,
            PrAction::ResolveThread { resolved: false, .. } => PrActionKind::UnresolveThread,
        }
    }
}

impl PrActionKind {
    /// What the status line says once the change went through.
    pub fn done_text(self) -> &'static str {
        match self {
            PrActionKind::Comment => "Comment posted",
            PrActionKind::MarkReady => "Marked ready for review",
            PrActionKind::ConvertToDraft => "Converted to draft",
            PrActionKind::UpdateBranch => "Branch updated",
            PrActionKind::RerunFailed => "Re-running failed checks",
            PrActionKind::Merge => "Merged",
            PrActionKind::DeleteBranch => "Branch deleted",
            PrActionKind::Enqueue => "Added to the merge queue",
            PrActionKind::Dequeue => "Removed from the merge queue",
            PrActionKind::LineComment => "Line comment posted",
            PrActionKind::Reply => "Reply posted",
            PrActionKind::SubmitReview => "Review submitted",
            PrActionKind::ResolveThread => "Thread resolved",
            PrActionKind::UnresolveThread => "Thread unresolved",
        }
    }
}

/// What the PR workers send back.
pub enum PrMsg {
    /// One list's search finished.
    List { group: PrGroup, result: Result<Vec<PrSummary>, String> },
    /// A PR's detail finished reading.
    Detail { pr: PrRef, result: Result<Box<PrDetail>, String> },
    /// The run a check pointed at finished reading.
    Run { run_id: u64, result: Result<Box<Run>, String> },
    /// A PR's changed files finished reading.
    Files { pr: PrRef, result: Result<Vec<PrFile>, String> },
    /// A change to a PR finished.
    ActDone { pr: PrRef, kind: PrActionKind, result: Result<(), String> },
}
