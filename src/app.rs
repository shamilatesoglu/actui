//! Application state, input handling, and the command/event protocol that
//! connects the synchronous UI loop to the async GitHub workers.

use crate::github::{Job, RateLimit, Run, RunState, Step, WfInput, WfInputKind, Workflow};
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::{ListState, TableState};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a transient status message stays on screen.
const STATUS_TTL: Duration = Duration::from_secs(4);

/// Messages flowing from async workers into the UI.
pub enum DataMsg {
    User(String),
    Repos(usize),
    Runs { repo: String, runs: Vec<Run> },
    /// A repo's runs were unchanged (304) — count it done, keep existing data.
    RunsUnchanged,
    RepoError { repo: String, err: String },
    Jobs { run_id: u64, jobs: Vec<Job> },
    Logs { title: String, text: String },
    Workflows { repo: String, workflows: Vec<Workflow> },
    WorkflowInputs { repo: String, dispatchable: bool, inputs: Vec<WfInput> },
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
    Dispatch { repo: String, workflow_id: u64, git_ref: String, inputs: HashMap<String, String> },
    Cancel { repo: String, run_id: u64 },
    Rerun { repo: String, run_id: u64 },
    RerunFailed { repo: String, run_id: u64 },
    OpenUrl(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
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
}

pub enum PendingAction {
    Cancel { repo: String, run_id: u64, label: String },
    Rerun { repo: String, run_id: u64, label: String },
    RerunFailed { repo: String, run_id: u64, label: String },
}

impl PendingAction {
    pub fn prompt(&self) -> String {
        match self {
            PendingAction::Cancel { label, .. } => format!("Cancel run?  {label}"),
            PendingAction::Rerun { label, .. } => format!("Re-run all jobs?  {label}"),
            PendingAction::RerunFailed { label, .. } => format!("Re-run failed jobs?  {label}"),
        }
    }
}

pub enum DispatchStage {
    SelectWorkflow,
    EditParams,
}

/// A single field in the dispatch form, with its live value.
pub enum FieldKind {
    Text { value: String, numeric: bool },
    Bool(bool),
    Choice { options: Vec<String>, idx: usize },
}

pub struct DispatchField {
    pub name: String,
    pub description: String,
    pub required: bool,
    pub kind: FieldKind,
}

impl DispatchField {
    fn from_input(i: WfInput) -> Self {
        let kind = match i.kind {
            WfInputKind::Boolean => FieldKind::Bool(i.default == "true"),
            WfInputKind::Choice(options) => {
                let idx = options.iter().position(|o| *o == i.default).unwrap_or(0);
                FieldKind::Choice { options, idx }
            }
            WfInputKind::Number => FieldKind::Text { value: i.default, numeric: true },
            WfInputKind::Text => FieldKind::Text { value: i.default, numeric: false },
        };
        Self {
            name: i.name,
            description: i.description,
            required: i.required,
            kind,
        }
    }

