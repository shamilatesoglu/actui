//! The multi-line box for writing to a PR, and the keys that run it.

use super::{PrAction, PrActionKind, PrCommand};
use crate::app::{App, Command, PendingAction};
use crate::pulls::model::PrRef;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_textarea::TextArea;

/// What a box is for, which decides where its text goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ComposeTarget {
    PrComment,
}

impl ComposeTarget {
    pub fn title(self) -> &'static str {
        match self {
            ComposeTarget::PrComment => "Comment on",
        }
    }

    /// The change that sending this text makes.
    pub fn sends(self) -> PrActionKind {
        match self {
            ComposeTarget::PrComment => PrActionKind::Comment,
        }
    }

    fn action(self, text: String) -> PrAction {
        match self {
            ComposeTarget::PrComment => PrAction::Comment { body: text },
        }
    }
}

pub struct Compose {
    pub target: ComposeTarget,
    pub editor: TextArea<'static>,
}

impl Compose {
    pub fn new(target: ComposeTarget, text: &str) -> Self {
        let mut editor = TextArea::from(text.lines());
        editor.move_cursor(tui_textarea::CursorMove::Bottom);
        editor.move_cursor(tui_textarea::CursorMove::End);
        Compose { target, editor }
    }

    pub fn text(&self) -> String {
        self.editor.lines().join("\n")
    }
}

impl App {
    /// Opens a box on the open PR.
    pub(super) fn open_compose(&mut self, target: ComposeTarget) {
        let Some(view) = &mut self.prs.open else { return };
        if view.inflight.is_some() {
            self.set_status("Still sending your last message", true);
            return;
        }
        view.compose = Some(Compose::new(target, ""));
    }

    /// Takes a key while a box is open. Everything but Ctrl+S and Esc is
    /// typing, so keys that mean more elsewhere stay in the box.
    pub(super) fn key_compose(&mut self, key: KeyEvent) {
        let Some(view) = &mut self.prs.open else { return };
        let Some(compose) = &mut view.compose else { return };
        match key.code {
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => self.send_compose(),
            KeyCode::Esc if compose.editor.is_empty() => view.compose = None,
            KeyCode::Esc => self.ask(PendingAction::DiscardCompose),
            _ => {
                compose.editor.input(key);
            }
        }
    }

    fn send_compose(&mut self) {
        let Some(view) = &mut self.prs.open else { return };
        let Some(compose) = view.compose.take() else { return };
        let (target, text) = (compose.target, compose.text());
        if text.trim().is_empty() {
            view.compose = Some(compose);
            self.set_status("Write something first", true);
            return;
        }
        let pr: PrRef = view.pr.clone();
        view.inflight = Some((target, text.clone()));
        self.pending.push(Command::Pr(PrCommand::Act { pr, action: target.action(text) }));
        self.set_status("Sending…", false);
    }

    /// Closes the box without sending (the user confirmed the discard).
    pub(in crate::app) fn discard_compose(&mut self) {
        if let Some(view) = &mut self.prs.open {
            view.compose = None;
        }
    }
}
