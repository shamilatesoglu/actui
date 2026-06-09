//! Application state and the data reducer (`apply`) that ingests `DataMsg`s and
//! maintains derived state, plus the cross-thread command/event protocol. The
//! keystroke→intent input layer lives in the `input` submodule; the overlay
//! view types, log-viewer model, and protocol enums in their own submodules.

mod input;
mod logs;
mod overlays;
mod protocol;

pub use logs::{LogsView, StepsView};
pub use overlays::*;
pub use protocol::{AnnJob, Command, DataMsg};
pub(crate) use logs::log_content;

use crate::github::{Job, RateLimit, Run, RunState, Step};
use chrono::{DateTime, Utc};
use ratatui::widgets::{ListState, TableState};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Most active runs polled per fast tick (keeps request bursts small).
const MAX_ACTIVE_POLL: usize = 8;
/// Cap on cached job-log blobs so a long session doesn't grow without bound.
const MAX_LOG_CACHE: usize = 40;

/// How long a transient status message stays on screen.
const STATUS_TTL: Duration = Duration::from_secs(4);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Filter {
    All,
    Running,
    Queued,
    Failed,
    Success,
}

impl Filter {
    pub const ALL: [Filter; 5] = [
        Filter::All,
        Filter::Running,
        Filter::Queued,
        Filter::Failed,
        Filter::Success,
    ];
    pub fn label(&self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Running => "Running",
            Filter::Queued => "Queued",
            Filter::Failed => "Failed",
            Filter::Success => "Success",
        }
    }
    fn matches(&self, s: RunState) -> bool {
        match self {
            Filter::All => true,
            Filter::Running => s == RunState::Running,
            Filter::Queued => s == RunState::Queued,
            Filter::Failed => s == RunState::Failure,
            Filter::Success => s == RunState::Success,
        }
    }
}

/// Which pane the keyboard drives in normal mode.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Runs,
    Jobs,
}

#[derive(PartialEq, Eq)]
pub enum Mode {
    Normal,
    Search,
    Help,
    Logs,
    Dispatch,
    Confirm,
    Errors,
    Artifacts,
    Approval,
    Annotations,
    RefPicker,
}

pub struct App {
    pub user: String,
    pub runs: Vec<Run>,
    pub view: Vec<usize>, // indices into `runs`, after filter+search
    pub table_state: TableState,
    pub filter: Filter,
    pub search: String,
    pub mode: Mode,
    pub focus: Focus,
    pub errors: Vec<String>,

    pub jobs: Vec<Job>,
    pub jobs_state: ListState,
    pub jobs_run_id: Option<u64>,
    /// Jobs cached per run id, so re-selecting a run restores them even when
    /// the conditional refetch comes back `304 Not Modified`.
    pub jobs_cache: HashMap<u64, Vec<Job>>,

    pub repos_total: usize,
    pub repos_done: usize,
    pub loading: bool,
    pub rate: Option<RateLimit>,
    /// Seconds remaining on a rate-limit back-off, if any (for the header).
    pub paused_secs: Option<u64>,
    pub last_refresh: Option<DateTime<Utc>>,
    /// (text, is_error, set_at) — expires after STATUS_TTL.
    pub status_msg: Option<(String, bool, Instant)>,
    pub spinner: usize,
    pub should_quit: bool,

    pub dispatch: Option<DispatchState>,
    pub logs: Option<LogsView>,
    /// Completed-job logs cached by job id, so re-opening doesn't re-download.
    pub logs_cache: HashMap<u64, String>,
    /// Live step view for a still-running job (mutually exclusive with `logs`).
    pub steps_view: Option<StepsView>,
    pub artifacts: Option<ArtifactsView>,
    pub approval: Option<ApprovalView>,
    pub annotations: Option<AnnotationsView>,
    /// When set, the next fetched log opens pre-searched for this text — lets the
    /// annotations view jump straight to the offending line.
    pub pending_log_search: Option<String>,
    pub ref_picker: Option<RefPicker>,
    pub pending_action: Option<PendingAction>,

