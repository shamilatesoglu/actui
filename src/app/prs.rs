//! The pull request screens' state. Key handling is in `input`, the worker
//! protocol in `protocol`.

mod input;
mod protocol;

pub use protocol::{PrCommand, PrMsg};

use super::{App, Command};
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

/// Everything the PRs screens show.
#[derive(Default)]
pub struct PrsState {
    pub list: PrList,
    /// The PR opened from the list, if any.
    pub open: Option<PrView>,
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
    use crate::pulls::model::{ChecksState, ReviewDecision};
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
