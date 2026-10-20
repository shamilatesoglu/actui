//! Reviewing a PR: line comments collected into a pending review, replies to
//! and resolving of review threads, the pending-review panel, the verdict
//! picker, and submitting the review.

use super::{ComposeTarget, Loadable, PrAction, PrCommand, PrTab, PrsState, Scroll};
use crate::app::{App, Command};
use crate::pulls::model::{DraftComment, DraftReply, LineSpan, PrRef, ReviewEvent, ReviewSubmission, Side};
use crossterm::event::{KeyCode, KeyEvent};

/// A PR's review being written: what goes to GitHub when it is submitted.
/// It lives in actui's memory only.
#[derive(Default)]
pub struct PendingReview {
    pub comments: Vec<DraftComment>,
    pub replies: Vec<DraftReply>,
}

impl PendingReview {
    pub fn is_empty(&self) -> bool {
        self.comments.is_empty() && self.replies.is_empty()
    }

    pub fn len(&self) -> usize {
        self.comments.len() + self.replies.len()
    }
}

/// A box over the open PR for reviewing. One is open at a time.
pub enum ReviewPopup {
    /// The review threads on one diff row.
    Thread(ThreadPopup),
    /// The pending review's comments then replies; `sel` indexes both in turn.
    Pending { sel: usize },
    /// The verdict a review is submitted with; `sel` indexes `ReviewEvent::ALL`.
    Verdict { sel: usize },
}

pub struct ThreadPopup {
    /// Looked up in the detail on every frame, so a refresh shows new replies.
    pub thread_ids: Vec<String>,
    /// Index into `thread_ids` of the thread shown.
    pub sel: usize,
    pub scroll: Scroll,
}

impl PrsState {
    /// The PR's pending review, if it has one.
    pub fn review_of(&self, pr: &PrRef) -> Option<&PendingReview> {
        self.drafts.get(pr).filter(|d| !d.is_empty())
    }

    /// Whether anything typed would be lost by quitting: a pending review, a
    /// box with text in it, or a message sent and not yet answered.
    #[allow(dead_code)] // the quit guard uses it
    pub fn has_unsent(&self) -> bool {
        let typed = |text: &str| !text.trim().is_empty();
        self.drafts.values().any(|d| !d.is_empty())
            || self.open.as_ref().is_some_and(|v| {
                v.compose.as_ref().is_some_and(|c| typed(&c.text()))
                    || v.inflight.as_ref().is_some_and(|(_, text)| typed(text))
            })
    }
}

/// Moves `sel` by `delta` within `len` items.
fn step(sel: usize, delta: i32, len: usize) -> usize {
    (sel as i64 + delta as i64).clamp(0, len.saturating_sub(1) as i64) as usize
}

impl App {
    /// Review keys on an open PR: `p` and `R` on any tab, and `v`, `c` and
    /// Enter in a diff. Returns whether the key was used.
    pub(super) fn key_review(&mut self, key: KeyEvent) -> bool {
        let Some(view) = &mut self.prs.open else { return false };
        match key.code {
            KeyCode::Char('p') => view.review_popup = Some(ReviewPopup::Pending { sel: 0 }),
            KeyCode::Char('R') => view.review_popup = Some(ReviewPopup::Verdict { sel: 0 }),
            _ if view.tab != PrTab::Files || view.files.diff.is_none() => return false,
            KeyCode::Char('v') => self.toggle_mark(),
            KeyCode::Char('c') => self.compose_line_comment(),
            KeyCode::Enter => return self.open_thread_popup(),
            _ => return false,
        }
        true
    }

    /// `v`: marks the cursor row as the start of a range, or clears the mark.
    fn toggle_mark(&mut self) {
        let Some(diff) = self.prs.open.as_mut().and_then(|v| v.files.diff.as_mut()) else { return };
        if diff.mark.take().is_some() {
            return;
        }
        if diff.lines.get(diff.cursor).and_then(|row| row.anchor()).is_some() {
            diff.mark = Some(diff.cursor);
        } else {
            self.set_status("Only code lines can be commented on", true);
        }
    }