    /// Set by the refresh key; consumed by the main loop.
    pub force_refresh: bool,
    pub pending: Vec<Command>,

    /// Last-seen state per run id, to detect active→terminal transitions and
    /// fire a completion notification. Rebuilt each broad sweep.
    run_states: HashMap<u64, RunState>,
}

impl App {
    pub fn new() -> Self {
        Self {
            user: String::new(),
            runs: Vec::new(),
            view: Vec::new(),
            table_state: TableState::default(),
            filter: Filter::All,
            search: String::new(),
            mode: Mode::Normal,
            focus: Focus::Runs,
            errors: Vec::new(),
            jobs: Vec::new(),
            jobs_state: ListState::default(),
            jobs_run_id: None,
            jobs_cache: HashMap::new(),
            repos_total: 0,
            repos_done: 0,
            loading: true,
            rate: None,
            paused_secs: None,
            last_refresh: None,
            status_msg: None,
            spinner: 0,
            should_quit: false,
            dispatch: None,
            logs: None,
            logs_cache: HashMap::new(),
            steps_view: None,
            artifacts: None,
            approval: None,
            annotations: None,
            pending_log_search: None,
            ref_picker: None,
            pending_action: None,
            force_refresh: false,
            pending: Vec::new(),
            run_states: HashMap::new(),
        }
    }

    pub fn selected_run(&self) -> Option<&Run> {
        let i = self.table_state.selected()?;
        let idx = *self.view.get(i)?;
        self.runs.get(idx)
    }

    fn set_status(&mut self, msg: impl Into<String>, is_err: bool) {
        self.status_msg = Some((msg.into(), is_err, Instant::now()));
    }

    /// Public status notifier for the main loop (e.g. rate-limit feedback).
    pub fn notify(&mut self, msg: impl Into<String>, is_err: bool) {
        self.set_status(msg, is_err);
    }

    /// The active status message, if it hasn't expired.
    pub fn status(&self) -> Option<(&str, bool)> {
        self.status_msg
            .as_ref()
            .filter(|(_, _, at)| at.elapsed() < STATUS_TTL)
            .map(|(m, e, _)| (m.as_str(), *e))
    }

