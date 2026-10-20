//! Boxes drawn over the PR screens.

use super::conversation::{thread_lines, thread_place};
use super::*;
use crate::app::{Compose, ThreadPopup};
use crate::pulls::model::{blocked_reason, PrDetail, PrRef, ReviewEvent};

/// The merge box, over whatever the PR screen shows.
pub(in crate::ui) fn draw_merge_dialog(f: &mut Frame, app: &mut App) {
    if !app.shows_prs() {
        return;
    }
    let Some(view) = app.prs.open.as_ref() else { return };
    let (Some(dialog), Loadable::Ready(detail)) = (&view.merge_dialog, &view.detail) else { return };
    let inner = popup(f, centered(60, 50, f.area()), &format!("Merge {}", view.pr.label()), accent());
    let [body, hint_area] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);

    let mut lines = Vec::new();
    if let Some(reason) = blocked_reason(detail) {
        lines.push(Line::from(Span::styled(
            format!("Blocked: {reason}. Merging works only if you can bypass branch protection."),
            Style::default().fg(soft_red()),
        )));
        lines.push(Line::default());
    }
    for (i, method) in dialog.methods.iter().enumerate() {
        let picked = i == dialog.sel;
        let style = if picked { Style::default().fg(accent()).add_modifier(Modifier::BOLD) } else { Style::default() };
        lines.push(Line::from(Span::styled(format!("{} {}", if picked { "›" } else { " " }, method.title()), style)));
    }
    if let Some(on) = dialog.delete_branch {
        lines.push(Line::default());
        lines.push(Line::from(format!("[{}] Delete branch {} after merging", if on { "x" } else { " " }, detail.head.branch)));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);

    let delete = if dialog.delete_branch.is_some() { " · d delete branch" } else { "" };
    f.render_widget(
        Paragraph::new(Span::styled(format!("j/k method{delete} · ⏎ merge · Esc cancel"), Style::default().fg(dim()))),
        hint_area,
    );
}

/// The comment box, over whatever the PR screen shows.
pub(in crate::ui) fn draw_compose(f: &mut Frame, app: &mut App) {
    if !app.shows_prs() {
        return;
    }
    let Some(view) = app.prs.open.as_mut() else { return };
    let Some(Compose { target, editor }) = view.compose.as_mut() else { return };
    let title = target.title(&view.pr);
    let inner = popup(f, centered(70, 50, f.area()), &title, accent());
    let [text_area, hint_area] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    editor.set_style(Style::default().fg(text()).bg(popup_bg()));
    editor.set_cursor_line_style(Style::default());
    f.render_widget(&*editor, text_area);
    hint(f, &target.keys().join(" · "), hint_area);
}

fn hint(f: &mut Frame, text: &str, area: Rect) {
    f.render_widget(Paragraph::new(Span::styled(text.to_string(), Style::default().fg(dim()))), area);
}

/// The open review box: a diff row's threads, the pending review, or the
/// verdict picker.
pub(in crate::ui) fn draw_review_popup(f: &mut Frame, app: &mut App) {
    if !app.shows_prs() {
        return;
    }
    let PrsState { open, drafts, .. } = &mut app.prs;
    let Some(view) = open.as_mut() else { return };
    let review = drafts.get(&view.pr).filter(|d| !d.is_empty());
    let detail = match &view.detail {
        Loadable::Ready(d) => Some(&**d),
        _ => None,
    };
    match &mut view.review_popup {
        Some(ReviewPopup::Thread(thread)) => draw_thread_popup(f, thread, detail, review),
        Some(ReviewPopup::Pending { sel }) => draw_pending_panel(f, *sel, &view.pr, detail, review),
        Some(ReviewPopup::Verdict { sel }) => draw_verdict_picker(f, *sel, &view.pr, detail, review),
        None => {}
    }
}

