//! Keys and clicks on the PRs screens.

use super::{DiffView, Loadable, PrCommand, PrTab, PrView};
use crate::pulls::model::CheckTarget;
use crate::app::{App, Command, Filter, Mode};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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
            Some(view) if view.tab == PrTab::Files => match &mut view.files.diff {
                Some(diff) => diff.move_cursor(delta),
                None => view.files.move_sel(delta),
            },
            Some(view) => view.scroll_overview(delta),
            None => self.prs.list.move_sel(delta),
        }
    }

    pub(in crate::app) fn key_prs(&mut self, key: KeyEvent) {
        // A search box takes every key, including the ones that mean more elsewhere.
        if let Some(diff) = self.prs.open.as_mut().and_then(|v| v.files.diff.as_mut()).filter(|d| d.searching) {
            key_search_box(diff, key);
            return;
        }
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
        if self.key_files(key) {
            return;
        }
        let Some(view) = &mut self.prs.open else { return };
        match key.code {
            KeyCode::Esc | KeyCode::Backspace => self.prs.open = None,
            KeyCode::Tab => {
                view.cycle_tab(1);
                self.fetch_files_if_shown();
            }
            KeyCode::BackTab => {
                view.cycle_tab(-1);
                self.fetch_files_if_shown();
            }
            KeyCode::Char('o') => {
                // A file GitHub sends no diff for is read on its Files page.
                let no_diff = view.tab == PrTab::Files && view.files.shown_file().is_some_and(|f| f.patch.is_none());
                let url = if no_diff { format!("{}/files", view.pr.url()) } else { view.pr.url() };
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

    /// Reads the files when the Files tab has just come up without them.
    fn fetch_files_if_shown(&mut self) {
        let Some(view) = &self.prs.open else { return };
        if view.tab == PrTab::Files && !matches!(view.files.files, Loadable::Ready(_)) {
            self.pending.push(Command::Pr(PrCommand::FetchFiles(view.pr.clone())));
        }
    }

    /// Keys on the Files tab: the file list, or the diff once one is open.
    /// Returns whether the key was used.
    fn key_files(&mut self, key: KeyEvent) -> bool {
        // Rows moved by the paging keys in the file list.
        const PAGE: i32 = 10;
        let Some(view) = &mut self.prs.open else { return false };
        if view.tab != PrTab::Files {
            return false;
        }
        let files = &mut view.files;
        if let Some(diff) = &mut files.diff {
            let page = diff.height.saturating_sub(1).max(1) as i32;
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => diff.move_cursor(1),
                KeyCode::Char('k') | KeyCode::Up => diff.move_cursor(-1),
                KeyCode::PageDown => diff.move_cursor(page),
                KeyCode::PageUp => diff.move_cursor(-page),
                KeyCode::Char('g') | KeyCode::Home => diff.move_cursor(i32::MIN),
                KeyCode::Char('G') | KeyCode::End => diff.move_cursor(i32::MAX),
                KeyCode::Char('/') => {
                    diff.set_search("");
                    diff.searching = true;
                }
                KeyCode::Char('n') => diff.next_match(true),
                KeyCode::Char('N') => diff.next_match(false),
                KeyCode::Esc | KeyCode::Char('h') | KeyCode::Backspace => files.diff = None,
                _ => return false,
            }
            return true;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => files.move_sel(1),
            KeyCode::Char('k') | KeyCode::Up => files.move_sel(-1),
            KeyCode::PageDown => files.move_sel(PAGE),
            KeyCode::PageUp => files.move_sel(-PAGE),
            KeyCode::Char('g') | KeyCode::Home => files.move_sel(i32::MIN),
            KeyCode::Char('G') | KeyCode::End => files.move_sel(i32::MAX),
            KeyCode::Enter | KeyCode::Char('l') => files.open_selected(),
            _ => return false,
        }
        true
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

/// Types into the diff's search box. Enter keeps the search and leaves the
/// box, Esc drops the search.
fn key_search_box(diff: &mut DiffView, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => diff.searching = false,
        KeyCode::Esc => {
            diff.set_search("");
            diff.searching = false;
        }
        KeyCode::Backspace => {
            let mut q = diff.search.clone();
            if q.pop().is_none() {
                diff.searching = false;
            }
            diff.set_search(&q);
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            let q = format!("{}{c}", diff.search);
            diff.set_search(&q);
        }
        _ => {}
    }
}
