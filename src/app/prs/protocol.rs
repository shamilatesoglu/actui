//! What the PRs screens ask the workers for, and what comes back.

use crate::pulls::model::{PrDetail, PrGroup, PrRef, PrSummary};

/// Work the PRs screens request from the GitHub worker.
pub enum PrCommand {
    /// Read both lists again.
    FetchList,
    /// Read one PR's detail again.
    FetchDetail(PrRef),
}

/// What the PR workers send back.
pub enum PrMsg {
    /// One list's search finished.
    List { group: PrGroup, result: Result<Vec<PrSummary>, String> },
    /// A PR's detail finished reading.
    Detail { pr: PrRef, result: Result<Box<PrDetail>, String> },
}