    /// The value to submit for this field.
    fn value(&self) -> String {
        match &self.kind {
            FieldKind::Text { value, .. } => value.clone(),
            FieldKind::Bool(b) => b.to_string(),
            FieldKind::Choice { options, idx } => options.get(*idx).cloned().unwrap_or_default(),
        }
    }
}

pub struct DispatchState {
    pub repo: String,
    pub workflows: Vec<Workflow>,
    pub wf_state: ListState,
    pub stage: DispatchStage,
    pub git_ref: String,
    /// The ref the current inputs were fetched at (to detect a changed ref).
    pub fetched_ref: String,
    /// Selected workflow (set when entering the form stage).
    pub workflow_id: u64,
    pub workflow_path: String,
    pub fields: Vec<DispatchField>,
    pub loaded: bool,      // inputs fetched
    pub dispatchable: bool, // has a workflow_dispatch trigger
    pub field_idx: usize,  // 0 = ref, 1..=fields.len() = inputs
}

impl DispatchState {
    fn field_count(&self) -> usize {
        self.fields.len() + 1 // + the ref field
    }
}

pub struct LogGroup {
    pub collapsed: bool,
    pub body_count: usize,
    /// Wall-clock seconds for the step, derived from line timestamps.
    pub secs: Option<f64>,
}

/// Live per-step view shown while a job is still running (text logs aren't
/// available from the API until the job completes). Steps are read live from
/// `App::jobs`, so this stays current as the job is polled.
pub struct StepsView {
    pub job_id: u64,
    pub job_name: String,
    pub repo: String,
    pub cursor: usize,
}

pub struct LogsView {
    pub title: String,
    pub lines: Vec<String>,
    /// Group id each line belongs to (None = outside any group).
    pub line_group: Vec<Option<usize>>,
    pub is_header: Vec<bool>,
    pub is_endgroup: Vec<bool>,
    pub groups: Vec<LogGroup>,
    /// Source-line indices currently shown (folds applied).
    pub visible: Vec<usize>,
    /// Cursor position within `visible`.
    pub cursor: usize,
    /// In-log search.
    pub search: String,
    pub searching: bool,
    pub matches: Vec<usize>, // source-line indices containing the query (sorted)
    pub match_idx: Option<usize>,
}

/// Strip a leading BOM, trailing CR/LF, and the ISO timestamp prefix.
fn log_content(raw: &str) -> &str {
    let s = raw
        .trim_start_matches('\u{feff}')
        .trim_end_matches(['\r', '\n']);
    if let Some((first, rest)) = s.split_once(' ') {
        if first.len() >= 20 && first.contains('T') && first.contains(':') {
            return rest;
        }
    }
    s
}

/// Parse the RFC3339 timestamp GitHub prefixes onto each log line.
fn line_time(raw: &str) -> Option<DateTime<Utc>> {
    let tok = raw.trim_start_matches('\u{feff}').split(' ').next()?;
    DateTime::parse_from_rfc3339(tok)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

impl LogsView {
    pub fn new(title: String, text: &str) -> Self {
        let lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
        let n = lines.len();
        let mut line_group = vec![None; n];
        let mut is_header = vec![false; n];
        let mut is_endgroup = vec![false; n];
        let mut groups: Vec<LogGroup> = Vec::new();
        let mut had_problem: Vec<bool> = Vec::new();
        let mut g_start: Vec<Option<DateTime<Utc>>> = Vec::new();
        let mut g_end: Vec<Option<DateTime<Utc>>> = Vec::new();
        let mut current: Option<usize> = None;

        for (i, raw) in lines.iter().enumerate() {
            let c = log_content(raw);
            let t = line_time(raw);
            if c.starts_with("##[group]") {
                let gid = groups.len();
                groups.push(LogGroup { collapsed: true, body_count: 0, secs: None });
                had_problem.push(false);
                g_start.push(t);
                g_end.push(t);
                is_header[i] = true;
                line_group[i] = Some(gid);
                current = Some(gid);
            } else if c.starts_with("##[endgroup]") {
                is_endgroup[i] = true;
                line_group[i] = current;
                if let (Some(g), Some(t)) = (current, t) {
                    g_end[g] = Some(t);
                }
                current = None;
            } else {
                line_group[i] = current;
                if let Some(g) = current {
                    groups[g].body_count += 1;
                    if let Some(t) = t {
                        g_end[g] = Some(t);
                    }
                    if c.starts_with("##[error]") || c.starts_with("##[warning]") {
                        had_problem[g] = true;
                    }
                }
            }
        }
        // Auto-expand groups that contain an error/warning so problems aren't hidden.
        for (g, problem) in had_problem.iter().enumerate() {
            if *problem {
                groups[g].collapsed = false;
            }
        }
        // Per-step duration from first→last line timestamp.
        for g in 0..groups.len() {
            if let (Some(a), Some(b)) = (g_start[g], g_end[g]) {
                groups[g].secs = Some((b - a).num_milliseconds().max(0) as f64 / 1000.0);
            }
        }

        let mut v = Self {
            title,
            lines,
            line_group,
            is_header,
            is_endgroup,
            groups,
            visible: Vec::new(),
            cursor: 0,
            search: String::new(),
            searching: false,
            matches: Vec::new(),
            match_idx: None,
        };
        v.recompute_visible();
        v
    }

    pub fn has_groups(&self) -> bool {
        !self.groups.is_empty()
    }

    fn recompute_visible(&mut self) {
        let prev = self.visible.get(self.cursor).copied();
        self.visible = (0..self.lines.len())
            .filter(|&i| {
                if self.is_endgroup[i] {
                    return false; // endgroup markers never render
                }
                match self.line_group[i] {
                    None => true,
                    Some(g) => self.is_header[i] || !self.groups[g].collapsed,
                }
            })
            .collect();
        self.cursor = prev
            .and_then(|p| self.visible.iter().position(|&i| i == p))
            .unwrap_or_else(|| self.cursor.min(self.visible.len().saturating_sub(1)));
    }

    pub fn move_cursor(&mut self, delta: i32) {
        if self.visible.is_empty() {
            return;
        }
        let c = self.cursor as i32 + delta;
        self.cursor = c.clamp(0, self.visible.len() as i32 - 1) as usize;
    }

    pub fn cursor_to(&mut self, top: bool) {
        self.cursor = if top {
            0
        } else {
            self.visible.len().saturating_sub(1)
        };
    }

    /// Fold/unfold the group the cursor sits in, keeping the cursor on its header.
    pub fn toggle_fold(&mut self) {
        let Some(&src) = self.visible.get(self.cursor) else {
            return;
        };
        let Some(g) = self.line_group[src] else {
            return;
        };
        self.groups[g].collapsed = !self.groups[g].collapsed;
        let header = (0..self.lines.len()).find(|&i| self.is_header[i] && self.line_group[i] == Some(g));
        self.recompute_visible();
        if let Some(h) = header {
            if let Some(pos) = self.visible.iter().position(|&i| i == h) {
                self.cursor = pos;
            }
        }
    }

    pub fn set_all_collapsed(&mut self, collapsed: bool) {
        for g in &mut self.groups {
            g.collapsed = collapsed;
        }
        self.recompute_visible();
    }

    fn current_src(&self) -> Option<usize> {
        self.visible.get(self.cursor).copied()
    }

    /// Move the cursor to a source line, expanding its group if folded.
    fn cursor_to_src(&mut self, src: usize) {
        if let Some(g) = self.line_group[src] {
            if self.groups[g].collapsed {
                self.groups[g].collapsed = false;
                self.recompute_visible();
            }
        }
        if let Some(pos) = self.visible.iter().position(|&i| i == src) {
            self.cursor = pos;
        }
    }

    /// Recompute matches for the current query and jump to the nearest one.
    pub fn update_search(&mut self) {
        self.matches.clear();
        self.match_idx = None;
        if self.search.is_empty() {
            return;
        }
        let q = self.search.to_lowercase();
        for i in 0..self.lines.len() {
            if self.is_endgroup[i] {
                continue;
            }
            if log_content(&self.lines[i]).to_lowercase().contains(&q) {
                self.matches.push(i);
            }
        }
        // Reveal every group that holds a hit so matches are reachable.
        for &i in &self.matches {
            if let Some(g) = self.line_group[i] {
                self.groups[g].collapsed = false;
            }
        }
        self.recompute_visible();
        if self.matches.is_empty() {
            return;
        }
        // Jump to the first match at or after the cursor (wrapping).
        let here = self.current_src().unwrap_or(0);
        let idx = self.matches.iter().position(|&m| m >= here).unwrap_or(0);
        self.match_idx = Some(idx);
        self.cursor_to_src(self.matches[idx]);
    }

    pub fn next_match(&mut self, dir: i32) {
        if self.matches.is_empty() {
            return;
        }
        let n = self.matches.len() as i32;
        let cur = self.match_idx.map(|i| i as i32).unwrap_or(-1);
        let ni = (((cur + dir) % n) + n) % n;
        self.match_idx = Some(ni as usize);
        self.cursor_to_src(self.matches[ni as usize]);
    }

    pub fn clear_search(&mut self) {
        self.search.clear();
        self.searching = false;
        self.matches.clear();
        self.match_idx = None;
    }

    pub fn is_match(&self, src: usize) -> bool {
        self.matches.binary_search(&src).is_ok()
    }
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
    /// Live step view for a still-running job (mutually exclusive with `logs`).
    pub steps_view: Option<StepsView>,
    pub pending_action: Option<PendingAction>,

    /// Set by the refresh key; consumed by the main loop.
    pub force_refresh: bool,
    pub pending: Vec<Command>,
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
            steps_view: None,
            pending_action: None,
            force_refresh: false,
            pending: Vec::new(),
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
            DataMsg::Logs { title, text } => {
                self.logs = Some(LogsView::new(title, &text));
                self.steps_view = None; // text replaces the live step view
                self.mode = Mode::Logs;
                self.status_msg = None; // clear the "Fetching logs…" notice
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
        self.recompute_view();
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
                r.repository.full_name.to_lowercase().contains(&q)
                    || r.title().to_lowercase().contains(&q)
                    || r.workflow_name().to_lowercase().contains(&q)
                    || r.head_branch.as_deref().unwrap_or("").to_lowercase().contains(&q)
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

    // -- input ---------------------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent) {
        // Ctrl-C always quits.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        match self.mode {
            Mode::Normal => self.key_normal(key),
            Mode::Search => self.key_search(key),
            Mode::Help => {
                self.mode = Mode::Normal;
            }
            Mode::Logs => self.key_logs(key),
            Mode::Dispatch => self.key_dispatch(key),
            Mode::Confirm => self.key_confirm(key),
            Mode::Errors => self.mode = Mode::Normal, // any key closes
        }
    }

    fn key_normal(&mut self, key: KeyEvent) {
        self.status_msg = None;
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            // Movement applies to whichever pane is focused.
            KeyCode::Char('j') | KeyCode::Down => self.move_focused(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_focused(-1),
            KeyCode::PageDown => self.move_focused(10),
            KeyCode::PageUp => self.move_focused(-10),
            KeyCode::Char('g') | KeyCode::Home => self.jump_focused(true),
            KeyCode::Char('G') | KeyCode::End => self.jump_focused(false),
            // Pane focus.
            KeyCode::Tab => self.toggle_focus(),
            KeyCode::BackTab => self.toggle_focus(),
            KeyCode::Right => self.focus_jobs(),
            KeyCode::Char('h') | KeyCode::Left | KeyCode::Esc => self.focus = Focus::Runs,
            // Filters.
            KeyCode::Char('1') => self.set_filter(Filter::All),
            KeyCode::Char('2') => self.set_filter(Filter::Running),
            KeyCode::Char('3') => self.set_filter(Filter::Queued),
            KeyCode::Char('4') => self.set_filter(Filter::Failed),
            KeyCode::Char('5') => self.set_filter(Filter::Success),
            KeyCode::Char('[') => self.cycle_filter(-1),
            KeyCode::Char(']') => self.cycle_filter(1),
            KeyCode::Char('/') => self.mode = Mode::Search,
            KeyCode::Char('r') | KeyCode::F(5) => self.force_refresh = true,
            KeyCode::Char('E') => {
                if self.errors.is_empty() {
                    self.set_status("No load errors", false);
                } else {
                    self.mode = Mode::Errors;
                }
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            // Enter / l: drill from runs into jobs, or open the focused job's logs.
            KeyCode::Enter | KeyCode::Char('l') => match self.focus {
                Focus::Runs => self.focus_jobs(),
                Focus::Jobs => self.open_logs(),
            },
            KeyCode::Char('o') => {
                if let Some(run) = self.selected_run() {
                    self.pending.push(Command::OpenUrl(run.html_url.clone()));
                }
            }
            // Always-available: open the selected job's logs regardless of focus.
            KeyCode::Char('L') => self.open_logs(),
            KeyCode::Char('c') => self.confirm_cancel(),
            KeyCode::Char('x') => self.confirm_rerun(false),
            KeyCode::Char('X') => self.confirm_rerun(true),
            KeyCode::Char('d') => self.open_dispatch(),
            _ => {}
        }
    }

    fn move_focused(&mut self, delta: i32) {
        match self.focus {
            Focus::Runs => self.move_sel(delta),
            Focus::Jobs => self.cycle_job(delta),
        }
    }

    fn jump_focused(&mut self, top: bool) {
        match self.focus {
            Focus::Runs => {
                if !self.view.is_empty() {
                    self.select_idx(if top { 0 } else { self.view.len() - 1 });
                }
            }
            Focus::Jobs => {
                if !self.jobs.is_empty() {
                    self.jobs_state
                        .select(Some(if top { 0 } else { self.jobs.len() - 1 }));
                }
            }
        }
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Runs => Focus::Jobs,
            Focus::Jobs => Focus::Runs,
        };
        if self.focus == Focus::Jobs {
            self.ensure_job_selected();
        }
    }

    fn focus_jobs(&mut self) {
        if self.jobs.is_empty() {
            self.set_status("No jobs to focus (still loading?)", true);
            return;
        }
        self.focus = Focus::Jobs;
        self.ensure_job_selected();
    }

    fn ensure_job_selected(&mut self) {
        if self.jobs_state.selected().is_none() && !self.jobs.is_empty() {
            self.jobs_state.select(Some(0));
        }
    }

    fn key_search(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.search.clear();
                self.mode = Mode::Normal;
                self.recompute_view();
            }
            KeyCode::Enter => self.mode = Mode::Normal,
            KeyCode::Backspace => {
                self.search.pop();
                self.recompute_view();
            }
            KeyCode::Char(c) => {
                self.search.push(c);
                self.recompute_view();
            }
            _ => {}
        }
    }

    fn key_logs(&mut self, key: KeyEvent) {
        // Live step view (job still running): no text logs yet.
        if self.steps_view.is_some() {
            let n = self.steps_view_steps().len();
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.steps_view = None;
                    self.mode = Mode::Normal;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    let sv = self.steps_view.as_mut().unwrap();
                    if n > 0 {
                        sv.cursor = (sv.cursor + 1).min(n - 1);
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    let sv = self.steps_view.as_mut().unwrap();
                    sv.cursor = sv.cursor.saturating_sub(1);
                }
                KeyCode::Char('g') | KeyCode::Home => self.steps_view.as_mut().unwrap().cursor = 0,
                KeyCode::Char('G') | KeyCode::End => {
                    self.steps_view.as_mut().unwrap().cursor = n.saturating_sub(1)
                }
                // Force a text-log fetch (works once the blob exists).
                KeyCode::Enter | KeyCode::Char('l') => {
                    let sv = self.steps_view.as_ref().unwrap();
                    let (repo, job_id, title) =
                        (sv.repo.clone(), sv.job_id, format!("{} — {}", sv.repo, sv.job_name));
                    self.set_status("Fetching logs…", false);
                    self.pending.push(Command::FetchLogs { repo, job_id, title });
                }
                _ => {}
            }
            return;
        }

        let Some(lv) = &mut self.logs else {
            self.mode = Mode::Normal;
            return;
        };

        // Search-input sub-mode: keystrokes edit the query.
        if lv.searching {
            match key.code {
                KeyCode::Esc => lv.clear_search(),
                KeyCode::Enter => {
                    lv.searching = false; // keep query/highlights, stop typing
                    if lv.matches.is_empty() && !lv.search.is_empty() {
                        self.set_status("No matches", true);
                    }
                }
                KeyCode::Backspace => {
                    lv.search.pop();
                    lv.update_search();
                }
                KeyCode::Char(c) => {
                    lv.search.push(c);
                    lv.update_search();
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.logs = None;
                self.mode = Mode::Normal;
            }
            KeyCode::Char('j') | KeyCode::Down => lv.move_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => lv.move_cursor(-1),
            KeyCode::PageDown => lv.move_cursor(20),
            KeyCode::PageUp => lv.move_cursor(-20),
            KeyCode::Char('g') | KeyCode::Home => lv.cursor_to(true),
            KeyCode::Char('G') | KeyCode::End => lv.cursor_to(false),
            // Folding.
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Tab => lv.toggle_fold(),
            KeyCode::Char('e') => lv.set_all_collapsed(false), // expand all
            KeyCode::Char('f') => lv.set_all_collapsed(true),  // fold all
            // Search.
            KeyCode::Char('/') => {
                lv.search.clear();
                lv.matches.clear();
                lv.match_idx = None;
                lv.searching = true;
            }
            KeyCode::Char('n') => lv.next_match(1),
            KeyCode::Char('N') => lv.next_match(-1),
            _ => {}
        }
    }

    fn key_dispatch(&mut self, key: KeyEvent) {
        let Some(d) = &mut self.dispatch else {
            self.mode = Mode::Normal;
            return;
        };
        match d.stage {
            DispatchStage::SelectWorkflow => match key.code {
                KeyCode::Esc => {
                    self.dispatch = None;
                    self.mode = Mode::Normal;
                }
                KeyCode::Char('j') | KeyCode::Down => list_move(&mut d.wf_state, d.workflows.len(), 1),
                KeyCode::Char('k') | KeyCode::Up => list_move(&mut d.wf_state, d.workflows.len(), -1),
                KeyCode::Enter => {
                    if let Some(wf) = d.wf_state.selected().and_then(|i| d.workflows.get(i)) {
                        let repo = d.repo.clone();
                        let git_ref = d.git_ref.clone();
                        d.workflow_id = wf.id;
                        d.workflow_path = wf.path.clone();
                        d.stage = DispatchStage::EditParams;
                        d.loaded = false;
                        d.dispatchable = true;
                        d.fields.clear();
                        d.field_idx = 0;
                        d.fetched_ref = git_ref.clone();
                        self.pending.push(Command::FetchWorkflowInputs {
                            repo,
                            path: wf.path.clone(),
                            git_ref,
                        });
                    }
                }
                _ => {}
            },
            DispatchStage::EditParams => self.key_dispatch_form(key),
        }
    }

    fn key_dispatch_form(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Enter {
            self.submit_dispatch();
            return;
        }
        let left_ref;
        {
            let Some(d) = &mut self.dispatch else { return };
            let count = d.field_count();
            let old = d.field_idx;
            Self::dispatch_form_edit(key, d, count);
            // Moved off the ref field after changing it → reload inputs for that ref.
            left_ref = old == 0 && d.field_idx != 0 && d.git_ref != d.fetched_ref;
        }
        if left_ref {
            self.refetch_dispatch_inputs();
        }
    }

    fn refetch_dispatch_inputs(&mut self) {
        let Some(d) = &mut self.dispatch else { return };
        d.loaded = false;
        d.fields.clear();
        d.field_idx = 0;
        d.fetched_ref = d.git_ref.clone();
        let (repo, path, git_ref) = (d.repo.clone(), d.workflow_path.clone(), d.git_ref.clone());
        self.pending.push(Command::FetchWorkflowInputs { repo, path, git_ref });
    }

    fn dispatch_form_edit(key: KeyEvent, d: &mut DispatchState, count: usize) {
        match key.code {
            KeyCode::Esc => d.stage = DispatchStage::SelectWorkflow,
            KeyCode::Tab | KeyCode::Down => d.field_idx = (d.field_idx + 1) % count,
            KeyCode::BackTab | KeyCode::Up => d.field_idx = (d.field_idx + count - 1) % count,
            // Field-specific editing.
            KeyCode::Char(c) => {
                if d.field_idx == 0 {
                    d.git_ref.push(c);
                } else if let Some(f) = d.fields.get_mut(d.field_idx - 1) {
                    match &mut f.kind {
                        FieldKind::Text { value, numeric } => {
                            if !*numeric || c.is_ascii_digit() || c == '.' || c == '-' {
                                value.push(c);
                            }
                        }
                        FieldKind::Bool(b) if c == ' ' => *b = !*b,
                        FieldKind::Choice { options, idx } if c == ' ' && !options.is_empty() => {
                            *idx = (*idx + 1) % options.len();
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Backspace => {
                if d.field_idx == 0 {
                    d.git_ref.pop();
                } else if let Some(FieldKind::Text { value, .. }) =
                    d.fields.get_mut(d.field_idx - 1).map(|f| &mut f.kind)
                {
                    value.pop();
                }
            }
            KeyCode::Left | KeyCode::Right => {
                let fwd = key.code == KeyCode::Right;
                if d.field_idx > 0 {
                    if let Some(f) = d.fields.get_mut(d.field_idx - 1) {
                        match &mut f.kind {
                            FieldKind::Bool(b) => *b = !*b,
                            FieldKind::Choice { options, idx } if !options.is_empty() => {
                                let n = options.len();
                                *idx = if fwd { (*idx + 1) % n } else { (*idx + n - 1) % n };
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn key_confirm(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                if let Some(a) = self.pending_action.take() {
                    match a {
                        PendingAction::Cancel { repo, run_id, .. } => {
                            self.pending.push(Command::Cancel { repo, run_id })
                        }
                        PendingAction::Rerun { repo, run_id, .. } => {
                            self.pending.push(Command::Rerun { repo, run_id })
                        }
                        PendingAction::RerunFailed { repo, run_id, .. } => {
                            self.pending.push(Command::RerunFailed { repo, run_id })
                        }
                    }
                }
                self.mode = Mode::Normal;
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                self.pending_action = None;
                self.mode = Mode::Normal;
            }
            _ => {}
        }
    }

    // -- helpers -------------------------------------------------------------

    fn move_sel(&mut self, delta: i32) {
        if self.view.is_empty() {
            return;
        }
        let cur = self.table_state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, self.view.len() as i32 - 1) as usize;
        self.select_idx(next);
    }

    fn select_idx(&mut self, i: usize) {
        if self.view.is_empty() {
            return;
        }
        self.table_state.select(Some(i.min(self.view.len() - 1)));
        self.sync_jobs_for_selection();
    }

    fn cycle_filter(&mut self, delta: i32) {
        let cur = Filter::ALL.iter().position(|f| *f == self.filter).unwrap_or(0) as i32;
        let n = Filter::ALL.len() as i32;
        let next = ((cur + delta) % n + n) % n;
        self.set_filter(Filter::ALL[next as usize]);
    }

    fn set_filter(&mut self, f: Filter) {
        self.filter = f;
        self.recompute_view();
    }

    /// True when the *selected* run is still queued/in progress — the only case
    /// where polling its jobs on the fast cadence is worthwhile.
    pub fn selected_run_active(&self) -> bool {
        self.selected_run()
            .map(|r| matches!(r.state(), RunState::Running | RunState::Queued))
            .unwrap_or(false)
    }

    /// Broad sweep: re-list every watched repo's runs, plus the selected run's
    /// jobs. Runs on the slow cadence.
    pub fn queue_broad_refresh(&mut self) {
        if self.loading {
            return;
        }
        self.pending.push(Command::Refresh);
        self.queue_selected_jobs();
    }

    /// Focused poll: just the selected run's jobs (one request), for the fast
    /// cadence while you're watching something run.
    pub fn queue_focused_refresh(&mut self) {
        self.queue_selected_jobs();
    }

    fn queue_selected_jobs(&mut self) {
        if let Some(run) = self.selected_run() {
            self.pending.push(Command::FetchJobs {
                repo: run.repository.full_name.clone(),
                run_id: run.id,
            });
        }
    }

    fn open_logs(&mut self) {
        let Some((job_id, job_name, running)) = self
            .selected_job()
            .map(|j| (j.id, j.name.clone(), j.is_running()))
        else {
            self.set_status("No job selected (jobs still loading?)", true);
            return;
        };
        let repo = match self.selected_run() {
            Some(r) => r.repository.full_name.clone(),
            None => return,
        };
        if running {
            // Text logs 404 until the job completes; show the live step view
            // instead (auto-switches to full logs on completion).
            self.logs = None;
            self.steps_view = Some(StepsView { job_id, job_name, repo, cursor: 0 });
            self.mode = Mode::Logs;
            self.status_msg = None;
        } else {
            let title = format!("{repo} — {job_name}");
            self.set_status("Fetching logs…", false);
            self.pending.push(Command::FetchLogs { repo, job_id, title });
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

    pub fn cycle_job(&mut self, delta: i32) {
        list_move(&mut self.jobs_state, self.jobs.len(), delta);
    }

    fn confirm_cancel(&mut self) {
        let Some(run) = self.selected_run() else { return };
        if !matches!(run.state(), RunState::Running | RunState::Queued) {
            self.set_status("Run is not active — nothing to cancel", true);
            return;
        }
        self.pending_action = Some(PendingAction::Cancel {
            repo: run.repository.full_name.clone(),
            run_id: run.id,
            label: format!("{} #{}", run.workflow_name(), run.run_number),
        });
        self.mode = Mode::Confirm;
    }

    fn confirm_rerun(&mut self, failed_only: bool) {
        let Some(run) = self.selected_run() else { return };
        let label = format!("{} #{}", run.workflow_name(), run.run_number);
        let repo = run.repository.full_name.clone();
        let run_id = run.id;
        self.pending_action = Some(if failed_only {
            PendingAction::RerunFailed { repo, run_id, label }
        } else {
            PendingAction::Rerun { repo, run_id, label }
        });
        self.mode = Mode::Confirm;
    }

    fn open_dispatch(&mut self) {
        let Some(run) = self.selected_run() else {
            self.set_status("Select a run first (its repo is used for dispatch)", true);
            return;
        };
        let repo = run.repository.full_name.clone();
        let default_branch = run.head_branch.clone().unwrap_or_else(|| "main".into());
        self.dispatch = Some(DispatchState {
            repo: repo.clone(),
            workflows: Vec::new(),
            wf_state: ListState::default(),
            stage: DispatchStage::SelectWorkflow,
            git_ref: default_branch,
            fetched_ref: String::new(),
            workflow_id: 0,
            workflow_path: String::new(),
            fields: Vec::new(),
            loaded: false,
            dispatchable: true,
            field_idx: 0,
        });
        self.mode = Mode::Dispatch;
        self.pending.push(Command::FetchWorkflows { repo });
    }

    fn submit_dispatch(&mut self) {
        let Some(d) = &self.dispatch else { return };
        if !d.loaded {
            return; // still fetching the form
        }
        if !d.dispatchable {
            self.set_status("This workflow has no workflow_dispatch trigger", true);
            return;
        }
        if d.git_ref.trim().is_empty() {
            self.set_status("A ref (branch/tag/sha) is required", true);
            return;
        }
        // Required text fields must be filled.
        if let Some(missing) = d.fields.iter().find(|f| {
            f.required
                && matches!(&f.kind, FieldKind::Text { value, .. } if value.trim().is_empty())
        }) {
            self.set_status(format!("Input '{}' is required", missing.name), true);
            return;
        }
        let inputs: HashMap<String, String> = d
            .fields
            .iter()
            .filter_map(|f| {
                let v = f.value();
                // Omit empty optional text so the workflow default applies.
                if v.is_empty() && !f.required {
                    None
                } else {
                    Some((f.name.clone(), v))
                }
            })
            .collect();
        self.pending.push(Command::Dispatch {
            repo: d.repo.clone(),
            workflow_id: d.workflow_id,
            git_ref: d.git_ref.clone(),
            inputs,
        });
        self.dispatch = None;
        self.mode = Mode::Normal;
        self.set_status("Dispatching workflow…", false);
    }
}

fn list_move(state: &mut ListState, len: usize, delta: i32) {
    if len == 0 {
        return;
    }
    let cur = state.selected().unwrap_or(0) as i32;
    let next = (cur + delta).clamp(0, len as i32 - 1) as usize;
    state.select(Some(next));
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mirrors the real GitHub log shape: timestamps, a BOM, two groups
    // (one clean, one containing an error), and endgroup markers.
    const SAMPLE: &str = "\u{feff}2026-06-03T09:13:47.0000000Z Current runner version\n\
2026-06-03T09:13:47.0000000Z ##[group]GITHUB_TOKEN Permissions\n\
2026-06-03T09:13:47.0000000Z Contents: read\n\
2026-06-03T09:13:47.0000000Z Metadata: read\n\
2026-06-03T09:13:47.0000000Z ##[endgroup]\n\
2026-06-03T09:13:47.0000000Z Secret source: Actions\n\
2026-06-03T09:13:47.0000000Z ##[group]Run build\n\
2026-06-03T09:13:47.0000000Z ##[error]boom\n\
2026-06-03T09:13:47.0000000Z ##[endgroup]\n";

    fn src_lines(lv: &LogsView) -> Vec<&str> {
        lv.visible.iter().map(|&i| log_content(&lv.lines[i])).collect()
    }

    #[test]
    fn folds_clean_group_autoexpands_error_group() {
        let lv = LogsView::new("t".into(), SAMPLE);
        assert_eq!(lv.groups.len(), 2);
        // Group 0 (clean) starts collapsed; group 1 (has error) auto-expands.
        assert!(lv.groups[0].collapsed);
        assert!(!lv.groups[1].collapsed);
        // endgroup markers and the collapsed body are hidden.
        let v = src_lines(&lv);
        assert_eq!(
            v,
            vec![
                "Current runner version",
                "##[group]GITHUB_TOKEN Permissions",
                "Secret source: Actions",
                "##[group]Run build",
                "##[error]boom",
            ]
        );
        assert_eq!(lv.groups[0].body_count, 2);
    }

    #[test]
    fn toggle_expands_and_collapses() {
        let mut lv = LogsView::new("t".into(), SAMPLE);
        lv.cursor = 1; // on the "GITHUB_TOKEN Permissions" header
        lv.toggle_fold();
        let v = src_lines(&lv);
        assert!(v.contains(&"Contents: read"));
        assert!(v.contains(&"Metadata: read"));
        // Cursor stays on the header after toggling.
        assert_eq!(log_content(&lv.lines[lv.visible[lv.cursor]]), "##[group]GITHUB_TOKEN Permissions");
        lv.toggle_fold();
        assert!(!src_lines(&lv).contains(&"Contents: read"));
    }

    #[test]
    fn search_finds_and_reveals_folded_hits() {
        let mut lv = LogsView::new("t".into(), SAMPLE);
        // "read" lives inside the collapsed clean group.
        assert!(lv.groups[0].collapsed);
        lv.search = "read".into();
        lv.update_search();
        assert_eq!(lv.matches.len(), 2); // "Contents: read", "Metadata: read"
        assert!(!lv.groups[0].collapsed); // search revealed the folded group
        // Both matches are now visible and cursor sits on the first.
        assert_eq!(log_content(&lv.lines[lv.visible[lv.cursor]]), "Contents: read");
        lv.next_match(1);
        assert_eq!(log_content(&lv.lines[lv.visible[lv.cursor]]), "Metadata: read");
        lv.next_match(1); // wraps
        assert_eq!(log_content(&lv.lines[lv.visible[lv.cursor]]), "Contents: read");
    }

    #[test]
    fn computes_step_duration_from_timestamps() {
        let text = "2026-06-03T09:13:47.0000000Z ##[group]Build\n\
2026-06-03T09:13:49.5000000Z compiling\n\
2026-06-03T09:13:50.0000000Z ##[endgroup]\n";
        let lv = LogsView::new("t".into(), text);
        let s = lv.groups[0].secs.expect("duration");
        assert!((s - 3.0).abs() < 0.01, "expected ~3.0s, got {s}");
    }

    #[test]
    fn search_no_match_is_empty() {
        let mut lv = LogsView::new("t".into(), SAMPLE);
        lv.search = "zzzznope".into();
        lv.update_search();
        assert!(lv.matches.is_empty());
        assert!(lv.match_idx.is_none());
    }

    #[test]
    fn expand_and_fold_all() {
        let mut lv = LogsView::new("t".into(), SAMPLE);
        lv.set_all_collapsed(false);
        assert!(src_lines(&lv).contains(&"Contents: read"));
        lv.set_all_collapsed(true);
        let v = src_lines(&lv);
        assert!(!v.contains(&"Contents: read"));
        assert!(!v.contains(&"##[error]boom")); // even the error group folds on "fold all"
    }
}