    /// `c` in a diff: a comment on the cursor row, or on the marked range up
    /// to it.
    fn compose_line_comment(&mut self) {
        let Some(view) = &mut self.prs.open else { return };
        let Some(path) = view.files.shown_file().map(|f| f.path.clone()) else { return };
        let Some(diff) = &mut view.files.diff else { return };
        let anchor_at = |row: usize| diff.lines.get(row).and_then(|r| r.anchor());
        let Some((side, line)) = anchor_at(diff.cursor) else {
            self.set_status("Only code lines can be commented on", true);
            return;
        };
        let span = match diff.mark.and_then(anchor_at) {
            None => LineSpan::One(line),
            Some((mark_side, _)) if mark_side != side => {
                self.set_status("A range has to stay on one side of the diff", true);
                return;
            }
            Some((_, mark_line)) => LineSpan::new(mark_line, line),
        };
        self.open_compose(ComposeTarget::LineComment { path, side, span });
        // The box holds the range now.
        if let Some(view) = self.prs.open.as_mut().filter(|v| v.compose.is_some()) {
            if let Some(diff) = &mut view.files.diff {
                diff.mark = None;
            }
        }
    }

    /// Enter in a diff: shows the review threads on the cursor row, if any.
    fn open_thread_popup(&mut self) -> bool {
        let Some(view) = &mut self.prs.open else { return false };
        let (Some(diff), Loadable::Ready(detail)) = (&view.files.diff, &view.detail) else { return false };
        let Some(found) = diff.thread_rows.get(&diff.cursor) else { return false };
        let thread_ids: Vec<String> = found.iter().filter_map(|&i| detail.threads.get(i)).map(|t| t.id.clone()).collect();
        if thread_ids.is_empty() {
            return false;
        }
        view.review_popup = Some(ReviewPopup::Thread(ThreadPopup { thread_ids, sel: 0, scroll: Scroll::default() }));
        true
    }

