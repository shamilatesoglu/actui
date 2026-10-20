//! Keys and clicks on the PRs screens.

use super::PrCommand;
use crate::app::{App, Command, Filter, Mode};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Position;

impl App {
    /// Opens the PRs list and reads it again.
    pub(in crate::app) fn open_prs(&mut self) {
        self.mode = Mode::Prs;
        self.pending.push(Command::Pr(PrCommand::FetchList));
    }

    fn close_prs(&mut self) {
        self.mode = Mode::Normal;
    }

    pub(in crate::app) fn key_prs(&mut self, key: KeyEvent) {
        // Rows moved by the paging keys.
        const PAGE: i32 = 10;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q' | 'P') | KeyCode::Backspace => self.close_prs(),
            // A filter tab takes you back to the runs it filters.
            KeyCode::Char(c @ '1'..='5') => {
                self.close_prs();
                self.set_filter(Filter::ALL[c as usize - '1' as usize]);
            }
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
