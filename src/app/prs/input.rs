//! Keys and clicks on the PRs screens.

use super::{Loadable, PrCommand, PrTab, PrView};
use crate::pulls::model::CheckTarget;
use crate::app::{App, Command, Filter, Mode};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Position;

impl App {
    /// Opens the PRs list and reads it again.
    pub(in crate::app) fn open_prs(&mut self) {
        self.end_run_jump();
        self.mode = Mode::Prs;
        self.pending.push(Command::Pr(PrCommand::FetchList));
    }

    fn close_prs(&mut self) {
        self.prs.open = None;
        self.mode = Mode::Normal;
    }

    fn open_pr(&mut self) {
        if let Some(pr) = self.prs.list.selected().map(|p| p.pr.clone()) {
            self.pending.push(Command::Pr(PrCommand::FetchDetail(pr.clone())));
            self.prs.open = Some(PrView::new(pr));
        }
    }

    /// Moves the list cursor, or scrolls the open PR's text (the mouse wheel).
    pub(in crate::app) fn move_prs(&mut self, delta: i32) {
        match &mut self.prs.open {
            Some(view) => view.scroll_overview(delta),
            None => self.prs.list.move_sel(delta),
        }
    }

    pub(in crate::app) fn key_prs(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q' | 'P') => self.close_prs(),
            // A filter tab takes you back to the runs it filters.
            KeyCode::Char(c @ '1'..='5') => {
                self.close_prs();
                self.set_filter(Filter::ALL[c as usize - '1' as usize]);
            }
            _ if self.prs.open.is_some() => self.key_open_pr(key),
            _ => self.key_pr_list(key),
        }
    }

    fn key_open_pr(&mut self, key: KeyEvent) {
        let Some(view) = &mut self.prs.open else { return };
        match key.code {
            KeyCode::Esc | KeyCode::Backspace => self.prs.open = None,
            KeyCode::Tab => view.cycle_tab(1),
            KeyCode::BackTab => view.cycle_tab(-1),
            KeyCode::Char('o') => {
                let url = view.pr.url();
                self.pending.push(Command::OpenUrl(url));
            }
            KeyCode::Char('r') => self.queue_pr_refresh(),
            KeyCode::Char('j') | KeyCode::Down if view.tab == PrTab::Overview => view.scroll_overview(1),
            KeyCode::Char('k') | KeyCode::Up if view.tab == PrTab::Overview => view.scroll_overview(-1),
            KeyCode::Char('J') if view.tab == PrTab::Overview => view.move_check(1),
            KeyCode::Char('K') if view.tab == PrTab::Overview => view.move_check(-1),
            KeyCode::Enter if view.tab == PrTab::Overview => self.open_selected_check(),
            _ => {}
        }
    }

    /// Acts on the check picked with J/K: its Actions run opens on the runs
    /// screen, any other page opens in the browser.
    fn open_selected_check(&mut self) {
        let Some(view) = &self.prs.open else { return };
        let Loadable::Ready(detail) = &view.detail else { return };
        let Some(check) = view.checks.selected().and_then(|i| detail.checks.get(i)) else { return };
        let (repo, target) = (view.pr.repo.clone(), check.target.clone());
        match target {
            CheckTarget::ActionsRun { run_id } => self.jump_to_run(repo, run_id),
            CheckTarget::Link(url) => self.pending.push(Command::OpenUrl(url)),
            CheckTarget::Nothing => self.set_status("That check has no page to open", true),
        }
    }

    fn key_pr_list(&mut self, key: KeyEvent) {
        // Rows moved by the paging keys.
        const PAGE: i32 = 10;
        match key.code {
            KeyCode::Esc | KeyCode::Backspace => self.close_prs(),
            KeyCode::Enter => self.open_pr(),
            KeyCode::Char('r') => self.pending.push(Command::Pr(PrCommand::FetchList)),
            KeyCode::Char('o') => {
                if let Some(pr) = self.prs.list.selected() {
                    let url = pr.pr.url();
                    self.pending.push(Command::OpenUrl(url));
                }
            }
            KeyCode::Char('j') | KeyCode::Down => self.prs.list.move_sel(1),
            KeyCode::Char('k') | KeyCode::Up => self.prs.list.move_sel(-1),
            KeyCode::PageDown => self.prs.list.move_sel(PAGE),
            KeyCode::PageUp => self.prs.list.move_sel(-PAGE),
            KeyCode::Char('g') | KeyCode::Home => self.prs.list.jump(true),
            KeyCode::Char('G') | KeyCode::End => self.prs.list.jump(false),
            _ => {}
        }
    }

    pub(in crate::app) fn click_prs(&mut self, x: u16, y: u16) {
        let pos = Position::new(x, y);
        if self.hit.prs_tab.contains(pos) {
            self.close_prs();
        } else if self.hit.runners_tab.contains(pos) {
            self.close_prs();
            self.open_runners();
        } else if self.hit.timing_tab.contains(pos) {
            self.close_prs();
            self.open_timing();
        } else if let Some(filt) = self.filter_tab_at(pos) {
            self.close_prs();
            self.set_filter(filt);
        } else if self.hit.repos.contains(pos) {
            // The sidebar is still up beside the list, so a click there takes
            // you back to the runs.
            self.click_repo_row(y);
            self.close_prs();
        } else if self.hit.prs_pane.contains(pos) {
            self.prs.list.click_row((y - self.hit.prs_pane.y) as usize);
        }
    }
}
