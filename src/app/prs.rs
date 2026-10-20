//! The pull request screens' state. Key handling is in `input`, the worker
//! protocol in `protocol`.

mod diff;
mod input;
mod protocol;

pub use diff::{thread_rows, DiffLine, DiffView, FilesState};
pub use protocol::{PrCommand, PrMsg};

use super::{App, Command, CursorHold, Focus, Mode};
use crate::github::Run;
use crate::pulls::model::{CheckState, PrDetail, PrGroup, PrRef, PrSummary};
use ratatui::widgets::ListState;

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

/// A PR opened from the list.
pub struct PrView {
    pub pr: PrRef,
    pub detail: Loadable<Box<PrDetail>>,
    pub tab: PrTab,
    pub overview_scroll: u16,
    /// The most the overview text scrolls, as of the last frame.
    pub overview_max_scroll: u16,
    pub checks: ListState,
    pub files: FilesState,
}

impl PrView {
    fn new(pr: PrRef) -> Self {
        PrView {
            pr,
            detail: Loadable::Loading,
            tab: PrTab::Overview,
            overview_scroll: 0,
            overview_max_scroll: 0,
            checks: ListState::default(),
            files: FilesState::default(),
        }
    }

    pub fn cycle_tab(&mut self, delta: i32) {
        let at = PrTab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0) as i32;
        self.tab = PrTab::ALL[(at + delta).rem_euclid(PrTab::ALL.len() as i32) as usize];
    }

    pub fn scroll_overview(&mut self, delta: i32) {
        let next = (self.overview_scroll as i32 + delta).clamp(0, self.overview_max_scroll as i32);
        self.overview_scroll = next as u16;
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
    /// Reads the open PR again; the refresh timer calls this.
    pub fn queue_pr_refresh(&mut self) {
        if let Some(view) = &self.prs.open {
            self.pending.push(Command::Pr(PrCommand::FetchDetail(view.pr.clone())));
            // The files are read again once they have been asked for.
            if view.tab == PrTab::Files || matches!(view.files.files, Loadable::Ready(_)) {
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{DataMsg, Filter, Mode};
    use crate::pulls::model::{ChecksState, ReviewDecision};
    use crossterm::event::{KeyCode, KeyEvent};
    use chrono::Utc;

    fn summary(repo: &str, n: u64) -> PrSummary {
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
    fn app_on_a_pr_with_a_run_check() -> App {
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

    fn press(app: &mut App, code: KeyCode) {
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
}
