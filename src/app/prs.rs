//! The pull request screens' state. Key handling is in `input`, the worker
//! protocol in `protocol`, reviewing in `review`.

mod compose;
mod diff;
mod input;
mod merge;
mod protocol;
mod review;

pub use compose::{Compose, ComposeTarget, Outbox};
pub use diff::{draft_rows, DiffLine, DiffView, FilesState};
pub use merge::MergeDialog;
pub use protocol::{PrAction, PrActionKind, PrCommand, PrMsg};
pub use review::{PendingReview, ReviewPopup, ThreadPopup};

use super::{App, Command, CursorHold, Focus, Mode};
use crate::github::Run;
use crate::pulls::model::{CheckState, PrDetail, PrGroup, PrRef, PrSummary};
use ratatui::text::Line;
use ratatui::widgets::ListState;
use std::collections::{HashMap, HashSet};

/// Something a request is out reading.
#[derive(Default)]
pub enum Loadable<T> {
    #[default]
    Loading,
    Ready(T),
    Failed(String),
}

/// One line of the PRs list.
pub enum PrRow<'a> {
    Header { group: PrGroup, detail: String },
    Pr(&'a PrSummary),
}

/// The two lists on the PRs screen, as one scrolling column of rows.
#[derive(Default)]
pub struct PrList {
    pub review_requested: Loadable<Vec<PrSummary>>,
    pub authored: Loadable<Vec<PrSummary>>,
    /// Selected index into `rows()`.
    pub state: ListState,
}

impl PrList {
    /// Takes in a finished search. A failed refresh leaves what was already
    /// shown in place, and the cursor stays on its PR.
    pub fn set(&mut self, group: PrGroup, result: Result<Vec<PrSummary>, String>) {
        let keep = self.selected().map(|p| p.pr.clone());
        let slot = match group {
            PrGroup::ReviewRequested => &mut self.review_requested,
            PrGroup::Authored => &mut self.authored,
        };
        match result {
            Ok(prs) => *slot = Loadable::Ready(prs),
            Err(e) if !matches!(slot, Loadable::Ready(_)) => *slot = Loadable::Failed(e),
            Err(_) => {}
        }
        self.reselect(keep);
    }

    fn reselect(&mut self, keep: Option<PrRef>) {
        let rows = self.rows();
        let at = keep
            .and_then(|pr| rows.iter().position(|r| matches!(r, PrRow::Pr(p) if p.pr == pr)))
            .or_else(|| rows.iter().position(|r| matches!(r, PrRow::Pr(_))));
        self.state.select(at);
    }