    pub fn apply(&mut self, msg: DataMsg) {
        match msg {
            DataMsg::User(u) => self.user = u,
            DataMsg::Repos(n) => {
                self.repos_total = n;
                self.repos_done = 0;
                self.loading = true;
                self.errors.clear(); // errors reflect the latest sweep only
            }
            DataMsg::Runs { repo, runs } => {
                self.repos_done += 1;
                // Replace any existing runs for this repo with the fresh set.
                self.runs.retain(|r| r.repository.full_name != repo);
                self.runs.extend(runs);
                self.resort();
                self.recompute_view();
                if self.repos_done >= self.repos_total {
                    self.finish_refresh();
                }
            }
            DataMsg::RunsUnchanged => {
                self.repos_done += 1;
                if self.repos_done >= self.repos_total {
                    self.finish_refresh();
                }
            }
            DataMsg::RepoError { repo, err } => {
                self.repos_done += 1;
                self.errors.push(format!("{repo}: {err}"));
                if self.repos_done >= self.repos_total {
                    self.finish_refresh();
                }
            }
            DataMsg::Jobs { run_id, jobs } => {
                self.jobs_cache.insert(run_id, jobs.clone());
                if self.selected_run().map(|r| r.id) == Some(run_id) {
                    self.jobs = jobs;
                    self.jobs_run_id = Some(run_id);
                    if self.jobs_state.selected().is_none() && !self.jobs.is_empty() {
                        self.jobs_state.select(Some(0));
                    }
                    // If we're watching a job's live steps and it just finished,
                    // pull the now-available full text logs.
                    if let Some(sv) = &self.steps_view {
                        let done = self.jobs.iter().any(|j| j.id == sv.job_id && !j.is_running());
                        if done {
                            self.pending.push(Command::FetchLogs {
                                repo: sv.repo.clone(),
                                job_id: sv.job_id,
                                title: format!("{} — {}", sv.repo, sv.job_name),
                            });
                        }
                    }
                }
            }
            DataMsg::Logs { job_id, title, text } => {
                if self.logs_cache.len() >= MAX_LOG_CACHE {
                    self.logs_cache.clear();
                }
                self.logs_cache.insert(job_id, text.clone());
                let mut lv = LogsView::new(title, &text);
                // A jump from the annotations view pre-searches the offending line.
                if let Some(q) = self.pending_log_search.take() {
                    lv.search = q;
                    lv.update_search();
                }
                self.logs = Some(lv);
                self.steps_view = None; // text replaces the live step view
                self.mode = Mode::Logs;
                self.status_msg = None; // clear the "Fetching logs…" notice
            }
            DataMsg::Annotations { run_id, mut items } => {
                let Some(av) = &mut self.annotations else { return };
                if av.run_id != run_id {
                    return;
                }
                // Failures first, then group by job and file for a steady order.
                items.sort_by(|a, b| {
                    a.level
                        .rank()
                        .cmp(&b.level.rank())
                        .then_with(|| a.job_name.cmp(&b.job_name))
                        .then_with(|| a.path.cmp(&b.path))
                        .then_with(|| a.start_line.cmp(&b.start_line))
                });
                av.items = items;
                av.loaded = true;
                if av.state.selected().is_none() && !av.items.is_empty() {
                    av.state.select(Some(0));
                }
            }
            DataMsg::Artifacts { run_id, artifacts } => {
                if let Some(av) = &mut self.artifacts {
                    if av.run_id == run_id {
                        av.items = artifacts;
                        av.loaded = true;
                        if av.state.selected().is_none() && !av.items.is_empty() {
                            av.state.select(Some(0));
                        }
                    }
                }
            }
            DataMsg::PendingDeployments { run_id, items } => {
                let Some(av) = &mut self.approval else { return };
                if av.run_id != run_id {
                    return;
                }
                if items.is_empty() {
                    // No environment gate — this is a fork-PR approval. Drop the
                    // picker and fall back to a simple confirm.
                    let (repo, run_id) = (av.repo.clone(), av.run_id);
                    self.approval = None;
                    let label = self
                        .runs
                        .iter()
                        .find(|r| r.id == run_id)
                        .map(|r| format!("{} #{}", r.workflow_name(), r.run_number))
                        .unwrap_or_default();
                    self.pending_action = Some(PendingAction::Approve { repo, run_id, label });
                    self.mode = Mode::Confirm;
                    return;
                }
                // Pre-select every environment the user is allowed to approve.
                av.selected = items
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.current_user_can_approve)
                    .map(|(i, _)| i)
                    .collect();
                let first = items.iter().position(|p| p.current_user_can_approve).unwrap_or(0);
                av.items = items;
                av.loaded = true;
                av.state.select(Some(first));
            }
            DataMsg::Refs { repo, branches, tags } => {
                let Some(rp) = &mut self.ref_picker else { return };
                if rp.repo != repo {
                    return;
                }
                let mut items: Vec<RefItem> = branches
                    .into_iter()
                    .map(|name| RefItem { name, kind: RefKind::Branch })
                    .collect();
                items.extend(tags.into_iter().map(|name| RefItem { name, kind: RefKind::Tag }));
                rp.items = items;
                rp.loaded = true;
                rp.recompute();
            }
            DataMsg::Workflows { repo, workflows } => {
                if let Some(d) = &mut self.dispatch {
                    if d.repo == repo {
                        d.workflows = workflows;
                        if d.wf_state.selected().is_none() && !d.workflows.is_empty() {
                            d.wf_state.select(Some(0));
                        }
                    }
                }
            }
            DataMsg::WorkflowInputs { repo, dispatchable, inputs } => {
                if let Some(d) = &mut self.dispatch {
                    if d.repo == repo && matches!(d.stage, DispatchStage::EditParams) {
                        d.fields = inputs.into_iter().map(DispatchField::from_input).collect();
                        d.dispatchable = dispatchable;
                        d.loaded = true;
                        d.field_idx = 0;
                    }
                }
            }
            DataMsg::Action(m) => self.set_status(m, false),
            DataMsg::Error(e) => self.set_status(e, true),
            DataMsg::RefreshDone => self.finish_refresh(),
        }
    }

    fn finish_refresh(&mut self) {
        self.loading = false;
        self.last_refresh = Some(Utc::now());
        // Drop cached jobs for runs that fell out of the latest sweep so the
        // cache can't grow without bound over a long session.
        let live: HashSet<u64> = self.runs.iter().map(|r| r.id).collect();
        self.jobs_cache.retain(|id, _| live.contains(id));
        self.detect_run_completions();
        self.recompute_view();
    }

    /// Compare each run's state to the previous sweep; for runs that went from
    /// queued/running to a terminal state, queue a completion notification.
    /// `run_states` is then rebuilt from the current runs (so it self-prunes).
    fn detect_run_completions(&mut self) {
        let mut finished: Vec<(String, RunState)> = Vec::new();
        for r in &self.runs {
            let now = r.state();
            let was_active = matches!(
                self.run_states.get(&r.id),
                Some(RunState::Running | RunState::Queued)
            );
            let terminal = matches!(
                now,
                RunState::Success | RunState::Failure | RunState::Cancelled
            );
            if was_active && terminal {
                let label = format!(
                    "{} · {} #{}",
                    r.repository.full_name,
                    r.workflow_name(),
                    r.run_number
                );
                finished.push((label, now));
            }
        }
        self.run_states = self.runs.iter().map(|r| (r.id, r.state())).collect();

        for (label, state) in finished {
            let (word, failed) = match state {
                RunState::Success => ("succeeded", false),
                RunState::Failure => ("failed", true),
                _ => ("cancelled", true),
            };
            self.pending.push(Command::Notify {
                title: format!("actui — run {word}"),
                body: label.clone(),
                failed,
            });
            self.set_status(format!("{label} {word}"), failed);
        }
    }

    fn resort(&mut self) {
        self.runs.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    }

    pub fn recompute_view(&mut self) {
        let prev_id = self.selected_run().map(|r| r.id);
        let q = self.search.to_lowercase();
        self.view = self
            .runs
            .iter()
            .enumerate()
            .filter(|(_, r)| self.filter.matches(r.state()))
            .filter(|(_, r)| {
                if q.is_empty() {
                    return true;
                }
                fuzzy(&r.repository.full_name, &q)
                    || fuzzy(r.title(), &q)
                    || fuzzy(r.workflow_name(), &q)
                    || fuzzy(r.head_branch.as_deref().unwrap_or(""), &q)
            })
            .map(|(i, _)| i)
            .collect();

        // Preserve selection on the same run if it survived the filter.
        let new_sel = prev_id
            .and_then(|id| self.view.iter().position(|&i| self.runs[i].id == id))
            .or(if self.view.is_empty() { None } else { Some(0) });
        self.table_state.select(new_sel);
        self.sync_jobs_for_selection();
    }

    /// When the selected run changes, show cached jobs immediately (if any) and
    /// request a fresh copy. The cache matters because the conditional refetch
    /// returns `304` for a run we've already loaded — without it the list would
    /// be stuck empty on re-selection.
    fn sync_jobs_for_selection(&mut self) {
        let Some((run_id, repo)) = self
            .selected_run()
            .map(|r| (r.id, r.repository.full_name.clone()))
        else {
            self.jobs.clear();
            self.jobs_run_id = None;
            return;
        };
        if self.jobs_run_id == Some(run_id) {
            return;
        }
        match self.jobs_cache.get(&run_id) {
            Some(cached) => {
                self.jobs = cached.clone();
                self.jobs_run_id = Some(run_id);
                if self.jobs_state.selected().is_none() && !self.jobs.is_empty() {
                    self.jobs_state.select(Some(0));
                }
            }
            None => {
                self.jobs.clear();
                self.jobs_state.select(None);
                self.jobs_run_id = None;
            }
        }
        // Always refresh (304 keeps the cache; Modified updates it).
        self.pending.push(Command::FetchJobs { repo, run_id });
    }

    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let mut running = 0;
        let mut queued = 0;
        let mut failed = 0;
        let mut success = 0;
        for r in &self.runs {
            match r.state() {
                RunState::Running => running += 1,
                RunState::Queued => queued += 1,
                RunState::Failure => failed += 1,
                RunState::Success => success += 1,
                _ => {}
            }
        }
        (running, queued, failed, success)
    }

    pub fn tick(&mut self) {
        if self.loading {
            self.spinner = (self.spinner + 1) % 10;
        }
        // Expire stale status messages so the footer never shows outdated info.
        if let Some((_, _, at)) = &self.status_msg {
            if at.elapsed() >= STATUS_TTL {
                self.status_msg = None;
            }
        }
    }

    // -- polling, refresh queueing & accessors -------------------------------

    /// Broad sweep: re-list every watched repo's runs, plus the selected run's
    /// jobs. Runs on the slow cadence.
    pub fn queue_broad_refresh(&mut self) {
        if self.loading {
            return;
        }
        self.pending.push(Command::Refresh);
        self.queue_selected_jobs();
    }

    /// True when any run (not just the selected one) is queued/in progress.
    pub fn any_run_active(&self) -> bool {
        self.runs
            .iter()
            .any(|r| matches!(r.state(), RunState::Running | RunState::Queued))
    }

    /// Focused poll for the fast cadence: refresh jobs for the active runs (up
    /// to `MAX_ACTIVE_POLL`), so every in-flight run stays current — not only the
    /// selected one. Bounded to keep the request burst small.
    pub fn queue_focused_refresh(&mut self) {
        let active: Vec<(String, u64)> = self
            .runs
            .iter()
            .filter(|r| matches!(r.state(), RunState::Running | RunState::Queued))
            .take(MAX_ACTIVE_POLL)
            .map(|r| (r.repository.full_name.clone(), r.id))
            .collect();
        for (repo, run_id) in active {
            self.pending.push(Command::FetchJobs { repo, run_id });
        }
    }

    /// When the live step view is open on a still-running job, the run whose
    /// jobs feed it. Used to poll that one run on a tight cadence so steps
    /// advance near real-time instead of on the slower focused interval.
    pub fn live_steps_run(&self) -> Option<(String, u64)> {
        if self.mode != Mode::Logs {
            return None;
        }
        let sv = self.steps_view.as_ref()?;
        let run_id = self.jobs_run_id?;
        let running = self.jobs.iter().any(|j| j.id == sv.job_id && j.is_running());
        running.then(|| (sv.repo.clone(), run_id))
    }

    /// Queue a jobs fetch for the run feeding the open live step view, if any.
    pub fn queue_live_steps_refresh(&mut self) {
        if let Some((repo, run_id)) = self.live_steps_run() {
            self.pending.push(Command::FetchJobs { repo, run_id });
        }
    }

    fn queue_selected_jobs(&mut self) {
        if let Some(run) = self.selected_run() {
            self.pending.push(Command::FetchJobs {
                repo: run.repository.full_name.clone(),
                run_id: run.id,
            });
        }
    }

    /// Steps of the job the step-view is pinned to (read live from `jobs`).
    pub fn steps_view_steps(&self) -> &[Step] {
        match &self.steps_view {
            Some(sv) => self
                .jobs
                .iter()
                .find(|j| j.id == sv.job_id)
                .map(|j| j.steps.as_slice())
                .unwrap_or(&[]),
            None => &[],
        }
    }

    pub fn selected_job(&self) -> Option<&Job> {
        let i = self.jobs_state.selected()?;
        self.jobs.get(i)
    }
}

