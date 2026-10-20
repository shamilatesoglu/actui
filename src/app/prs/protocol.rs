//! What the PRs screens ask the workers for, and what comes back.

use crate::pulls::model::{PrGroup, PrSummary};

/// Work the PRs screens request from the GitHub worker.
pub enum PrCommand {
    /// Read both lists again.
    FetchList,
}

/// What the PR workers send back.
pub enum PrMsg {
    /// One list's search finished.
    List { group: PrGroup, result: Result<Vec<PrSummary>, String> },
}