    /// Both groups with their PRs. A PR in both appears only under Review
    /// requested, where it is waiting on you.
    pub fn rows(&self) -> Vec<PrRow<'_>> {
        let mut rows = Vec::new();
        let requested = match &self.review_requested {
            Loadable::Ready(prs) => prs.as_slice(),
            _ => &[],
        };
        let groups = [
            (PrGroup::ReviewRequested, &self.review_requested, &[][..]),
            (PrGroup::Authored, &self.authored, requested),
        ];
        for (group, list, skip) in groups {
            let shown: Vec<&PrSummary> = match list {
                Loadable::Ready(prs) => prs.iter().filter(|p| !skip.iter().any(|s| s.pr == p.pr)).collect(),
                _ => Vec::new(),
            };
            let detail = match list {
                Loadable::Loading => "loading…".to_string(),
                Loadable::Failed(e) => e.clone(),
                Loadable::Ready(_) if shown.is_empty() => "none".to_string(),
                Loadable::Ready(_) => shown.len().to_string(),
            };
            rows.push(PrRow::Header { group, detail });
            rows.extend(shown.into_iter().map(PrRow::Pr));
        }
        rows
    }

    pub fn selected(&self) -> Option<&PrSummary> {
        let at = self.state.selected()?;
        match self.rows().into_iter().nth(at)? {
            PrRow::Pr(pr) => Some(pr),
            PrRow::Header { .. } => None,
        }
    }

    /// Indexes of the rows that are PRs.
    fn pr_indices(&self) -> Vec<usize> {
        let rows = self.rows();
        (0..rows.len()).filter(|&i| matches!(rows[i], PrRow::Pr(_))).collect()
    }

    /// Moves the cursor by `delta` PRs, stepping over headers.
    pub fn move_sel(&mut self, delta: i32) {
        let prs = self.pr_indices();
        if prs.is_empty() {
            return;
        }
        let cur = self.state.selected().and_then(|s| prs.iter().position(|&i| i == s)).unwrap_or(0);
        let next = (cur as i32 + delta).clamp(0, prs.len() as i32 - 1) as usize;
        self.state.select(Some(prs[next]));
    }

    /// Puts the cursor on the first or last PR.
    pub fn jump(&mut self, top: bool) {
        let prs = self.pr_indices();
        let pick = if top { prs.first() } else { prs.last() };
        if let Some(&i) = pick {
            self.state.select(Some(i));
        }
    }

    /// Selects the PR on the given row of the viewport; headers and empty
    /// space do nothing.
    pub fn click_row(&mut self, row_from_top: usize) {
        let at = self.state.offset() + row_from_top;
        if self.pr_indices().contains(&at) {
            self.state.select(Some(at));
        }
    }

    /// How many PRs wait on your review.
    pub fn waiting(&self) -> usize {
        match &self.review_requested {
            Loadable::Ready(prs) => prs.len(),
            _ => 0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrTab {
    Overview,
    Files,
    Conversation,
}

impl PrTab {
    pub const ALL: [PrTab; 3] = [PrTab::Overview, PrTab::Files, PrTab::Conversation];

    pub fn title(self) -> &'static str {
        match self {
            PrTab::Overview => "Overview",
            PrTab::Files => "Files",
            PrTab::Conversation => "Conversation",
        }
    }
}

/// A scrolling text: how far it is scrolled, and the lines last built for it
/// with their wrapped height. Building and measuring them is the costly part
/// of a frame, so they are kept until the width changes or `forget` is called.
#[derive(Default)]
pub struct Scroll {
    offset: u16,
    /// The most `offset` can be, as of the last frame.
    max: u16,
    /// Rows the text area showed.
    page: u16,
    built: Option<Built>,
    #[cfg(test)]
    pub builds: u32,
}

struct Built {
    width: u16,
    rows: usize,
    lines: Vec<Line<'static>>,
}

impl Scroll {
    pub fn by(&mut self, delta: i32) {
        let next = (self.offset as i64 + delta as i64).clamp(0, self.max as i64);
        self.offset = next as u16;
    }

    pub fn offset(&self) -> u16 {
        self.offset
    }

    #[cfg(test)]
    pub fn max(&self) -> u16 {
        self.max
    }

    pub fn page(&self) -> u16 {
        self.page
    }

    /// Drops the built lines, because the data they came from changed.
    pub fn forget(&mut self) {
        self.built = None;
    }

    /// The lines for a text area `width` wide, and their wrapped height.
    /// `build` and `measure` run only when nothing is kept for this width.
    pub fn lines(
        &mut self,
        width: u16,
        build: impl FnOnce() -> Vec<Line<'static>>,
        measure: impl FnOnce(&[Line<'static>]) -> usize,
    ) -> (&[Line<'static>], usize) {
        if self.built.as_ref().is_none_or(|b| b.width != width) {
            let lines = build();
            let rows = measure(&lines);
            self.built = Some(Built { width, rows, lines });
            #[cfg(test)]
            {
                self.builds += 1;
            }
        }
        let built = self.built.as_ref().expect("just built");
        (&built.lines, built.rows)
    }

    /// Takes in the size of the text and of the area showing it.
    pub fn fit(&mut self, rows: usize, height: u16) {
        self.max = rows.saturating_sub(height as usize).min(u16::MAX as usize) as u16;
        self.page = height;
        self.offset = self.offset.min(self.max);
    }
}

/// A PR opened from the list.
pub struct PrView {
    pub pr: PrRef,
    pub detail: Loadable<Box<PrDetail>>,
    pub tab: PrTab,
    pub overview: Scroll,
    pub conversation: Scroll,
    pub checks: ListState,
    pub files: FilesState,
    /// The box being written in, if one is open.
    pub compose: Option<Compose>,
    /// The box that picks how to merge, if one is open.
    pub merge_dialog: Option<MergeDialog>,
    /// The review box open over the PR, if any.
    pub review_popup: Option<ReviewPopup>,
}

impl PrView {
    fn new(pr: PrRef) -> Self {
        PrView {
            pr,
            detail: Loadable::Loading,
            tab: PrTab::Overview,
            overview: Scroll::default(),
            conversation: Scroll::default(),
            checks: ListState::default(),
            files: FilesState::default(),
            compose: None,
            merge_dialog: None,
            review_popup: None,
        }
    }

    /// Whether a box is open over the PR: the comment box, the merge box or
    /// a review box.
    pub fn has_box(&self) -> bool {
        self.compose.is_some() || self.merge_dialog.is_some() || self.review_popup.is_some()
    }

    pub fn cycle_tab(&mut self, delta: i32) {
        let at = PrTab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0) as i32;
        self.tab = PrTab::ALL[(at + delta).rem_euclid(PrTab::ALL.len() as i32) as usize];
    }

    /// Looks up the open diff's review threads in the current detail.
    pub fn sync_threads(&mut self) {
        if let Loadable::Ready(detail) = &self.detail {
            self.files.sync_threads(&detail.threads);
        }
    }

    /// Scrolls the text of the shown tab, if that tab is a scrolling text.
    pub fn scroll_text(&mut self, delta: i32) {
        match self.tab {
            PrTab::Overview => self.overview.by(delta),
            PrTab::Conversation => self.conversation.by(delta),
            PrTab::Files => {}
        }
    }

    /// Scrolls the conversation by whole pages (`pages` may be negative).
    pub fn page_conversation(&mut self, pages: i32) {
        let page = self.conversation.page().saturating_sub(1).max(1) as i32;
        self.conversation.by(pages.saturating_mul(page));
    }

    pub fn move_check(&mut self, delta: i32) {
        let Loadable::Ready(detail) = &self.detail else { return };
        let count = detail.checks.len() as i32;
        if count == 0 {
            return;
        }
        let at = self.checks.selected().unwrap_or(0) as i32;
        self.checks.select(Some((at + delta).clamp(0, count - 1) as usize));
    }

    /// Takes in a finished read. A failed refresh leaves what was already
    /// shown in place.
    fn set_detail(&mut self, result: Result<Box<PrDetail>, String>) {
        match result {
            Ok(detail) => {
                let kept = self.checks.selected().filter(|&i| i < detail.checks.len());
                self.checks.select(kept.or(if detail.checks.is_empty() { None } else { Some(0) }));
                self.detail = Loadable::Ready(detail);
                self.overview.forget();
                self.conversation.forget();
                self.sync_threads();
            }
            Err(e) if !matches!(self.detail, Loadable::Ready(_)) => self.detail = Loadable::Failed(e),
            Err(_) => {}
        }
    }
}

/// A visit to the runs screen from a PR's check.
pub struct RunJump {
    pub run_id: u64,
    /// The run was not on the runs list and was added for the visit, so
    /// leaving takes it out again.
    pub stray: bool,
}

/// Everything the PRs screens show.
#[derive(Default)]
pub struct PrsState {
    pub list: PrList,
    /// The PR opened from the list, if any.
    pub open: Option<PrView>,
    /// Set while the runs screen shows a check's run, so Esc can come back.
    pub jump: Option<RunJump>,
    /// Each PR's review being written, kept while the user moves around.
    pub drafts: HashMap<PrRef, PendingReview>,
    /// PRs whose review has been sent and has no answer yet.
    sending: HashSet<PrRef>,
    /// Each PR's text that left its box and is not safe yet.
    pub outbox: HashMap<PrRef, Outbox>,
}

impl PrsState {
    /// Whether the open PR has checks still running, which is when it is
    /// worth reading again soon.
    pub fn open_checks_pending(&self) -> bool {
        match self.open.as_ref().map(|v| &v.detail) {
            Some(Loadable::Ready(d)) => d.checks.iter().any(|c| c.state == CheckState::Pending),
            _ => false,
        }
    }
}

impl App {
    /// Whether the PRs screens are what shows, which includes a question
    /// asked over them.
    pub fn shows_prs(&self) -> bool {
        self.mode == Mode::Prs || (self.mode == Mode::Confirm && self.confirm_from == Mode::Prs)
    }

    /// Reads the open PR again; the refresh timer calls this.
    pub fn queue_pr_refresh(&mut self) {
        if let Some(view) = &self.prs.open {
            self.pending.push(Command::Pr(PrCommand::FetchDetail(view.pr.clone())));
            // Only the Files tab keeps its files fresh; elsewhere they cost quota for nothing.
            if view.tab == PrTab::Files {
                self.pending.push(Command::Pr(PrCommand::FetchFiles(view.pr.clone())));
            }
        }
    }

    /// Goes to a run on the runs screen, reading it first if it is not listed.
    pub(super) fn jump_to_run(&mut self, repo: String, run_id: u64) {
        self.prs.jump = Some(RunJump { run_id, stray: false });
        self.mode = Mode::Normal;
        self.focus = Focus::Runs;
        if self.runs.iter().any(|r| r.id == run_id) {
            self.select_jump_run(run_id);
        } else {
            self.pending.push(Command::Pr(PrCommand::FetchRun { repo, run_id }));
        }
    }

    /// Puts the runs cursor on the run and keeps it there through sweeps.
    fn select_jump_run(&mut self, run_id: u64) {
        self.cursor_hold = CursorHold::PickedRun;
        self.recompute_view_keeping(Some(run_id));
    }

    fn apply_jump_run(&mut self, result: Result<Box<Run>, String>) {
        let Some(jump) = &self.prs.jump else { return };
        let run_id = jump.run_id;
        let listed = self.runs.iter().any(|r| r.id == run_id);
        match result {
            Ok(run) if run.id == run_id => {
                if !listed {
                    self.runs.push(*run);
                    self.resort();
                    if let Some(jump) = &mut self.prs.jump {
                        jump.stray = true;
                    }
                }
                self.select_jump_run(run_id);
            }
            Ok(_) => {}
            // A sweep has listed the run since, so the failed read no longer matters.
            Err(_) if listed => {}
            Err(e) => {
                self.prs.jump = None;
                self.mode = Mode::Prs;
                self.set_status(format!("Run {run_id}: {e}"), true);
            }
        }
    }

    /// Ends a visit to a check's run, taking out the run if the visit added it.
    pub(super) fn end_run_jump(&mut self) {
        let Some(jump) = self.prs.jump.take() else { return };
        let keep = self.run_to_keep();
        if jump.stray {
            self.runs.retain(|r| r.id != jump.run_id);
        }
        self.recompute_view_keeping(keep);
    }

    /// The run a visit added, if it belongs to `repo`, to put back after that
    /// repo's runs are replaced.
    pub(super) fn stray_run_of(&self, repo: &str) -> Option<Run> {
        let jump = self.prs.jump.as_ref().filter(|j| j.stray)?;
        self.runs.iter().find(|r| r.id == jump.run_id && r.repository.full_name == repo).cloned()
    }

    /// Puts back a visit's run that a sweep of its repo replaced. If the sweep
    /// listed it, it is a normal run from then on.
    pub(super) fn restore_stray_run(&mut self, stray: Option<Run>) {
        let Some(run) = stray else { return };
        if self.runs.iter().any(|r| r.id == run.id) {
            if let Some(jump) = &mut self.prs.jump {
                jump.stray = false;
            }
        } else {
            self.runs.push(run);
        }
    }

    pub(super) fn apply_pr(&mut self, msg: PrMsg) {
        match msg {
            PrMsg::List { group, result } => {
                if let Err(e) = &result {
                    self.set_status(format!("{}: {e}", group.title()), true);
                }
                self.prs.list.set(group, result);
            }
            PrMsg::Run(result) => self.apply_jump_run(result),
            PrMsg::Files { pr, result } => {
                if !self.prs.open.as_ref().is_some_and(|v| v.pr == pr) {
                    return;
                }
                if let Err(e) = &result {
                    self.set_status(format!("{}: {e}", pr.label()), true);
                }
                if let Some(view) = &mut self.prs.open {
                    view.files.set(result);
                    view.sync_threads();
                }
            }
            PrMsg::Detail { pr, result } => {
                // An answer for a PR that has since been closed or swapped is dropped.
                if !self.prs.open.as_ref().is_some_and(|v| v.pr == pr) {
                    return;
                }
                if let Err(e) = &result {
                    self.set_status(format!("{}: {e}", pr.label()), true);
                }
                if let Some(view) = &mut self.prs.open {
                    view.set_detail(result);
                }
            }
            PrMsg::ActDone { pr, kind, result } => self.apply_act_done(pr, kind, result),
        }
    }

    /// Reports a finished change, gives a failed message back to the box it
    /// was written in, and reads the PR again either way.
    fn apply_act_done(&mut self, pr: PrRef, kind: PrActionKind, result: Result<(), String>) {
        match &result {
            Ok(()) => self.set_status(kind.done_text(), false),
            Err(e) => self.set_status(format!("{}: {e}", pr.label()), true),
        }
        let merged = kind == PrActionKind::Merge && result.is_ok();
        // Only a review that went through ends the drafts; a failed one keeps
        // them to send again.
        if kind == PrActionKind::SubmitReview {
            self.prs.sending.remove(&pr);
            if result.is_ok() {
                self.prs.drafts.remove(&pr);
            }
        }
        // Text sent from a box is safe once posted. Refused, it goes back in a
        // box: now if its PR is open, or when it opens next.
        let answered = matches!(self.prs.outbox.get(&pr), Some(Outbox::Sending(s)) if s.target.sends() == Some(kind));
        if answered {
            if let (Some(Outbox::Sending(sent)), Err(_)) = (self.prs.outbox.remove(&pr), &result) {
                self.prs.outbox.insert(pr.clone(), Outbox::Refused(sent));
            }
            self.give_back_refused();
        }
        let Some(view) = self.prs.open.as_ref().filter(|v| v.pr == pr) else {
            // The list still shows a PR merged after its view was closed.
            if merged {
                self.pending.push(Command::Pr(PrCommand::FetchList));
            }
            return;
        };
        self.pending.push(Command::Pr(PrCommand::FetchDetail(pr.clone())));
        if matches!(view.files.files, Loadable::Ready(_)) {
            self.pending.push(Command::Pr(PrCommand::FetchFiles(pr)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{DataMsg, Filter, Mode, PendingAction};
    use crate::pulls::model::{
        Check, CheckState, CheckTarget, ChecksState, MergeMethod, MergeStatus, QueueState, ReviewDecision,
    };
    use crossterm::event::{KeyCode, KeyEvent};
    use chrono::Utc;

    pub(super) fn summary(repo: &str, n: u64) -> PrSummary {
        PrSummary {
            pr: PrRef { repo: repo.into(), number: n },
            node_id: format!("PR_{n}"),
            title: format!("change {n}"),
            author: "ana".into(),
            updated_at: Utc::now(),
            draft: false,
            checks: ChecksState::Passing,
            review: ReviewDecision::NotRequired,
        }
    }

    fn run_in(repo: &str, id: u64, conclusion: &str) -> crate::github::Run {
        crate::github::Run {
            id,
            name: Some("CI".into()),
            display_title: "fix".into(),
            head_branch: Some("main".into()),
            head_sha: "a1b2c3d".into(),
            run_number: 7,
            workflow_id: 11,
            event: "push".into(),
            status: "completed".into(),
            conclusion: Some(conclusion.into()),
            html_url: String::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            run_started_at: None,
            actor: None,
            repository: crate::github::RunRepo { full_name: repo.into() },
        }
    }

    /// An app showing a PR whose only check is Actions run 555.
    pub(super) fn app_on_a_pr_with_a_run_check() -> App {
        use crate::pulls::model::*;
        let mut app = App::new(&crate::config::Config::default());
        let pr = PrRef { repo: "org/api".into(), number: 7 };
        let detail = PrDetail {
            node_id: "PR_7".into(),
            title: "Fix login".into(),
            body: String::new(),
            author: "ana".into(),
            url: pr.url(),
            head: HeadRef { branch: "main".into(), repo: "org/api".into(), oid: "abc".into() },
            base_branch: "release".into(),
            draft: false,
            labels: vec![],
            merge_status: MergeStatus::Blocked,
            reviewers: vec![],
            checks: vec![Check {
                name: "Unit tests".into(),
                state: CheckState::Passed,
                target: CheckTarget::ActionsRun { run_id: 555 },
            }],
            comments: vec![],
            reviews: vec![],
            threads: vec![],
            merge_methods: vec![MergeMethod::Squash],
            queue: QueueState::Queued { position: 2 },
        };
        let mut view = PrView::new(pr);
        view.set_detail(Ok(Box::new(detail)));
        app.prs.open = Some(view);
        app.mode = Mode::Prs;
        app
    }

    pub(super) fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::from(code));
    }

    #[test]
    fn a_check_run_outside_the_list_is_fetched_and_shown() {
        let mut app = app_on_a_pr_with_a_run_check();
        app.apply(DataMsg::Runs { repo: "org/api".into(), runs: vec![run_in("org/api", 1, "failure")] });
        app.set_filter(Filter::Failed);
        app.pending.clear();

        press(&mut app, KeyCode::Enter);
        assert!(app.mode == Mode::Normal);
        assert!(
            matches!(&app.pending[..], [Command::Pr(PrCommand::FetchRun { repo, run_id: 555 })] if repo == "org/api")
        );

        app.apply(DataMsg::Pr(PrMsg::Run(Ok(Box::new(run_in("org/api", 555, "success"))))));
        assert_eq!(app.selected_run().map(|r| r.id), Some(555));
        assert_eq!(app.filter, Filter::Failed, "the filter is left alone");
    }

    #[test]
    fn a_loaded_check_run_is_selected_without_a_fetch() {
        let mut app = app_on_a_pr_with_a_run_check();
        app.apply(DataMsg::Runs {
            repo: "org/api".into(),
            runs: vec![run_in("org/api", 1, "failure"), run_in("org/api", 555, "success")],
        });
        app.set_filter(Filter::Failed);
        app.pending.clear();

        press(&mut app, KeyCode::Enter);
        assert!(app.pending.is_empty());
        assert_eq!(app.selected_run().map(|r| r.id), Some(555));

        // A sweep of the repo does not move the cursor off it.
        app.apply(DataMsg::Runs {
            repo: "org/api".into(),
            runs: vec![run_in("org/api", 1, "failure"), run_in("org/api", 555, "success")],
        });
        assert_eq!(app.selected_run().map(|r| r.id), Some(555));
        press(&mut app, KeyCode::Esc);
        assert!(app.mode == Mode::Prs);
        assert!(app.runs.iter().any(|r| r.id == 555), "a run that was already loaded stays");
    }

    #[test]
    fn a_sweep_does_not_drop_a_fetched_run() {
        let mut app = app_on_a_pr_with_a_run_check();
        press(&mut app, KeyCode::Enter);
        app.apply(DataMsg::Pr(PrMsg::Run(Ok(Box::new(run_in("org/api", 555, "success"))))));
        app.apply(DataMsg::Runs { repo: "org/api".into(), runs: vec![run_in("org/api", 1, "failure")] });
        assert_eq!(app.selected_run().map(|r| r.id), Some(555));
        press(&mut app, KeyCode::Esc);
        assert!(!app.runs.iter().any(|r| r.id == 555));
    }

    #[test]
    fn esc_from_a_jumped_run_returns_to_the_pr_and_drops_the_stray_run() {
        let mut app = app_on_a_pr_with_a_run_check();
        press(&mut app, KeyCode::Enter);
        app.apply(DataMsg::Pr(PrMsg::Run(Ok(Box::new(run_in("org/api", 555, "success"))))));
        assert!(app.runs.iter().any(|r| r.id == 555));

        press(&mut app, KeyCode::Esc);
        assert!(app.mode == Mode::Prs);
        assert!(app.prs.open.is_some());
        assert!(app.prs.jump.is_none());
        assert!(!app.runs.iter().any(|r| r.id == 555));
    }

    #[test]
    fn a_failed_run_fetch_stays_on_the_pr_with_the_error() {
        let mut app = app_on_a_pr_with_a_run_check();
        press(&mut app, KeyCode::Enter);
        app.apply(DataMsg::Pr(PrMsg::Run(Err("404 Not Found".into()))));
        assert!(app.mode == Mode::Prs);
        assert!(app.prs.jump.is_none());
        assert!(app.status_msg.as_ref().is_some_and(|(m, err, _)| m.contains("404") && *err));
    }

    #[test]
    fn checks_without_a_run_open_a_link_or_say_so() {
        use crate::pulls::model::{Check, CheckState, CheckTarget};
        let mut app = app_on_a_pr_with_a_run_check();
        let check = |target| Check { name: "x".into(), state: CheckState::Passed, target };
        let set = |app: &mut App, target| {
            if let Some(Loadable::Ready(d)) = app.prs.open.as_mut().map(|v| &mut v.detail) {
                d.checks = vec![check(target)];
            }
        };
        set(&mut app, CheckTarget::Link("https://ci.example/1".into()));
        press(&mut app, KeyCode::Enter);
        assert!(matches!(&app.pending[..], [Command::OpenUrl(u)] if u == "https://ci.example/1"));
        set(&mut app, CheckTarget::Nothing);
        press(&mut app, KeyCode::Enter);
        assert!(app.mode == Mode::Prs);
        assert!(app.status_msg.as_ref().is_some_and(|(m, _, _)| m == "That check has no page to open"));
    }

    pub(super) fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    pub(super) fn ctrl_s(app: &mut App) {
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), crossterm::event::KeyModifiers::CONTROL));
    }

    pub(super) fn edit_detail(app: &mut App, edit: impl FnOnce(&mut PrDetail)) {
        if let Some(Loadable::Ready(d)) = app.prs.open.as_mut().map(|v| &mut v.detail) {
            edit(d);
        }
    }

    #[test]
    fn a_failed_comment_reopens_the_box_with_its_text() {
        let mut app = app_on_a_pr_with_a_run_check();
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "hello");
        ctrl_s(&mut app);
        let pr = app.prs.open.as_ref().unwrap().pr.clone();
        assert!(app.prs.open.as_ref().unwrap().compose.is_none());
        assert!(matches!(
            &app.pending[..],
            [Command::Pr(PrCommand::Act { action: PrAction::Comment { body }, .. })] if body == "hello"
        ));
        app.pending.clear();

        app.apply(DataMsg::Pr(PrMsg::ActDone { pr, kind: PrActionKind::Comment, result: Err("boom".into()) }));
        let view = app.prs.open.as_ref().unwrap();
        assert_eq!(view.compose.as_ref().map(|c| c.text()).as_deref(), Some("hello"));
        assert!(app.prs.outbox.is_empty());
        assert!(app.status_msg.as_ref().is_some_and(|(m, err, _)| m.contains("boom") && *err));
    }

    /// Lists the open PR so Esc and Enter can leave and reopen it.
    fn list_the_open_pr(app: &mut App) {
        app.prs.list.set(PrGroup::ReviewRequested, Ok(vec![summary("org/api", 7)]));
        app.prs.list.set(PrGroup::Authored, Ok(vec![]));
    }

    fn compose_text(app: &App) -> Option<String> {
        app.prs.open.as_ref()?.compose.as_ref().map(|c| c.text())
    }

    #[test]
    fn a_comment_refused_after_leaving_its_pr_comes_back_on_reopening() {
        let mut app = app_on_a_pr_with_a_run_check();
        list_the_open_pr(&mut app);
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "hello");
        ctrl_s(&mut app);
        let pr = app.prs.open.as_ref().unwrap().pr.clone();
        press(&mut app, KeyCode::Esc);
        assert!(app.prs.open.is_none(), "back on the list");
        assert_eq!(app.prs.unsent_count(), 1, "the quit question counts it while it is out");

        app.apply(DataMsg::Pr(PrMsg::ActDone { pr, kind: PrActionKind::Comment, result: Err("boom".into()) }));
        assert!(app.status_msg.as_ref().is_some_and(|(m, err, _)| m.contains("boom") && *err));
        assert_eq!(app.prs.unsent_count(), 1, "and keeps counting it once refused");

        press(&mut app, KeyCode::Enter);
        assert_eq!(compose_text(&app).as_deref(), Some("hello"), "the box has the text back");
        assert_eq!(app.prs.unsent_count(), 1, "counted once, in its box");
        press(&mut app, KeyCode::Esc);
        assert!(app.mode == Mode::Confirm, "closing the box asks first");
    }

    #[test]
    fn a_comment_posted_after_leaving_its_pr_is_no_longer_counted() {
        let mut app = app_on_a_pr_with_a_run_check();
        list_the_open_pr(&mut app);
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "hello");
        ctrl_s(&mut app);
        let pr = app.prs.open.as_ref().unwrap().pr.clone();
        press(&mut app, KeyCode::Esc);
        app.apply(DataMsg::Pr(PrMsg::ActDone { pr, kind: PrActionKind::Comment, result: Ok(()) }));
        assert_eq!(app.prs.unsent_count(), 0);
        press(&mut app, KeyCode::Enter);
        assert!(compose_text(&app).is_none());
    }

    #[test]
    fn a_posted_comment_clears_what_was_kept_and_reads_the_pr_again() {
        let mut app = app_on_a_pr_with_a_run_check();
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "hello");
        ctrl_s(&mut app);
        let pr = app.prs.open.as_ref().unwrap().pr.clone();
        app.pending.clear();

        app.apply(DataMsg::Pr(PrMsg::ActDone { pr, kind: PrActionKind::Comment, result: Ok(()) }));
        let view = app.prs.open.as_ref().unwrap();
        assert!(view.compose.is_none() && app.prs.outbox.is_empty());
        assert!(app.status_msg.as_ref().is_some_and(|(m, err, _)| m == "Comment posted" && !*err));
        assert!(matches!(&app.pending[..], [Command::Pr(PrCommand::FetchDetail(_))]));
    }

    #[test]
    fn an_action_result_also_reads_loaded_files_again() {
        let mut app = app_on_a_pr_with_a_run_check();
        let pr = app.prs.open.as_ref().unwrap().pr.clone();
        app.prs.open.as_mut().unwrap().files.set(Ok(vec![]));
        app.apply(DataMsg::Pr(PrMsg::ActDone { pr, kind: PrActionKind::UpdateBranch, result: Ok(()) }));
        assert!(matches!(
            &app.pending[..],
            [Command::Pr(PrCommand::FetchDetail(_)), Command::Pr(PrCommand::FetchFiles(_))]
        ));
    }

    #[test]
    fn escape_with_text_asks_before_discarding() {
        let mut app = app_on_a_pr_with_a_run_check();
        press(&mut app, KeyCode::Char('c'));
        press(&mut app, KeyCode::Esc);
        assert!(app.mode == Mode::Prs, "an empty box closes without asking");
        assert!(app.prs.open.as_ref().unwrap().compose.is_none());

        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "x");
        press(&mut app, KeyCode::Esc);
        assert!(app.mode == Mode::Confirm);
        assert!(matches!(app.pending_action, Some(PendingAction::DiscardCompose)));
        press(&mut app, KeyCode::Char('n'));
        assert!(app.mode == Mode::Prs);
        let compose = app.prs.open.as_ref().unwrap().compose.as_ref().expect("the box stays");
        assert_eq!(compose.text(), "x");

        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('y'));
        assert!(app.mode == Mode::Prs);
        assert!(app.prs.open.as_ref().unwrap().compose.is_none());
    }

    #[test]
    fn the_box_takes_keys_that_mean_more_elsewhere() {
        let mut app = app_on_a_pr_with_a_run_check();
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "qD1é");
        press(&mut app, KeyCode::Enter);
        type_text(&mut app, "two");
        assert!(app.mode == Mode::Prs && app.prs.open.is_some());
        assert_eq!(app.prs.open.as_ref().unwrap().compose.as_ref().unwrap().text(), "qD1é\ntwo");
        assert!(app.pending_action.is_none());
    }

    #[test]
    fn confirm_returns_to_the_screen_it_came_from() {
        let mut app = App::new(&crate::config::Config::default());
        let mut run = run_in("org/api", 1, "failure");
        run.status = "in_progress".into();
        run.conclusion = None;
        app.apply(DataMsg::Runs { repo: "org/api".into(), runs: vec![run] });
        press(&mut app, KeyCode::Char('c'));
        assert!(app.mode == Mode::Confirm, "cancel asks first");
        press(&mut app, KeyCode::Char('n'));
        assert!(app.mode == Mode::Normal);

        let mut app = app_on_a_pr_with_a_run_check();
        press(&mut app, KeyCode::Char('D'));
        assert!(app.mode == Mode::Confirm);
        press(&mut app, KeyCode::Char('y'));
        assert!(app.mode == Mode::Prs);
    }

    #[test]
    fn toggling_draft_asks_then_sends() {
        let mut app = app_on_a_pr_with_a_run_check();
        press(&mut app, KeyCode::Char('D'));
        assert_eq!(app.pending_action.as_ref().map(|a| a.prompt()).as_deref(), Some("Convert to draft?"));
        press(&mut app, KeyCode::Char('y'));
        assert!(matches!(
            &app.pending[..],
            [Command::Pr(PrCommand::Act { action: PrAction::SetDraft { node_id, draft: true }, .. })] if node_id == "PR_7"
        ));

        edit_detail(&mut app, |d| d.draft = true);
        app.pending.clear();
        press(&mut app, KeyCode::Char('D'));
        assert_eq!(app.pending_action.as_ref().map(|a| a.prompt()).as_deref(), Some("Mark ready for review?"));
    }

    #[test]
    fn update_branch_is_only_offered_when_behind() {
        let mut app = app_on_a_pr_with_a_run_check();
        edit_detail(&mut app, |d| d.merge_status = MergeStatus::Clean);
        press(&mut app, KeyCode::Char('u'));
        assert!(app.mode == Mode::Prs && app.pending.is_empty());
        assert!(app.status_msg.is_some());

        edit_detail(&mut app, |d| d.merge_status = MergeStatus::Behind);
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(app.pending_action.as_ref().map(|a| a.prompt()).as_deref(), Some("Update branch from release?"));
        press(&mut app, KeyCode::Char('y'));
        assert!(matches!(
            &app.pending[..],
            [Command::Pr(PrCommand::Act { action: PrAction::UpdateBranch { head_oid }, .. })] if head_oid == "abc"
        ));
    }

    #[test]
    fn rerun_failed_collects_the_distinct_failed_runs() {
        let mut app = app_on_a_pr_with_a_run_check();
        press(&mut app, KeyCode::Char('x'));
        assert!(app.mode == Mode::Prs && app.status_msg.is_some(), "nothing failed");

        edit_detail(&mut app, |d| {
            let check = |name: &str, state, run_id| Check {
                name: name.into(),
                state,
                target: CheckTarget::ActionsRun { run_id },
            };
            d.checks = vec![
                check("a", CheckState::Failed, 10),
                check("b", CheckState::Failed, 10),
                check("c", CheckState::Failed, 11),
                check("d", CheckState::Passed, 12),
            ];
        });
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.pending_action.as_ref().map(|a| a.prompt()).as_deref(), Some("Re-run failed checks?"));
        press(&mut app, KeyCode::Char('y'));
        assert!(matches!(
            &app.pending[..],
            [Command::Pr(PrCommand::Act { action: PrAction::RerunFailed { run_ids }, .. })] if run_ids == &[10, 11]
        ));
    }

    fn merge_dialog(app: &App) -> &MergeDialog {
        app.prs.open.as_ref().unwrap().merge_dialog.as_ref().expect("the merge dialog is open")
    }

    #[test]
    fn a_fork_pr_offers_no_branch_deletion() {
        let mut app = app_on_a_pr_with_a_run_check();
        edit_detail(&mut app, |d| {
            d.queue = QueueState::NoQueue;
            d.head.repo = "fork/api".into();
        });
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(merge_dialog(&app).delete_branch, None);
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(merge_dialog(&app).delete_branch, None, "d does nothing for a fork");

        let mut app = app_on_a_pr_with_a_run_check();
        edit_detail(&mut app, |d| d.queue = QueueState::NoQueue);
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(merge_dialog(&app).delete_branch, Some(false));
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(merge_dialog(&app).delete_branch, Some(true));
    }

    #[test]
    fn merge_offers_only_allowed_methods_default_first() {
        let mut app = app_on_a_pr_with_a_run_check();
        edit_detail(&mut app, |d| {
            d.queue = QueueState::NoQueue;
            d.merge_methods = vec![MergeMethod::Squash, MergeMethod::Rebase];
        });
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(merge_dialog(&app).methods, [MergeMethod::Squash, MergeMethod::Rebase]);
        assert_eq!(merge_dialog(&app).sel, 0);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Enter);
        assert!(app.prs.open.as_ref().unwrap().merge_dialog.is_none());
        assert!(matches!(
            &app.pending[..],
            [Command::Pr(PrCommand::Act {
                action: PrAction::Merge { method: MergeMethod::Rebase, head_oid, delete_branch: Some(branch) },
                ..
            })] if head_oid == "abc" && branch == "main"
        ));
    }

    #[test]
    fn a_refresh_while_the_box_is_open_does_not_change_the_commit_merged() {
        let mut app = app_on_a_pr_with_a_run_check();
        edit_detail(&mut app, |d| d.queue = QueueState::NoQueue);
        press(&mut app, KeyCode::Char('m'));
        edit_detail(&mut app, |d| d.head.oid = "newer".into());
        press(&mut app, KeyCode::Enter);
        assert!(matches!(
            &app.pending[..],
            [Command::Pr(PrCommand::Act { action: PrAction::Merge { head_oid, .. }, .. })] if head_oid == "abc"
        ));
    }

    #[test]
    fn a_queue_question_keeps_the_commit_it_was_asked_about() {
        let mut app = app_on_a_pr_with_a_run_check();
        edit_detail(&mut app, |d| d.queue = QueueState::NotQueued);
        press(&mut app, KeyCode::Char('m'));
        edit_detail(&mut app, |d| d.head.oid = "newer".into());
        press(&mut app, KeyCode::Char('y'));
        assert!(matches!(
            &app.pending[..],
            [Command::Pr(PrCommand::Act { action: PrAction::Enqueue { head_oid, .. }, .. })] if head_oid == "abc"
        ));
    }

    #[test]
    fn a_fork_is_recognised_whatever_the_case_of_its_name() {
        let mut app = app_on_a_pr_with_a_run_check();
        edit_detail(&mut app, |d| {
            d.queue = QueueState::NoQueue;
            d.head.repo = "Org/API".into();
        });
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(merge_dialog(&app).delete_branch, Some(false));
    }

    #[test]
    fn a_merge_finishing_after_the_view_closed_refreshes_the_list() {
        let mut app = app_on_a_pr_with_a_run_check();
        let pr = app.prs.open.as_ref().unwrap().pr.clone();
        app.prs.open = None;
        app.apply(DataMsg::Pr(PrMsg::ActDone { pr, kind: PrActionKind::Merge, result: Ok(()) }));
        assert!(matches!(&app.pending[..], [Command::Pr(PrCommand::FetchList)]));
    }

    #[test]
    fn the_merge_dialog_takes_every_key_and_esc_closes_it() {
        let mut app = app_on_a_pr_with_a_run_check();
        edit_detail(&mut app, |d| d.queue = QueueState::NoQueue);
        press(&mut app, KeyCode::Char('m'));
        press(&mut app, KeyCode::Char('q'));
        press(&mut app, KeyCode::Char('c'));
        assert!(app.mode == Mode::Prs && app.prs.open.is_some());
        assert!(app.prs.open.as_ref().unwrap().compose.is_none());
        press(&mut app, KeyCode::Esc);
        assert!(app.prs.open.as_ref().unwrap().merge_dialog.is_none());
        assert!(app.pending.is_empty());
    }

    #[test]
    fn a_draft_or_conflicting_pr_cannot_be_merged() {
        let mut app = app_on_a_pr_with_a_run_check();
        edit_detail(&mut app, |d| {
            d.queue = QueueState::NoQueue;
            d.draft = true;
        });
        press(&mut app, KeyCode::Char('m'));
        assert!(app.prs.open.as_ref().unwrap().merge_dialog.is_none() && app.mode == Mode::Prs);
        assert!(app.status_msg.as_ref().is_some_and(|(_, err, _)| *err));

        app.status_msg = None;
        edit_detail(&mut app, |d| {
            d.draft = false;
            d.merge_status = MergeStatus::Conflicts;
        });
        press(&mut app, KeyCode::Char('m'));
        assert!(app.prs.open.as_ref().unwrap().merge_dialog.is_none() && app.mode == Mode::Prs);
        assert!(app.status_msg.as_ref().is_some_and(|(_, err, _)| *err));
        assert!(app.pending.is_empty());
    }

    #[test]
    fn a_queue_repo_queues_instead_of_merging() {
        let mut app = app_on_a_pr_with_a_run_check();
        edit_detail(&mut app, |d| d.queue = QueueState::NotQueued);
        press(&mut app, KeyCode::Char('m'));
        assert!(app.prs.open.as_ref().unwrap().merge_dialog.is_none());
        assert_eq!(
            app.pending_action.as_ref().map(|a| a.prompt()).as_deref(),
            Some("Add org/api#7 to the merge queue?")
        );
        press(&mut app, KeyCode::Char('y'));
        assert!(matches!(
            &app.pending[..],
            [Command::Pr(PrCommand::Act { action: PrAction::Enqueue { node_id, head_oid }, .. })]
                if node_id == "PR_7" && head_oid == "abc"
        ));
    }

    #[test]
    fn a_queued_pr_can_be_taken_out_of_the_queue() {
        let mut app = app_on_a_pr_with_a_run_check();
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(
            app.pending_action.as_ref().map(|a| a.prompt()).as_deref(),
            Some("Remove org/api#7 from the merge queue?")
        );
        press(&mut app, KeyCode::Char('y'));
        assert!(matches!(
            &app.pending[..],
            [Command::Pr(PrCommand::Act { action: PrAction::Dequeue { node_id }, .. })] if node_id == "PR_7"
        ));
    }

    #[test]
    fn a_refused_merge_shows_why_and_refreshes() {
        let mut app = app_on_a_pr_with_a_run_check();
        let pr = app.prs.open.as_ref().unwrap().pr.clone();
        app.apply(DataMsg::Pr(PrMsg::ActDone {
            pr,
            kind: PrActionKind::Merge,
            result: Err("Head branch was modified".into()),
        }));
        assert!(app.status_msg.as_ref().is_some_and(|(m, err, _)| m.contains("Head branch was modified") && *err));
        assert!(matches!(&app.pending[..], [Command::Pr(PrCommand::FetchDetail(_))]));
    }

    #[test]
    fn a_pr_in_both_groups_lists_once_under_review_requested() {
        let mut list = PrList::default();
        list.set(PrGroup::ReviewRequested, Ok(vec![summary("org/api", 1)]));
        list.set(PrGroup::Authored, Ok(vec![summary("org/api", 1), summary("me/tool", 2)]));
        let prs: Vec<String> = list
            .rows()
            .iter()
            .filter_map(|r| match r {
                PrRow::Pr(p) => Some(p.pr.label()),
                _ => None,
            })
            .collect();
        assert_eq!(prs, ["org/api#1", "me/tool#2"]);
        assert_eq!(list.waiting(), 1);
    }

    #[test]
    fn a_refresh_keeps_the_cursor_on_its_pr() {
        let mut list = PrList::default();
        list.set(PrGroup::Authored, Ok(vec![summary("a/a", 1), summary("a/a", 2)]));
        list.move_sel(1);
        list.set(PrGroup::Authored, Ok(vec![summary("a/a", 3), summary("a/a", 1), summary("a/a", 2)]));
        assert_eq!(list.selected().unwrap().pr.number, 2);
    }

    #[test]
    fn a_failed_refresh_keeps_what_was_shown() {
        let mut list = PrList::default();
        list.set(PrGroup::Authored, Ok(vec![summary("a/a", 1)]));
        list.set(PrGroup::Authored, Err("502".into()));
        assert!(matches!(list.authored, Loadable::Ready(ref v) if v.len() == 1));
    }

    #[test]
    fn the_cursor_steps_over_headers() {
        let mut list = PrList::default();
        list.set(PrGroup::ReviewRequested, Ok(vec![summary("a/a", 1)]));
        list.set(PrGroup::Authored, Ok(vec![summary("a/a", 2)]));
        assert_eq!(list.selected().unwrap().pr.number, 1);
        list.move_sel(1);
        assert_eq!(list.selected().unwrap().pr.number, 2);
        list.move_sel(5);
        assert_eq!(list.selected().unwrap().pr.number, 2);
        list.move_sel(-5);
        assert_eq!(list.selected().unwrap().pr.number, 1);
    }

    #[test]
    fn headers_say_what_each_list_is_doing() {
        let mut list = PrList::default();
        list.set(PrGroup::ReviewRequested, Err("boom".into()));
        list.set(PrGroup::Authored, Ok(vec![]));
        let details: Vec<String> = list
            .rows()
            .into_iter()
            .filter_map(|r| match r {
                PrRow::Header { detail, .. } => Some(detail),
                _ => None,
            })
            .collect();
        assert_eq!(details, ["boom", "none"]);
        assert_eq!(PrList::default().rows().len(), 2);
    }

    fn with_draft(app: &mut App) {
        use crate::pulls::model::{DraftComment, LineSpan, Side};
        let pr = app.prs.open.as_ref().unwrap().pr.clone();
        let comment = DraftComment { path: "a.rs".into(), side: Side::Right, span: LineSpan::One(1), body: "why?".into() };
        app.prs.drafts.insert(pr, PendingReview { head_oid: "abc".into(), comments: vec![comment], replies: vec![] });
    }

    #[test]
    fn quitting_with_unsent_comments_asks_first() {
        let mut app = app_on_a_pr_with_a_run_check();
        with_draft(&mut app);
        press(&mut app, KeyCode::Char('q'));
        assert!(app.mode == Mode::Normal, "q leaves the PR screen first");
        assert!(app.mode == Mode::Normal);
        press(&mut app, KeyCode::Char('q'));
        assert!(app.mode == Mode::Confirm);
        assert!(!app.should_quit);
        let prompt = app.pending_action.as_ref().unwrap().prompt();
        assert_eq!(prompt, "Quit and lose 1 unsent review comment?");

        press(&mut app, KeyCode::Char('n'));
        assert!(app.mode == Mode::Normal && !app.should_quit);
        press(&mut app, KeyCode::Char('q'));
        press(&mut app, KeyCode::Char('y'));
        assert!(app.should_quit);

        let mut app = app_on_a_pr_with_a_run_check();
        with_draft(&mut app);
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), crossterm::event::KeyModifiers::CONTROL));
        assert!(app.mode == Mode::Confirm && !app.should_quit);
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), crossterm::event::KeyModifiers::CONTROL));
        assert!(app.should_quit, "a second Ctrl+C quits");
    }

    #[test]
    fn the_quit_question_counts_drafts_and_open_boxes() {
        let mut app = app_on_a_pr_with_a_run_check();
        with_draft(&mut app);
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "hi");
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), crossterm::event::KeyModifiers::CONTROL));
        assert_eq!(app.pending_action.as_ref().unwrap().prompt(), "Quit and lose 2 unsent review comments?");
    }

    #[test]
    fn quitting_without_drafts_is_immediate() {
        let mut app = App::new(&crate::config::Config::default());
        press(&mut app, KeyCode::Char('q'));
        assert!(app.should_quit);
        let mut app = app_on_a_pr_with_a_run_check();
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), crossterm::event::KeyModifiers::CONTROL));
        assert!(app.should_quit);
    }
}
