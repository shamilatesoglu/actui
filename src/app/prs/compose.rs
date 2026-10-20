//! The multi-line box for writing to a PR, and the keys that run it.

use super::{PrAction, PrActionKind, PrCommand};
use crate::app::{App, Command, PendingAction};
use crate::pulls::model::{LineSpan, PrRef, ReviewEvent, Side};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_textarea::TextArea;

/// What a box is for, which decides where its text goes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ComposeTarget {
    PrComment,
    /// A comment on diff lines, added to the pending review or posted alone.
    LineComment { path: String, side: Side, span: LineSpan },
    /// A reply to a review thread; `comment_id` is the thread's first comment.
    Reply { thread_id: String, comment_id: u64 },
    /// Rewrites a comment of the pending review; an index into its comments.
    EditDraft { index: usize },
    /// The message a review is submitted with.
    ReviewBody { event: ReviewEvent },
}

impl ComposeTarget {
    /// The box's title, for the PR `pr`.
    pub fn title(&self, pr: &PrRef) -> String {
        let pr = pr.label();
        match self {
            ComposeTarget::PrComment => format!("Comment on {pr}"),
            ComposeTarget::LineComment { path, side, span } => {
                let old = if *side == Side::Left { " (old)" } else { "" };
                format!("Comment on {path}:{}{old}", span.label())
            }
            ComposeTarget::Reply { .. } => format!("Reply on {pr}"),
            ComposeTarget::EditDraft { .. } => format!("Edit review comment on {pr}"),
            ComposeTarget::ReviewBody { event } => format!("{} {pr}", event.title()),
        }
    }

    /// The keys the box takes besides typing.
    pub fn keys(&self) -> &'static [&'static str] {
        match self {
            ComposeTarget::LineComment { .. } => &["Ctrl+S add to review", "Ctrl+G post alone", "Esc cancel"],
            ComposeTarget::EditDraft { .. } => &["Ctrl+S save", "Esc cancel"],
            ComposeTarget::ReviewBody { .. } => &["Ctrl+S submit review", "Esc cancel"],
            ComposeTarget::PrComment | ComposeTarget::Reply { .. } => &["Ctrl+S send", "Esc cancel"],
        }
    }

    /// The change that sending this text makes, for the boxes whose text can
    /// go to GitHub on its own.
    pub fn sends(&self) -> Option<PrActionKind> {
        match self {
            ComposeTarget::PrComment => Some(PrActionKind::Comment),
            ComposeTarget::LineComment { .. } => Some(PrActionKind::LineComment),
            ComposeTarget::Reply { .. } => Some(PrActionKind::Reply),
            ComposeTarget::ReviewBody { .. } => Some(PrActionKind::SubmitReview),
            ComposeTarget::EditDraft { .. } => None,
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
    /// Opens an empty box on the open PR.
    pub(super) fn open_compose(&mut self, target: ComposeTarget) {
        self.open_compose_with(target, "");
    }

    /// Opens a box on the open PR holding `text`.
    pub(super) fn open_compose_with(&mut self, target: ComposeTarget, text: &str) {
        let Some(view) = &mut self.prs.open else { return };
        if view.inflight.is_some() {
            self.set_status("Still sending your last message", true);
            return;
        }
        view.compose = Some(Compose::new(target, text));
    }

    /// Takes a key while a box is open. Everything but the send keys and Esc
    /// is typing, so keys that mean more elsewhere stay in the box.
    pub(super) fn key_compose(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let unchanged = self.compose_unchanged();
        let Some(view) = &mut self.prs.open else { return };
        let Some(compose) = &mut view.compose else { return };
        match key.code {
            KeyCode::Char('s') if ctrl => self.send_compose(),
            KeyCode::Char('g') if ctrl && matches!(compose.target, ComposeTarget::LineComment { .. }) => {
                self.post_line_comment_alone()
            }
            KeyCode::Esc if unchanged => view.compose = None,
            KeyCode::Esc => self.ask(PendingAction::DiscardCompose),
            _ => {
                compose.editor.input(key);
            }
        }
    }

    /// Whether closing the box loses nothing: it is empty, or still holds the
    /// draft it was opened to edit.
    fn compose_unchanged(&self) -> bool {
        let Some(view) = &self.prs.open else { return true };
        let Some(compose) = &view.compose else { return true };
        match compose.target {
            ComposeTarget::EditDraft { index } => self
                .prs
                .drafts
                .get(&view.pr)
                .and_then(|d| d.comments.get(index))
                .is_some_and(|c| c.body == compose.text()),
            _ => compose.editor.is_empty(),
        }
    }

    fn send_compose(&mut self) {
        let Some(view) = &mut self.prs.open else { return };
        let Some(compose) = view.compose.take() else { return };
        let text = compose.text();
        // Only a review may go without a message; its comments can say it all.
        let sent = if text.trim().is_empty() && !matches!(compose.target, ComposeTarget::ReviewBody { .. }) {
            Err("Write something first")
        } else {
            match compose.target.clone() {
                ComposeTarget::PrComment => {
                    self.send_from_box(ComposeTarget::PrComment, text.clone(), PrAction::Comment { body: text });
                    Ok(())
                }
                ComposeTarget::LineComment { path, side, span } => {
                    self.add_draft_comment(path, side, span, text);
                    Ok(())
                }
                ComposeTarget::Reply { thread_id, comment_id } => {
                    self.send_reply(thread_id, comment_id, text);
                    Ok(())
                }
                ComposeTarget::EditDraft { index } => self.edit_draft(index, text),
                ComposeTarget::ReviewBody { event } => self.send_review(event, text),
            }
        };
        if let Err(why) = sent {
            self.reopen_compose(compose, why);
        }
    }

    /// Puts a box back after refusing to send it, saying why.
    pub(super) fn reopen_compose(&mut self, compose: Compose, why: &str) {
        if let Some(view) = &mut self.prs.open {
            view.compose = Some(compose);
        }
        self.set_status(why, true);
    }

    /// Sends a box's text as `action`, keeping the text so a failure can put
    /// it back in a box like it.
    pub(super) fn send_from_box(&mut self, target: ComposeTarget, text: String, action: PrAction) {
        let Some(view) = &mut self.prs.open else { return };
        let pr: PrRef = view.pr.clone();
        view.inflight = Some((target, text));
        self.pending.push(Command::Pr(PrCommand::Act { pr, action }));
        self.set_status("Sending…", false);
    }

    /// Closes the box without sending (the user confirmed the discard).
    pub(in crate::app) fn discard_compose(&mut self) {
        if let Some(view) = &mut self.prs.open {
            view.compose = None;
        }
    }
}