/// Case-insensitive subsequence match: every char of `needle` (already
/// lowercased) appears in `haystack`, in order. Empty needle always matches.
fn fuzzy(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let mut chars = haystack.chars().flat_map(char::to_lowercase);
    needle.chars().all(|nc| chars.any(|hc| hc == nc))
}

fn list_move(state: &mut ListState, len: usize, delta: i32) {
    if len == 0 {
        return;
    }
    let cur = state.selected().unwrap_or(0) as i32;
    let next = (cur + delta).clamp(0, len as i32 - 1) as usize;
    state.select(Some(next));
}

/// Jump a list selection to the top or bottom; a no-op when the list is empty.
fn list_jump(state: &mut ListState, len: usize, top: bool) {
    if len > 0 {
        state.select(Some(if top { 0 } else { len - 1 }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_with(id: u64, status: &str, conclusion: Option<&str>) -> Run {
        Run {
            id,
            name: Some("CI".into()),
            display_title: "fix".into(),
            head_branch: Some("main".into()),
            run_number: 7,
            event: "push".into(),
            status: status.into(),
            conclusion: conclusion.map(|c| c.into()),
            html_url: "http://x".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            run_started_at: None,
            actor: None,
            repository: crate::github::RunRepo { full_name: "org/api".into() },
        }
    }

    #[test]
    fn notifies_only_on_active_to_terminal_transition() {
        let mut app = App::new();
        // First sweep: run is in progress — establishes the baseline, no notify.
        app.runs = vec![run_with(1, "in_progress", None)];
        app.detect_run_completions();
        assert!(app.pending.is_empty(), "no notification on first sighting");

        // Second sweep: same run now succeeded → one completion notification.
        app.runs = vec![run_with(1, "completed", Some("success"))];
        app.detect_run_completions();
        assert!(
            app.pending
                .iter()
                .any(|c| matches!(c, Command::Notify { failed: false, .. })),
            "expected a success notification"
        );

        // Third sweep: unchanged terminal state → no repeat notification.
        app.pending.clear();
        app.detect_run_completions();
        assert!(app.pending.is_empty(), "terminal state shouldn't re-notify");
    }

    #[test]
    fn needs_approval_only_for_held_runs() {
        assert!(run_with(1, "waiting", None).needs_approval()); // env deployment gate
        assert!(run_with(1, "action_required", None).needs_approval()); // fork PR
        assert!(run_with(1, "completed", Some("action_required")).needs_approval());
        assert!(!run_with(1, "queued", None).needs_approval());
        assert!(!run_with(1, "in_progress", None).needs_approval());
        assert!(!run_with(1, "completed", Some("success")).needs_approval());
    }

    #[test]
    fn no_notification_for_run_already_finished_when_first_seen() {
        let mut app = App::new();
        // A run we never watched as active appears already-successful: stay quiet.
        app.runs = vec![run_with(2, "completed", Some("success"))];
        app.detect_run_completions();
        assert!(app.pending.is_empty());
    }

    #[test]
    fn fuzzy_subsequence_matching() {
        assert!(fuzzy("org/api-server", "apisrv")); // chars in order, gaps ok
        assert!(fuzzy("Deploy", "dep")); // case-insensitive
        assert!(fuzzy("anything", "")); // empty needle matches
        assert!(!fuzzy("api", "apii")); // needle longer / not a subsequence
        assert!(!fuzzy("build", "lib")); // out of order
    }

    fn job(id: u64) -> Job {
        job_full(id, "build", Some("success"))
    }

    fn job_full(id: u64, name: &str, conclusion: Option<&str>) -> Job {
        Job {
            id,
            name: name.into(),
            status: "completed".into(),
            html_url: String::new(),
            check_run_url: format!("https://api.github.com/repos/org/api/check-runs/{id}"),
            conclusion: conclusion.map(Into::into),
            started_at: None,
            completed_at: None,
            steps: Vec::new(),
        }
    }

    #[test]
    fn recompute_view_preserves_selection_by_run_id() {
        let mut app = App::new();
        app.runs = vec![
            run_with(1, "in_progress", None),         // Running
            run_with(2, "completed", Some("success")), // Success
            run_with(3, "completed", Some("failure")), // Failure
        ];
        app.recompute_view();
        // Select a run that is NOT at index 0, so preservation is distinguishable
        // from the fall-back-to-0 path.
        app.table_state.select(Some(1));
        assert_eq!(app.selected_run().unwrap().id, 2);

        // (a) A filter that keeps the selected run: selection stays on run 2.
        app.filter = Filter::Success;
        app.recompute_view();
        assert_eq!(app.selected_run().unwrap().id, 2);

        // (b) A filter that excludes it: selection falls back to the first row.
        app.filter = Filter::Running;
        app.recompute_view();
        assert_eq!(app.selected_run().unwrap().id, 1);

        // (c) A filter matching nothing: selection becomes None.
        app.filter = Filter::Queued;
        app.recompute_view();
        assert!(app.selected_run().is_none());
    }

    #[test]
    fn sync_jobs_serves_cache_on_hit_and_clears_on_miss() {
        // Hit: a cached run shows its jobs immediately and still queues a refetch.
        let mut hit = App::new();
        hit.runs = vec![run_with(1, "in_progress", None)];
        hit.jobs_cache.insert(1, vec![job(10)]);
        hit.recompute_view();
        assert_eq!(hit.jobs.len(), 1);
        assert_eq!(hit.jobs_run_id, Some(1));
        assert!(hit
            .pending
            .iter()
            .any(|c| matches!(c, Command::FetchJobs { run_id: 1, .. })));

        // Miss: an uncached run clears the list but still queues a refetch.
        let mut miss = App::new();
        miss.runs = vec![run_with(2, "in_progress", None)];
        miss.recompute_view();
        assert!(miss.jobs.is_empty());
        assert_eq!(miss.jobs_state.selected(), None);
        assert_eq!(miss.jobs_run_id, None);
        assert!(miss
            .pending
            .iter()
            .any(|c| matches!(c, Command::FetchJobs { run_id: 2, .. })));
    }

    #[test]
    fn finish_refresh_prunes_jobs_cache_to_live_runs() {
        let mut app = App::new();
        app.jobs_cache.insert(1, vec![job(1)]);
        app.jobs_cache.insert(2, vec![job(2)]);
        app.jobs_cache.insert(3, vec![job(3)]);
        // Only runs 1 and 3 survive the latest sweep.
        app.runs = vec![
            run_with(1, "completed", Some("success")),
            run_with(3, "in_progress", None),
        ];
        app.finish_refresh();
        assert!(app.jobs_cache.contains_key(&1));
        assert!(!app.jobs_cache.contains_key(&2)); // dropped: no longer in runs
        assert!(app.jobs_cache.contains_key(&3));
    }

    #[test]
    fn annotation_targets_prefer_failed_jobs_then_fall_back_to_all() {
        let mut app = App::new();
        app.runs = vec![run_with(1, "completed", Some("failure"))];
        app.recompute_view();
        app.jobs_run_id = Some(1);
        app.jobs = vec![
            job_full(10, "build", Some("success")),
            job_full(11, "test", Some("failure")),
            job_full(12, "lint", Some("timed_out")),
        ];
        // From Runs focus we inspect only the failed/timed-out jobs.
        let t = app.annotation_targets();
        let ids: Vec<u64> = t.iter().map(|j| j.job_id).collect();
        assert_eq!(ids, vec![11, 12]);

        // With no failures, fall back to every completed job (warnings/notices).
        app.jobs = vec![job_full(20, "build", Some("success"))];
        let t = app.annotation_targets();
        assert_eq!(t.iter().map(|j| j.job_id).collect::<Vec<_>>(), vec![20]);

        // A running job has no check-run yet, so it's never a target.
        app.jobs = vec![Job { status: "in_progress".into(), ..job_full(30, "deploy", None) }];
        assert!(app.annotation_targets().is_empty());
    }

    #[test]
    fn logs_message_applies_pending_search() {
        let mut app = App::new();
        app.pending_log_search = Some("boom".into());
        app.apply(DataMsg::Logs {
            job_id: 1,
            title: "t".into(),
            text: "all good\n2026-01-01T00:00:00.0000000Z ##[error]boom here\n".into(),
        });
        let lv = app.logs.expect("a logs view");
        assert_eq!(lv.search, "boom");
        assert_eq!(lv.matches.len(), 1, "the pre-search should locate the error line");
        assert!(app.pending_log_search.is_none(), "the pending search is consumed once");
    }

    #[test]
    fn cycle_filter_wraps_both_directions() {
        let mut app = App::new();
        assert_eq!(app.filter, Filter::All);
        app.cycle_filter(-1);
        assert_eq!(app.filter, Filter::Success); // wrap backward past the start
        app.cycle_filter(1);
        assert_eq!(app.filter, Filter::All); // wrap forward past the end
        for _ in 0..Filter::ALL.len() {
            app.cycle_filter(1);
        }
        assert_eq!(app.filter, Filter::All); // a full lap returns to the start
    }

    fn dispatch_field(name: &str, required: bool, kind: FieldKind) -> DispatchField {
        DispatchField { name: name.into(), description: String::new(), required, kind }
    }

    fn loaded_dispatch(fields: Vec<DispatchField>, git_ref: &str) -> DispatchState {
        DispatchState {
            repo: "org/api".into(),
            workflows: Vec::new(),
            wf_state: ListState::default(),
            stage: DispatchStage::EditParams,
            git_ref: git_ref.into(),
            fetched_ref: git_ref.into(),
            workflow_id: 42,
            workflow_path: ".github/workflows/ci.yml".into(),
            fields,
            loaded: true,
            dispatchable: true,
            field_idx: 0,
        }
    }

    #[test]
    fn submit_dispatch_omits_empty_optionals_and_includes_the_rest() {
        let mut app = App::new();
        app.dispatch = Some(loaded_dispatch(
            vec![
                dispatch_field("required_field", true, FieldKind::Text { value: "val".into(), numeric: false }),
                dispatch_field("optional_empty", false, FieldKind::Text { value: String::new(), numeric: false }),
                dispatch_field("flag", false, FieldKind::Bool(true)),
                dispatch_field("env", false, FieldKind::Choice { options: vec!["dev".into(), "prod".into()], idx: 1 }),
            ],
            "main",
        ));
        app.submit_dispatch();

        let inputs = app
            .pending
            .iter()
            .find_map(|c| match c {
                Command::Dispatch { workflow_id: 42, inputs, git_ref, .. } if git_ref == "main" => Some(inputs.clone()),
                _ => None,
            })
            .expect("a Dispatch command should be queued");
        assert_eq!(inputs.get("required_field").map(String::as_str), Some("val"));
        assert!(!inputs.contains_key("optional_empty")); // empty optional → omitted so the workflow default applies
        assert_eq!(inputs.get("flag").map(String::as_str), Some("true"));
        assert_eq!(inputs.get("env").map(String::as_str), Some("prod"));
        assert!(app.dispatch.is_none()); // form closed on a successful submit
    }

    #[test]
    fn submit_dispatch_rejects_missing_required_and_blank_ref() {
        // A blank required field is rejected with a per-field message; nothing queued.
        let mut app = App::new();
        app.dispatch = Some(loaded_dispatch(
            vec![dispatch_field("token", true, FieldKind::Text { value: String::new(), numeric: false })],
            "main",
        ));
        app.submit_dispatch();
        assert!(app.dispatch.is_some()); // form stays open
        assert!(!app.pending.iter().any(|c| matches!(c, Command::Dispatch { .. })));
        let (msg, is_err) = app.status().expect("an error status");
        assert!(is_err && msg.contains("token"));

        // A blank ref is rejected too.
        let mut app = App::new();
        app.dispatch = Some(loaded_dispatch(Vec::new(), "   "));
        app.submit_dispatch();
        assert!(!app.pending.iter().any(|c| matches!(c, Command::Dispatch { .. })));
        let (msg, is_err) = app.status().expect("an error status");
        assert!(is_err && msg.to_lowercase().contains("ref"));
    }
}