    /// Keys while a review box is open; it takes all of them.
    pub(super) fn key_review_popup(&mut self, key: KeyEvent) {
        let Some(view) = &mut self.prs.open else { return };
        let Some(popup) = &mut view.review_popup else { return };
        if key.code == KeyCode::Esc {
            view.review_popup = None;
            return;
        }
        match popup {
            ReviewPopup::Thread(thread) => match key.code {
                KeyCode::Char('j') | KeyCode::Down => thread.scroll.by(1),
                KeyCode::Char('k') | KeyCode::Up => thread.scroll.by(-1),
                KeyCode::Char(c @ ('n' | 'N')) => {
                    let count = thread.thread_ids.len() as i32;
                    let delta = if c == 'n' { 1 } else { -1 };
                    thread.sel = (thread.sel as i32 + delta).rem_euclid(count.max(1)) as usize;
                    thread.scroll = Scroll::default();
                }
                KeyCode::Char('r') => self.compose_reply(),
                KeyCode::Char('x') => self.toggle_resolved(),
                _ => {}
            },
            ReviewPopup::Pending { sel } => {
                let len = self.prs.drafts.get(&view.pr).map_or(0, PendingReview::len);
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => *sel = step(*sel, 1, len),
                    KeyCode::Char('k') | KeyCode::Up => *sel = step(*sel, -1, len),
                    KeyCode::Char('e') => self.edit_selected_draft(),
                    KeyCode::Char('d') => self.drop_selected_draft(),
                    KeyCode::Char('R') => view.review_popup = Some(ReviewPopup::Verdict { sel: 0 }),
                    _ => {}
                }
            }
            ReviewPopup::Verdict { sel } => match key.code {
                KeyCode::Char('j') | KeyCode::Down => *sel = step(*sel, 1, ReviewEvent::ALL.len()),
                KeyCode::Char('k') | KeyCode::Up => *sel = step(*sel, -1, ReviewEvent::ALL.len()),
                KeyCode::Enter => {
                    let event = ReviewEvent::ALL[*sel];
                    view.review_popup = None;
                    self.open_compose(ComposeTarget::ReviewBody { event });
                }
                _ => {}
            },
        }
    }

    /// The thread the thread popup shows: its id, first comment and whether
    /// it is resolved.
    fn shown_thread(&self) -> Option<(String, Option<u64>, bool)> {
        let view = self.prs.open.as_ref()?;
        let Some(ReviewPopup::Thread(popup)) = &view.review_popup else { return None };
        let Loadable::Ready(detail) = &view.detail else { return None };
        let id = popup.thread_ids.get(popup.sel)?;
        let thread = detail.threads.iter().find(|t| &t.id == id)?;
        Some((thread.id.clone(), thread.comments.first().map(|c| c.id), thread.resolved))
    }

    fn compose_reply(&mut self) {
        match self.shown_thread() {
            Some((thread_id, Some(comment_id), _)) => self.open_compose(ComposeTarget::Reply { thread_id, comment_id }),
            Some((_, None, _)) => self.set_status("That thread has no comment to reply to", true),
            None => self.set_status("That thread is gone", true),
        }
    }

    fn toggle_resolved(&mut self) {
        let Some((thread_id, _, resolved)) = self.shown_thread() else {
            self.set_status("That thread is gone", true);
            return;
        };
        let Some(pr) = self.prs.open.as_ref().map(|v| v.pr.clone()) else { return };
        let action = PrAction::ResolveThread { thread_id, resolved: !resolved };
        self.pending.push(Command::Pr(PrCommand::Act { pr, action }));
        self.set_status(if resolved { "Unresolving…" } else { "Resolving…" }, false);
    }

    fn panel_sel(&self) -> Option<usize> {
        match self.prs.open.as_ref()?.review_popup {
            Some(ReviewPopup::Pending { sel }) => Some(sel),
            _ => None,
        }
    }

    fn edit_selected_draft(&mut self) {
        let (Some(view), Some(sel)) = (&self.prs.open, self.panel_sel()) else { return };
        let Some(review) = self.prs.drafts.get(&view.pr) else { return };
        match review.comments.get(sel) {
            Some(comment) => {
                let body = comment.body.clone();
                self.open_compose_with(ComposeTarget::EditDraft { index: sel }, &body);
            }
            None if sel < review.len() => self.set_status("A reply can't be edited; drop it and reply again", true),
            None => {}
        }
    }

    fn drop_selected_draft(&mut self) {
        let Some(sel) = self.panel_sel() else { return };
        let Some(view) = &mut self.prs.open else { return };
        let Some(review) = self.prs.drafts.get_mut(&view.pr) else { return };
        let comments = review.comments.len();
        if sel < comments {
            review.comments.remove(sel);
        } else if sel - comments < review.replies.len() {
            review.replies.remove(sel - comments);
        } else {
            return;
        }
        let left = review.len();
        if left == 0 {
            self.prs.drafts.remove(&view.pr);
        }
        view.review_popup = Some(ReviewPopup::Pending { sel: sel.min(left.saturating_sub(1)) });
        self.set_status("Dropped from your review", false);
    }

    fn added_to_review_status(&mut self, pr: &PrRef) {
        let n = self.prs.drafts.get(pr).map_or(0, PendingReview::len);
        self.set_status(format!("Added to your review ({n} pending)"), false);
    }

    /// Ctrl+S in a line comment's box: the comment joins the PR's pending
    /// review, starting one if needed.
    pub(super) fn add_draft_comment(&mut self, path: String, side: Side, span: LineSpan, body: String) {
        let Some(pr) = self.prs.open.as_ref().map(|v| v.pr.clone()) else { return };
        self.prs.drafts.entry(pr.clone()).or_default().comments.push(DraftComment { path, side, span, body });
        self.added_to_review_status(&pr);
    }

    /// Ctrl+G in a line comment's box: posts it on its own. A PR with a
    /// pending review takes it into the review instead, as on GitHub.
    pub(super) fn post_line_comment_alone(&mut self) {
        let Some(view) = &mut self.prs.open else { return };
        let Some(compose) = view.compose.take() else { return };
        let ComposeTarget::LineComment { path, side, span } = compose.target.clone() else {
            view.compose = Some(compose);
            return;
        };
        let head_oid = match &view.detail {
            Loadable::Ready(detail) => Some(detail.head.oid.clone()),
            _ => None,
        };
        let pr = view.pr.clone();
        let text = compose.text();
        let refusal = if self.prs.review_of(&pr).is_some() {
            Some("You have a review in progress; Ctrl+S adds to it")
        } else if text.trim().is_empty() {
            Some("Write something first")
        } else {
            None
        };
        let head_oid = match (refusal, head_oid) {
            (None, Some(head_oid)) => head_oid,
            (why, _) => return self.reopen_compose(compose, why.unwrap_or("The pull request is still loading")),
        };
        let comment = DraftComment { path, side, span, body: text.clone() };
        self.send_from_box(compose.target, text, PrAction::LineComment { head_oid, comment });
    }

    /// Ctrl+S in a reply's box: the reply joins the pending review when the
    /// PR has one, and is posted at once otherwise.
    pub(super) fn send_reply(&mut self, thread_id: String, comment_id: u64, body: String) {
        let Some(pr) = self.prs.open.as_ref().map(|v| v.pr.clone()) else { return };
        match self.prs.drafts.get_mut(&pr).filter(|d| !d.is_empty()) {
            Some(review) => {
                review.replies.push(DraftReply { thread_id, body });
                self.added_to_review_status(&pr);
            }
            None => {
                let target = ComposeTarget::Reply { thread_id, comment_id };
                self.send_from_box(target, body.clone(), PrAction::Reply { comment_id, body });
            }
        }
    }

    /// Ctrl+S in the box editing a pending comment: replaces its text.
    pub(super) fn edit_draft(&mut self, index: usize, body: String) -> Result<(), &'static str> {
        let Some(pr) = self.prs.open.as_ref().map(|v| v.pr.clone()) else { return Ok(()) };
        let comment = self.prs.drafts.get_mut(&pr).and_then(|d| d.comments.get_mut(index));
        let Some(comment) = comment else { return Err("That comment is no longer in your review") };
        comment.body = body;
        self.set_status("Comment updated", false);
        Ok(())
    }

    /// Ctrl+S in the review message box: sends the review with every pending
    /// comment and reply, placed on the head commit as of now.
    pub(super) fn send_review(&mut self, event: ReviewEvent, body: String) -> Result<(), &'static str> {
        let Some(view) = &self.prs.open else { return Ok(()) };
        let Loadable::Ready(detail) = &view.detail else { return Err("The pull request is still loading") };
        let (comments, replies) = match self.prs.drafts.get(&view.pr) {
            Some(review) => (review.comments.clone(), review.replies.clone()),
            None => (Vec::new(), Vec::new()),
        };
        // An approval says enough by itself; the other verdicts need words.
        if event != ReviewEvent::Approve && body.trim().is_empty() && comments.is_empty() && replies.is_empty() {
            return Err("Nothing to send");
        }
        let node_id = detail.node_id.clone();
        let submission = ReviewSubmission { head_oid: detail.head.oid.clone(), event, body: body.clone(), comments, replies };
        self.send_from_box(ComposeTarget::ReviewBody { event }, body, PrAction::SubmitReview { node_id, submission });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{app_on_a_pr_with_a_run_check, ctrl_s, edit_detail, press, summary, type_text};
    use super::super::*;
    use super::{PendingReview, ReviewPopup};
    use crate::app::{App, Command, DataMsg, Mode};
    use crate::pulls::model::{
        Comment, DraftComment, DraftReply, FileStatus, LineSpan, PrFile, ReviewEvent, ReviewThread, Side,
    };
    use chrono::Utc;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// Rows: 0 hunk, 1 context 10/10, 2 removed 11, 3 added 11, 4 added 12, 5 context 12/13.
    const PATCH: &str = "@@ -10,3 +10,4 @@\n ctx\n-old\n+new\n+added\n ctx2";

    /// An app with the diff of `src/a.rs` open on PR org/api#7, cursor on row 0.
    fn app_in_a_diff() -> App {
        let mut app = app_on_a_pr_with_a_run_check();
        let view = app.prs.open.as_mut().unwrap();
        view.tab = PrTab::Files;
        view.files.set(Ok(vec![PrFile {
            path: "src/a.rs".into(),
            previous_path: None,
            status: FileStatus::Modified,
            additions: 2,
            deletions: 1,
            patch: Some(PATCH.into()),
        }]));
        press(&mut app, KeyCode::Enter);
        assert!(app.prs.open.as_ref().unwrap().files.diff.is_some());
        app.pending.clear();
        app
    }

    fn go_to_row(app: &mut App, row: usize) {
        press(app, KeyCode::Char('g'));
        for _ in 0..row {
            press(app, KeyCode::Char('j'));
        }
    }

    fn comment_on_row(app: &mut App, row: usize, text: &str) {
        go_to_row(app, row);
        press(app, KeyCode::Char('c'));
        type_text(app, text);
        ctrl_s(app);
    }

    fn ctrl_g(app: &mut App) {
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
    }

    fn pr(app: &App) -> PrRef {
        app.prs.open.as_ref().unwrap().pr.clone()
    }

    fn drafts(app: &App) -> &PendingReview {
        app.prs.drafts.get(&pr(app)).expect("the PR has a pending review")
    }

    fn compose(app: &App) -> Option<&Compose> {
        app.prs.open.as_ref().unwrap().compose.as_ref()
    }

    fn status(app: &App) -> Option<(&str, bool)> {
        app.status_msg.as_ref().map(|(m, err, _)| (m.as_str(), *err))
    }

    /// A thread on `src/a.rs` line 12, right side, whose first comment is 42.
    fn add_thread(app: &mut App) {
        edit_detail(app, |d| {
            d.threads = vec![ReviewThread {
                id: "PRRT_1".into(),
                resolved: false,
                outdated: false,
                path: "src/a.rs".into(),
                line: Some(12),
                start_line: None,
                side: Side::Right,
                comments: vec![Comment { id: 42, author: "bob".into(), body: "why?".into(), created_at: Utc::now() }],
            }];
        });
        app.prs.open.as_mut().unwrap().sync_threads();
    }

    #[test]
    fn line_comments_collect_into_the_pending_review() {
        let mut app = app_in_a_diff();
        comment_on_row(&mut app, 4, "first");
        comment_on_row(&mut app, 2, "second");
        let placed: Vec<(&str, Side, LineSpan, &str)> = drafts(&app)
            .comments
            .iter()
            .map(|c| (c.path.as_str(), c.side, c.span, c.body.as_str()))
            .collect();
        assert_eq!(
            placed,
            [
                ("src/a.rs", Side::Right, LineSpan::One(12), "first"),
                ("src/a.rs", Side::Left, LineSpan::One(11), "second"),
            ]
        );
        assert!(app.pending.is_empty(), "nothing is sent until the review is");
        assert!(compose(&app).is_none());
    }

    #[test]
    fn a_range_spans_from_the_mark_to_the_cursor() {
        let mut app = app_in_a_diff();
        go_to_row(&mut app, 3);
        press(&mut app, KeyCode::Char('v'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('c'));
        assert_eq!(
            compose(&app).map(|c| c.target.clone()),
            Some(ComposeTarget::LineComment {
                path: "src/a.rs".into(),
                side: Side::Right,
                span: LineSpan::Range { start: 11, end: 12 },
            })
        );
    }

    #[test]
    fn a_range_across_sides_is_refused() {
        let mut app = app_in_a_diff();
        press(&mut app, KeyCode::Char('v'));
        assert!(app.prs.open.as_ref().unwrap().files.diff.as_ref().unwrap().mark.is_none(), "a hunk header can't be marked");

        go_to_row(&mut app, 2);
        press(&mut app, KeyCode::Char('v'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('c'));
        assert!(compose(&app).is_none());
        assert_eq!(status(&app), Some(("A range has to stay on one side of the diff", true)));
    }

    #[test]
    fn a_reply_posts_at_once_without_a_review_and_joins_one_with_it() {
        let mut app = app_in_a_diff();
        add_thread(&mut app);
        go_to_row(&mut app, 4);
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.prs.open.as_ref().unwrap().review_popup, Some(ReviewPopup::Thread(_))));
        press(&mut app, KeyCode::Char('r'));
        assert_eq!(
            compose(&app).map(|c| c.target.clone()),
            Some(ComposeTarget::Reply { thread_id: "PRRT_1".into(), comment_id: 42 })
        );
        type_text(&mut app, "ok");
        ctrl_s(&mut app);
        assert!(matches!(
            &app.pending[..],
            [Command::Pr(PrCommand::Act { action: PrAction::Reply { comment_id: 42, body }, .. })] if body == "ok"
        ));
        let pr = pr(&app);
        app.apply(DataMsg::Pr(PrMsg::ActDone { pr, kind: PrActionKind::Reply, result: Ok(()) }));
        app.pending.clear();

        press(&mut app, KeyCode::Esc);
        comment_on_row(&mut app, 4, "note");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('r'));
        type_text(&mut app, "two");
        ctrl_s(&mut app);
        assert_eq!(drafts(&app).replies, [DraftReply { thread_id: "PRRT_1".into(), body: "two".into() }]);
        assert!(app.pending.is_empty(), "the reply waits for the review");
    }

    #[test]
    fn a_single_comment_is_refused_while_a_review_is_in_progress() {
        let mut app = app_in_a_diff();
        go_to_row(&mut app, 4);
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "solo");
        ctrl_g(&mut app);
        assert!(matches!(
            &app.pending[..],
            [Command::Pr(PrCommand::Act { action: PrAction::LineComment { head_oid, comment }, .. })]
                if head_oid == "abc"
                    && *comment == DraftComment {
                        path: "src/a.rs".into(),
                        side: Side::Right,
                        span: LineSpan::One(12),
                        body: "solo".into(),
                    }
        ));
        assert!(!app.prs.drafts.get(&pr(&app)).is_some_and(|d| !d.is_empty()), "no review was started");
        let pr = pr(&app);
        app.apply(DataMsg::Pr(PrMsg::ActDone { pr, kind: PrActionKind::LineComment, result: Ok(()) }));
        app.pending.clear();

        comment_on_row(&mut app, 4, "one");
        go_to_row(&mut app, 2);
        press(&mut app, KeyCode::Char('c'));
        type_text(&mut app, "two");
        ctrl_g(&mut app);
        assert_eq!(compose(&app).map(|c| c.text()).as_deref(), Some("two"), "the box stays open");
        assert_eq!(status(&app), Some(("You have a review in progress; Ctrl+S adds to it", true)));
        assert!(app.pending.is_empty());
    }

    /// Sends the review with the verdict `steps` rows down the picker.
    fn submit(app: &mut App, steps: usize, body: &str) {
        press(app, KeyCode::Char('R'));
        for _ in 0..steps {
            press(app, KeyCode::Char('j'));
        }
        press(app, KeyCode::Enter);
        type_text(app, body);
        ctrl_s(app);
    }

    #[test]
    fn a_failed_review_keeps_every_draft() {
        let mut app = app_in_a_diff();
        add_thread(&mut app);
        comment_on_row(&mut app, 4, "first");
        go_to_row(&mut app, 4);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('r'));
        type_text(&mut app, "agreed");
        ctrl_s(&mut app);
        press(&mut app, KeyCode::Esc);
        let before = (drafts(&app).comments.clone(), drafts(&app).replies.clone());

        submit(&mut app, 1, "fix");
        let sent = match &app.pending[..] {
            [Command::Pr(PrCommand::Act { action: PrAction::SubmitReview { node_id, submission }, .. })] => {
                assert_eq!(node_id, "PR_7");
                submission.clone()
            }
            _ => panic!("expected the review to be sent: {}", app.pending.len()),
        };
        assert_eq!(sent.head_oid, "abc");
        assert_eq!(sent.event, ReviewEvent::RequestChanges);
        assert_eq!(sent.body, "fix");
        assert_eq!((sent.comments.clone(), sent.replies.clone()), before);
        app.pending.clear();

        let pr = pr(&app);
        let result = Err("Pull request head changed".to_string());
        app.apply(DataMsg::Pr(PrMsg::ActDone { pr, kind: PrActionKind::SubmitReview, result }));
        assert_eq!((drafts(&app).comments.clone(), drafts(&app).replies.clone()), before);
        assert!(status(&app).is_some_and(|(m, err)| m.contains("Pull request head changed") && err));
        assert!(app.pending.iter().any(|c| matches!(c, Command::Pr(PrCommand::FetchDetail(_)))));
        assert_eq!(compose(&app).map(|c| c.text()).as_deref(), Some("fix"), "the message comes back");
    }

    #[test]
    fn a_submitted_review_clears_the_drafts() {
        let mut app = app_in_a_diff();
        comment_on_row(&mut app, 4, "first");
        submit(&mut app, 2, "");
        assert!(matches!(
            &app.pending[..],
            [Command::Pr(PrCommand::Act { action: PrAction::SubmitReview { submission, .. }, .. })]
                if submission.event == ReviewEvent::Comment && submission.comments.len() == 1
        ));
        let pr = pr(&app);
        app.apply(DataMsg::Pr(PrMsg::ActDone { pr: pr.clone(), kind: PrActionKind::SubmitReview, result: Ok(()) }));
        assert!(!app.prs.drafts.contains_key(&pr));
        assert_eq!(status(&app), Some(("Review submitted", false)));
    }

    #[test]
    fn an_empty_approval_is_sent_but_an_empty_comment_review_is_not() {
        let mut app = app_on_a_pr_with_a_run_check();
        submit(&mut app, 2, "");
        assert!(app.pending.is_empty());
        assert_eq!(status(&app), Some(("Nothing to send", true)));
        press(&mut app, KeyCode::Esc);
        assert!(compose(&app).is_none());

        submit(&mut app, 0, "");
        assert!(matches!(
            &app.pending[..],
            [Command::Pr(PrCommand::Act { action: PrAction::SubmitReview { submission, .. }, .. })]
                if submission.event == ReviewEvent::Approve && submission.body.is_empty()
        ));
    }

    #[test]
    fn the_review_is_placed_on_the_head_seen_when_it_is_sent() {
        let mut app = app_on_a_pr_with_a_run_check();
        press(&mut app, KeyCode::Char('R'));
        press(&mut app, KeyCode::Enter);
        edit_detail(&mut app, |d| d.head.oid = "newer".into());
        ctrl_s(&mut app);
        assert!(matches!(
            &app.pending[..],
            [Command::Pr(PrCommand::Act { action: PrAction::SubmitReview { submission, .. }, .. })]
                if submission.head_oid == "newer"
        ));
    }

    #[test]
    fn the_panel_edits_and_drops_drafts() {
        let mut app = app_in_a_diff();
        comment_on_row(&mut app, 4, "first");
        comment_on_row(&mut app, 2, "second");
        press(&mut app, KeyCode::Char('p'));
        assert!(matches!(app.prs.open.as_ref().unwrap().review_popup, Some(ReviewPopup::Pending { sel: 0 })));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(compose(&app).map(|c| c.target.clone()), Some(ComposeTarget::EditDraft { index: 1 }));
        assert_eq!(compose(&app).map(|c| c.text()).as_deref(), Some("second"));
        press(&mut app, KeyCode::Esc);
        assert!(compose(&app).is_none() && app.mode == Mode::Prs, "unchanged text closes without asking");

        press(&mut app, KeyCode::Char('e'));
        type_text(&mut app, "!");
        press(&mut app, KeyCode::Esc);
        assert!(app.mode == Mode::Confirm, "changed text asks first");
        press(&mut app, KeyCode::Char('n'));
        ctrl_s(&mut app);
        assert_eq!(drafts(&app).comments[1].body, "second!");

        press(&mut app, KeyCode::Char('k'));
        press(&mut app, KeyCode::Char('d'));
        let bodies: Vec<&str> = drafts(&app).comments.iter().map(|c| c.body.as_str()).collect();
        assert_eq!(bodies, ["second!"]);
        press(&mut app, KeyCode::Char('d'));
        assert!(!app.prs.drafts.contains_key(&pr(&app)), "an empty review is no review");
        assert!(app.pending.is_empty());
    }

    #[test]
    fn x_in_the_thread_popup_resolves_and_unresolves() {
        let mut app = app_in_a_diff();
        add_thread(&mut app);
        go_to_row(&mut app, 4);
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('x'));
        assert!(matches!(
            &app.pending[..],
            [Command::Pr(PrCommand::Act { action: PrAction::ResolveThread { thread_id, resolved: true }, .. })]
                if thread_id == "PRRT_1"
        ));
        app.pending.clear();
        edit_detail(&mut app, |d| d.threads[0].resolved = true);
        press(&mut app, KeyCode::Char('x'));
        assert!(matches!(
            &app.pending[..],
            [Command::Pr(PrCommand::Act { action: PrAction::ResolveThread { resolved: false, .. }, .. })]
        ));
        press(&mut app, KeyCode::Esc);
        assert!(app.prs.open.as_ref().unwrap().review_popup.is_none());
    }

    #[test]
    fn drafts_survive_closing_the_pr() {
        let mut app = app_in_a_diff();
        app.prs.list.set(PrGroup::ReviewRequested, Ok(vec![summary("org/api", 7)]));
        app.prs.list.set(PrGroup::Authored, Ok(vec![]));
        comment_on_row(&mut app, 4, "keep me");
        let pr = pr(&app);

        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        assert!(app.prs.open.is_none(), "back on the list");
        press(&mut app, KeyCode::Esc);
        assert!(app.mode == Mode::Normal, "back on the runs");
        press(&mut app, KeyCode::Char('P'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.prs.open.as_ref().map(|v| v.pr.clone()), Some(pr.clone()));
        assert_eq!(app.prs.drafts[&pr].comments[0].body, "keep me");
        assert!(app.prs.has_unsent());
    }

    #[test]
    fn unsent_text_counts_whether_drafted_typed_or_sending() {
        let mut app = app_on_a_pr_with_a_run_check();
        assert!(!app.prs.has_unsent());
        press(&mut app, KeyCode::Char('c'));
        assert!(!app.prs.has_unsent(), "an empty box holds nothing");
        type_text(&mut app, "hi");
        assert!(app.prs.has_unsent());
        ctrl_s(&mut app);
        assert!(app.prs.has_unsent(), "sent but not answered");
    }
}
