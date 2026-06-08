//! The command/event protocol connecting the async GitHub workers to the
//! synchronous UI loop. `DataMsg` flows worker → UI (consumed by `App::apply`);
//! `Command` flows UI → worker (drained by the main loop's `dispatch_commands`).

use crate::github::{Artifact, Job, PendingDeployment, Run, WfInput, Workflow};
use std::collections::HashMap;

/// Messages flowing from async workers into the UI.
pub enum DataMsg {
    User(String),
    Repos(usize),
    Runs { repo: String, runs: Vec<Run> },
    /// A repo's runs were unchanged (304) — count it done, keep existing data.
    RunsUnchanged,
    RepoError { repo: String, err: String },
    Jobs { run_id: u64, jobs: Vec<Job> },
    Logs { job_id: u64, title: String, text: String },
    Workflows { repo: String, workflows: Vec<Workflow> },
    WorkflowInputs { repo: String, dispatchable: bool, inputs: Vec<WfInput> },
    Artifacts { run_id: u64, artifacts: Vec<Artifact> },
    /// Environments gating a run's deployment, awaiting review.
    PendingDeployments { run_id: u64, items: Vec<PendingDeployment> },
    /// Branches and tags for a repo, for the dispatch ref picker.
    Refs { repo: String, branches: Vec<String>, tags: Vec<String> },
    Action(String),
    Error(String),
    RefreshDone,
}

/// Work requested by the UI, executed by the main loop on the async runtime.
pub enum Command {
    Refresh,
    FetchJobs { repo: String, run_id: u64 },
    FetchLogs { repo: String, job_id: u64, title: String },
    FetchWorkflows { repo: String },
    FetchWorkflowInputs { repo: String, path: String, git_ref: String },
    FetchArtifacts { repo: String, run_id: u64 },
    DownloadArtifact { repo: String, artifact_id: u64, name: String },
    Dispatch { repo: String, workflow_id: u64, git_ref: String, inputs: HashMap<String, String> },
    Cancel { repo: String, run_id: u64 },
    Rerun { repo: String, run_id: u64 },
    RerunFailed { repo: String, run_id: u64 },
    RerunJob { repo: String, job_id: u64 },
    Approve { repo: String, run_id: u64 },
    FetchPendingDeployments { repo: String, run_id: u64 },
    ReviewDeployments {
        repo: String,
        run_id: u64,
        env_ids: Vec<u64>,
        approve: bool,
        comment: String,
    },
    FetchRefs { repo: String },
    SaveLogs { name: String, content: String },
    OpenUrl(String),
    /// A watched run finished — ring the bell / raise a desktop notification.
    Notify { title: String, body: String, failed: bool },
}
