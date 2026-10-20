//! What the PRs screens ask the workers for, and what comes back.

use crate::github::Run;
use crate::pulls::model::{PrDetail, PrGroup, PrRef, PrSummary};

/// Work the PRs screens request from the GitHub worker.
#[allow(clippy::enum_variant_names)] // every request is a read so far; writes come next
pub enum PrCommand {
    /// Read both lists again.
    FetchList,
    /// Read one PR's detail again.
    FetchDetail(PrRef),
    /// Read one Actions run that a check points at.
    FetchRun { repo: String, run_id: u64 },
}

/// What the PR workers send back.
pub enum PrMsg {
    /// One list's search finished.
    List { group: PrGroup, result: Result<Vec<PrSummary>, String> },
    /// A PR's detail finished reading.
    Detail { pr: PrRef, result: Result<Box<PrDetail>, String> },
    /// The run a check pointed at finished reading.
    Run(Result<Box<Run>, String>),
}