fn draw_thread_popup(f: &mut Frame, thread: &mut ThreadPopup, detail: Option<&PrDetail>, review: Option<&PendingReview>) {
    let count = thread.thread_ids.len();
    let shown = thread.thread_ids.get(thread.sel);
    let found = detail.zip(shown).and_then(|(d, id)| d.threads.iter().find(|t| &t.id == id));
    let title = if count > 1 { format!("Review thread {} of {count}", thread.sel + 1) } else { "Review thread".to_string() };
    let inner = popup(f, centered(70, 60, f.area()), &title, accent());
    let [body, hint_area] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);

    let mut lines = Vec::new();
    match found {
        Some(t) => {
            thread_lines(t, &mut lines);
            let mine = review.into_iter().flat_map(|r| &r.replies).filter(|r| r.thread_id == t.id);
            for reply in mine {
                lines.push(Line::default());
                lines.push(Line::styled("  you  reply in your pending review", Style::default().fg(Color::Yellow)));
                lines.extend(reply.body.lines().map(|l| Line::raw(format!("  {l}"))));
            }
        }
        None => lines.push(Line::styled("This thread is gone.", Style::default().fg(dim()))),
    }
    let rows = wrapped_rows(&lines, body.width);
    thread.scroll.fit(rows, body.height);
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((thread.scroll.offset(), 0)), body);

    let resolve = if found.is_some_and(|t| t.resolved) { "x unresolve" } else { "x resolve" };
    let next = if count > 1 { " · n/N thread" } else { "" };
    hint(f, &format!("j/k scroll{next} · r reply · {resolve} · Esc close"), hint_area);
}

fn draw_pending_panel(f: &mut Frame, sel: usize, pr: &PrRef, detail: Option<&PrDetail>, review: Option<&PendingReview>) {
    let inner = popup(f, centered(70, 50, f.area()), &format!("Pending review of {}", pr.label()), accent());
    let [body, hint_area] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    let Some(review) = review else {
        let text = "No comments yet. In a diff, c comments on a line and Ctrl+S adds it here.";
        f.render_widget(Paragraph::new(text).style(Style::default().fg(dim())).wrap(Wrap { trim: false }), body);
        hint(f, "R review · Esc close", hint_area);
        return;
    };
    let thread_place = |id: &str| {
        let thread = detail.and_then(|d| d.threads.iter().find(|t| t.id == id));
        match thread {
            Some(t) => format!("reply on {}", thread_place(t)),
            None => "reply".to_string(),
        }
    };
    let items = review
        .comments
        .iter()
        .map(|c| (c.place(), c.body.as_str()))
        .chain(review.replies.iter().map(|r| (thread_place(&r.thread_id), r.body.as_str())));
    // The list scrolls so the selected item stays in view.
    let skip = sel.saturating_sub((body.height as usize).saturating_sub(1));
    let lines: Vec<Line> = items
        .enumerate()
        .skip(skip)
        .map(|(i, (place, text))| {
            let picked = i == sel;
            let first = text.lines().next().unwrap_or("");
            let line = Line::from(vec![
                Span::raw(if picked { "› " } else { "  " }),
                Span::styled(place, Style::default().fg(accent())),
                Span::raw(format!("  {first}")),
            ]);
            if picked { line.style(select_style(true)) } else { line }
        })
        .collect();
    f.render_widget(Paragraph::new(lines), body);
    hint(f, "j/k move · e edit · d drop · R submit · Esc close", hint_area);
}

fn draw_verdict_picker(
    f: &mut Frame,
    sel: usize,
    pr: &PrRef,
    detail: Option<&PrDetail>,
    review: Option<&PendingReview>,
) {
    let inner = popup(f, centered(50, 40, f.area()), &format!("Review {}", pr.label()), accent());
    let [body, hint_area] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    let mut lines: Vec<Line> = ReviewEvent::ALL
        .iter()
        .enumerate()
        .map(|(i, event)| {
            let picked = i == sel;
            let style = if picked { Style::default().fg(accent()).add_modifier(Modifier::BOLD) } else { Style::default() };
            Line::from(Span::styled(format!("{} {}", if picked { "›" } else { " " }, event.title()), style))
        })
        .collect();
    lines.push(Line::default());
    let sends = match review {
        Some(r) => {
            let counts = [(r.comments.len(), "comment", "comments"), (r.replies.len(), "reply", "replies")];
            let parts: Vec<String> =
                counts.iter().filter(|(n, ..)| *n > 0).map(|&(n, one, many)| plural(n, one, many)).collect();
            format!("Sends {} with the verdict.", parts.join(" and "))
        }
        None => "No pending comments; the verdict goes with its message only.".to_string(),
    };
    lines.push(Line::styled(sends, Style::default().fg(dim())));
    // The drafts stay on the commit they were written against.
    if review.zip(detail).is_some_and(|(r, d)| r.head_oid != d.head.oid) {
        lines.push(Line::default());
        lines.push(Line::styled(
            "The branch has new commits since you started this review.",
            Style::default().fg(soft_red()),
        ));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
    hint(f, "j/k verdict · ⏎ write message · Esc cancel", hint_area);
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}
