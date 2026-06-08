//! The input layer: translates keystrokes into state mutations and queued
//! `Command`s. A second `impl App` block, kept apart from the data reducer in
//! `app.rs` so each file owns one concern. Methods reach `App`'s private
//! reducer helpers and fields directly, since this is a child module of `app`.

use super::*;
use crate::github::RunState;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::{HashMap, HashSet};

impl App {
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
            Mode::Artifacts => self.key_artifacts(key),
            Mode::Approval => self.key_approval(key),
            Mode::RefPicker => self.key_ref_picker(key),
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
            KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace | KeyCode::Esc => {
                self.focus = Focus::Runs
            }
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
            KeyCode::Char('o') => self.open_in_browser(),
            // Always-available: open the selected job's logs regardless of focus.
            KeyCode::Char('L') => self.open_logs(),
            KeyCode::Char('c') => self.confirm_cancel(),
            KeyCode::Char('x') => self.confirm_rerun(false),
            KeyCode::Char('X') => self.confirm_rerun(true),
            KeyCode::Char('R') => self.confirm_rerun_job(),
            KeyCode::Char('a') => self.confirm_approve(),
            KeyCode::Char('A') => self.open_artifacts(),
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
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace | KeyCode::Left => {
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
                // Cancel the run this job belongs to (GitHub has no per-job
                // cancel); confirmation returns here, keeping the step view open.
                KeyCode::Char('c') => self.confirm_cancel(),
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
            // Backspace closes too; Left stays bound to horizontal scroll.
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace => {
                self.logs = None;
                self.mode = Mode::Normal;
            }
            KeyCode::Char('j') | KeyCode::Down => lv.move_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => lv.move_cursor(-1),
            KeyCode::PageDown => lv.move_cursor(20),
            KeyCode::PageUp => lv.move_cursor(-20),
            KeyCode::Char('g') | KeyCode::Home => lv.cursor_to(true),
            KeyCode::Char('G') | KeyCode::End => lv.cursor_to(false),
            // Horizontal scroll for lines wider than the pane.
            KeyCode::Left => lv.hscroll = lv.hscroll.saturating_sub(8),
            KeyCode::Right => lv.hscroll = (lv.hscroll + 8).min(2000),
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
            // Save the raw log to a file in the working directory.
            KeyCode::Char('s') => self.save_current_logs(),
            _ => {}
        }
    }

    /// Queue a save of the open log to a file named after its title.
    fn save_current_logs(&mut self) {
        let Some(lv) = &self.logs else { return };
        let content = lv.lines.join("\n");
        let safe: String = lv
            .title
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '_' })
            .collect();
        let name = format!("{}.log", safe.trim_matches('_'));
        self.pending.push(Command::SaveLogs { name, content });
    }

    fn key_artifacts(&mut self, key: KeyEvent) {
        let Some(av) = &mut self.artifacts else {
            self.mode = Mode::Normal;
            return;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace | KeyCode::Left => {
                self.artifacts = None;
                self.mode = Mode::Normal;
            }
            KeyCode::Char('j') | KeyCode::Down => list_move(&mut av.state, av.items.len(), 1),
            KeyCode::Char('k') | KeyCode::Up => list_move(&mut av.state, av.items.len(), -1),
            KeyCode::Char('g') | KeyCode::Home => list_jump(&mut av.state, av.items.len(), true),
            KeyCode::Char('G') | KeyCode::End => list_jump(&mut av.state, av.items.len(), false),
            KeyCode::Enter => {
                if let Some(a) = av.state.selected().and_then(|i| av.items.get(i)) {
                    if a.expired {
                        self.set_status("Artifact has expired", true);
                    } else {
                        let (repo, artifact_id, name) = (av.repo.clone(), a.id, a.name.clone());
                        self.set_status(format!("Downloading {name}…"), false);
                        self.pending
                            .push(Command::DownloadArtifact { repo, artifact_id, name });
                    }
                }
            }
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
                KeyCode::Esc | KeyCode::Backspace | KeyCode::Left => {
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
        // On the ref field, Space or → opens the branch/tag picker.
        if let Some(d) = &self.dispatch {
            if d.field_idx == 0 && matches!(key.code, KeyCode::Char(' ') | KeyCode::Right) {
                self.open_ref_picker();
                return;
            }
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
                        PendingAction::RerunJob { repo, job_id, .. } => {
                            self.pending.push(Command::RerunJob { repo, job_id })
                        }
                        PendingAction::Approve { repo, run_id, .. } => {
                            self.pending.push(Command::Approve { repo, run_id })
                        }
                    }
                }
                self.mode = self.overlay_return_mode();
            }
            KeyCode::Char('n') | KeyCode::Esc | KeyCode::Backspace => {
                self.pending_action = None;
                self.mode = self.overlay_return_mode();
            }
            _ => {}
        }
    }

    /// Mode to restore after a confirm/overlay closes: back to the logs/steps
    /// view if one is open (e.g. cancel invoked while watching live steps),
    /// otherwise the normal two-pane view.
    fn overlay_return_mode(&self) -> Mode {
        if self.steps_view.is_some() || self.logs.is_some() {
            Mode::Logs
        } else {
            Mode::Normal
        }
    }

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

    pub(crate) fn cycle_filter(&mut self, delta: i32) {
        let cur = Filter::ALL.iter().position(|f| *f == self.filter).unwrap_or(0) as i32;
        let n = Filter::ALL.len() as i32;
        let next = ((cur + delta) % n + n) % n;
        self.set_filter(Filter::ALL[next as usize]);
    }

    fn set_filter(&mut self, f: Filter) {
        self.filter = f;
        self.recompute_view();
    }

    /// Open github.com for the current selection: the focused job's page when the
    /// Jobs pane holds focus and a job is selected, otherwise the run's page.
    fn open_in_browser(&mut self) {
        let url = if self.focus == Focus::Jobs {
            match (self.selected_run(), self.selected_job()) {
                (Some(run), Some(job)) => Some(if job.html_url.is_empty() {
                    format!("{}/job/{}", run.html_url, job.id)
                } else {
                    job.html_url.clone()
                }),
                _ => None,
            }
        } else {
            None
        };
        // Fall back to the run page (also when no job is selected).
        let url = url.or_else(|| self.selected_run().map(|r| r.html_url.clone()));
        if let Some(url) = url {
            self.pending.push(Command::OpenUrl(url));
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
            // Serve cached logs instantly; completed-job logs never change.
            if let Some(text) = self.logs_cache.get(&job_id) {
                self.logs = Some(LogsView::new(title, text));
                self.steps_view = None;
                self.mode = Mode::Logs;
                self.status_msg = None;
            } else {
                self.set_status("Fetching logs…", false);
                self.pending.push(Command::FetchLogs { repo, job_id, title });
            }
        }
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

    /// Re-run just the selected job (requires a job to be selected).
    fn confirm_rerun_job(&mut self) {
        let Some(repo) = self.selected_run().map(|r| r.repository.full_name.clone()) else {
            return;
        };
        let Some((job_id, label)) = self.selected_job().map(|j| (j.id, j.name.clone())) else {
            self.set_status("Select a job first (Tab to focus Jobs)", true);
            return;
        };
        self.pending_action = Some(PendingAction::RerunJob { repo, job_id, label });
        self.mode = Mode::Confirm;
    }

    /// Approve a run that's held for approval. Only runs actually awaiting
    /// approval offer this. We fetch the run's pending deployments: if any
    /// environment gates it, open the review picker; if not, it's a fork-PR
    /// approval and we fall back to a simple confirm (handled when the empty
    /// list arrives).
    fn confirm_approve(&mut self) {
        let Some(run) = self.selected_run() else { return };
        if !run.needs_approval() {
            self.set_status("Run is not awaiting approval", true);
            return;
        }
        let (repo, run_id) = (run.repository.full_name.clone(), run.id);
        self.approval = Some(ApprovalView {
            repo: repo.clone(),
            run_id,
            items: Vec::new(),
            selected: HashSet::new(),
            state: ListState::default(),
            loaded: false,
            comment: String::new(),
            editing_comment: false,
        });
        self.mode = Mode::Approval;
        self.pending.push(Command::FetchPendingDeployments { repo, run_id });
    }

    fn key_approval(&mut self, key: KeyEvent) {
        // Comment edit sub-mode: keystrokes edit the review comment.
        if self.approval.as_ref().is_some_and(|a| a.editing_comment) {
            let av = self.approval.as_mut().unwrap();
            match key.code {
                KeyCode::Esc | KeyCode::Enter => av.editing_comment = false,
                KeyCode::Backspace => {
                    av.comment.pop();
                }
                KeyCode::Char(c) => av.comment.push(c),
                _ => {}
            }
            return;
        }
        let loaded = self.approval.as_ref().is_some_and(|a| a.loaded);
        match key.code {
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('q') => {
                self.approval = None;
                self.mode = Mode::Normal;
            }
            _ if !loaded => {}
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(av) = &mut self.approval {
                    list_move(&mut av.state, av.items.len(), 1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(av) = &mut self.approval {
                    list_move(&mut av.state, av.items.len(), -1);
                }
            }
            KeyCode::Char('g') | KeyCode::Home => {
                if let Some(av) = &mut self.approval {
                    list_jump(&mut av.state, av.items.len(), true);
                }
            }
            KeyCode::Char('G') | KeyCode::End => {
                if let Some(av) = &mut self.approval {
                    list_jump(&mut av.state, av.items.len(), false);
                }
            }
            // Toggle the highlighted environment (only if the user can approve it).
            KeyCode::Char(' ') => {
                let mut denied = false;
                if let Some(av) = &mut self.approval {
                    if let Some(i) = av.state.selected() {
                        if av.can_approve(i) {
                            if !av.selected.remove(&i) {
                                av.selected.insert(i);
                            }
                        } else {
                            denied = true;
                        }
                    }
                }
                if denied {
                    self.set_status("You can't review that environment", true);
                }
            }
            KeyCode::Char('c') => {
                if let Some(av) = &mut self.approval {
                    av.editing_comment = true;
                }
            }
            // Approve / reject the selected environments.
            KeyCode::Enter | KeyCode::Char('y') => self.submit_review(true),
            KeyCode::Char('x') | KeyCode::Char('r') | KeyCode::Char('R') => self.submit_review(false),
            _ => {}
        }
    }

    /// Submit the chosen environments for approval (or rejection).
    fn submit_review(&mut self, approve: bool) {
        let Some(av) = &self.approval else { return };
        let env_ids = av.chosen_ids();
        if env_ids.is_empty() {
            self.set_status("Select at least one environment you can review", true);
            return;
        }
        let (repo, run_id, comment) = (av.repo.clone(), av.run_id, av.comment.clone());
        self.pending.push(Command::ReviewDeployments { repo, run_id, env_ids, approve, comment });
        self.approval = None;
        self.mode = Mode::Normal;
        self.set_status(
            if approve { "Approving deployment…" } else { "Rejecting deployment…" },
            false,
        );
    }

    /// Open the branch/tag picker for the dispatch ref field.
    fn open_ref_picker(&mut self) {
        let Some(d) = &self.dispatch else { return };
        let repo = d.repo.clone();
        self.ref_picker = Some(RefPicker {
            repo: repo.clone(),
            items: Vec::new(),
            view: Vec::new(),
            state: ListState::default(),
            filter: String::new(),
            loaded: false,
        });
        self.mode = Mode::RefPicker;
        self.pending.push(Command::FetchRefs { repo });
    }

    fn key_ref_picker(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.ref_picker = None;
                self.mode = Mode::Dispatch;
            }
            KeyCode::Down => {
                if let Some(rp) = &mut self.ref_picker {
                    list_move(&mut rp.state, rp.view.len(), 1);
                }
            }
            KeyCode::Up => {
                if let Some(rp) = &mut self.ref_picker {
                    list_move(&mut rp.state, rp.view.len(), -1);
                }
            }
            KeyCode::Enter => {
                let picked = self
                    .ref_picker
                    .as_ref()
                    .and_then(|rp| rp.selected_ref().map(|r| r.name.clone()));
                self.ref_picker = None;
                self.mode = Mode::Dispatch;
                if let Some(name) = picked {
                    // Setting a new ref means the workflow's inputs may differ.
                    let changed = match &mut self.dispatch {
                        Some(d) => {
                            d.git_ref = name;
                            d.git_ref != d.fetched_ref
                        }
                        None => false,
                    };
                    if changed {
                        self.refetch_dispatch_inputs();
                    }
                }
            }
            KeyCode::Backspace => {
                let empty = self.ref_picker.as_ref().map(|rp| rp.filter.is_empty()).unwrap_or(true);
                if empty {
                    self.ref_picker = None;
                    self.mode = Mode::Dispatch;
                } else if let Some(rp) = &mut self.ref_picker {
                    rp.filter.pop();
                    rp.recompute();
                }
            }
            KeyCode::Char(c) => {
                if let Some(rp) = &mut self.ref_picker {
                    rp.filter.push(c);
                    rp.recompute();
                }
            }
            _ => {}
        }
    }

    fn open_artifacts(&mut self) {
        let Some(run) = self.selected_run() else {
            self.set_status("Select a run first", true);
            return;
        };
        let (repo, run_id) = (run.repository.full_name.clone(), run.id);
        self.artifacts = Some(ArtifactsView {
            repo: repo.clone(),
            run_id,
            items: Vec::new(),
            state: ListState::default(),
            loaded: false,
        });
        self.mode = Mode::Artifacts;
        self.pending.push(Command::FetchArtifacts { repo, run_id });
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

    pub(crate) fn submit_dispatch(&mut self) {
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
