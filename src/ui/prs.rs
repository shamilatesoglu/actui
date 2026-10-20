//! The PRs list: your review requests and your own open PRs, one row each.
//! Borrows the palette accessors and `pane` from the parent `ui` module.

mod conversation;
mod diff;
mod help;
mod overlays;
mod overview;
#[cfg(test)]
mod tests;

use super::*;
use crate::app::{App, Loadable, PendingReview, PrRow, PrTab, PrView, PrsState, ReviewPopup, Scroll};
use crate::pulls::model::{
    Check, CheckState, CheckTarget, ChecksState, MergeStatus, PrSummary, QueueState, ReviewDecision,
};
use conversation::{conversation, draw_conversation};
use diff::draw_files;
use overview::draw_overview;
pub(super) use help::pr_help;
pub(super) use overlays::{draw_compose, draw_merge_dialog, draw_review_popup};
use ratatui::buffer::Buffer;
use ratatui::widgets::Widget;

/// Narrowest the title column shrinks to before it stops making room.
const MIN_TITLE: usize = 12;

pub(super) fn draw_prs_pane(f: &mut Frame, app: &mut App, area: Rect) {
    let inner = pane(f, area, "Pull requests", true);
    app.hit.prs_pane = inner;
    // The highlight marker takes the first column.
    let width = inner.width.saturating_sub(1) as usize;
    let spin = SPINNER[app.spinner];
    let drafts = &app.prs.drafts;
    let items: Vec<ListItem> = app
        .prs
        .list
        .rows()
        .iter()
        .map(|row| match row {
            PrRow::Header { group, detail } => ListItem::new(Line::from(vec![
                Span::styled(group.title(), Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(detail.clone(), Style::default().fg(dim())),
            ])),
            PrRow::Pr(pr) => {
                let has_draft = drafts.get(&pr.pr).is_some_and(|d| !d.is_empty());
                ListItem::new(pr_line(pr, width, spin, has_draft))
            }
        })
        .collect();
    let list = List::new(items).highlight_style(select_style(true)).highlight_symbol("▌");
    f.render_stateful_widget(list, inner, &mut app.prs.list.state);
}

/// The open PR: its title, the tab strip and the tab's body, across the
/// whole body area.
pub(super) fn draw_open_pr(f: &mut Frame, app: &mut App, area: Rect) {
    // The list is hidden, so its rows take no clicks.
    app.hit.prs_pane = Rect::default();
    let spin = SPINNER[app.spinner];
    let PrsState { open, drafts, .. } = &mut app.prs;
    let Some(view) = open.as_mut() else { return };
    let review = drafts.get(&view.pr).filter(|d| !d.is_empty());
    let inner = pane(f, area, "Pull request", true).inner(Margin { horizontal: 1, vertical: 0 });
    let [title_row, tabs_row, _, body] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);

    // The overview draws from the detail and moves its own scroll state, so
    // the detail is held apart from the view for the frame.
    let detail = std::mem::take(&mut view.detail);
    let mut title = vec![Span::styled(view.pr.label(), Style::default().fg(accent()).add_modifier(Modifier::BOLD))];
    if let Loadable::Ready(d) = &detail {
        title.push(Span::raw("  "));
        if d.draft {
            title.push(Span::styled("draft ", Style::default().fg(dim())));
        }
        title.push(Span::raw(d.title.clone()));
    }
    f.render_widget(Paragraph::new(Line::from(title)), title_row);
    let pending = review.map_or(0, PendingReview::len);
    f.render_widget(Paragraph::new(tab_strip(view.tab, &detail, &view.files.files, pending)), tabs_row);

    // The files come from their own read, so the tab shows without the detail.
    if view.tab == PrTab::Files {
        draw_files(f, view, body, spin, review.map_or(&[], |r| &r.comments));
        view.detail = detail;
        return;
    }
    match &detail {
        Loadable::Loading => {
            let line = Line::styled(format!("{spin} loading…"), Style::default().fg(dim()));
            f.render_widget(Paragraph::new(line), body);
        }
        Loadable::Failed(e) => {
            let line = Line::styled(e.clone(), Style::default().fg(Color::Red));
            f.render_widget(Paragraph::new(line).wrap(Wrap { trim: false }), body);
        }
        Loadable::Ready(d) => match view.tab {
            PrTab::Overview => draw_overview(f, view, d, body, spin),
            PrTab::Conversation => draw_conversation(f, &mut view.conversation, d, body),
            // Drawn above, before the detail matters.
            PrTab::Files => {}
        },
    }
    view.detail = detail;
}

/// Draws `lines` wrapped in `area`, scrolled by `scroll`, and records in
/// `scroll` how far the text can go so the last line can be reached.
fn draw_scrolling_text(
    f: &mut Frame,
    scroll: &mut Scroll,
    build: impl FnOnce() -> Vec<Line<'static>>,
    area: Rect,
) {
    let (lines, rows) = scroll.lines(area.width, build, |lines| wrapped_rows(lines, area.width));
    let lines = lines.to_vec();
    scroll.fit(rows, area.height);
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((scroll.offset(), 0)), area);
}

/// The most rows measured in one scratch buffer, which bounds its size.
const MEASURE_ROWS: usize = 2048;

/// The most rows a line can take wrapped to `width`: even breaking every word
/// mid-way takes under two rows per width's worth.
fn most_rows(line: &Line, width: u16) -> usize {
    2 * line.width().div_ceil(width as usize) + 1
}

/// How many rows `lines` take once wrapped to `width` the way the paragraph
/// wraps them, up to the last row with something on it. Counted by drawing
/// into a scratch buffer, since the word breaking is the paragraph's own.
/// Each line wraps on its own, so the lines are drawn a few at a time and
/// the buffer stays small however long the text is.
fn wrapped_rows(lines: &[Line], width: u16) -> usize {
    if width == 0 {
        return 0;
    }
    let mut total = 0;
    let (mut start, mut most) = (0, 0);
    for (i, line) in lines.iter().enumerate() {
        // Scrolling stops at u16::MAX rows, so there is no use counting on.
        if total >= u16::MAX as usize {
            return total;
        }
        let line_most = most_rows(line, width);
        if line_most > MEASURE_ROWS {
            // Too long for the scratch buffer: its most keeps its end reachable.
            total += rows_before_mark(&lines[start..i], width) + line_most;
            (start, most) = (i + 1, 0);
        } else if most + line_most > MEASURE_ROWS {
            total += rows_before_mark(&lines[start..i], width);
            (start, most) = (i, line_most);
        } else {
            most += line_most;
        }
    }
    total + rows_drawn(&lines[start..], width)
}

/// Rows `lines` take, empty ones at the end included: a one-cell line drawn
/// after them shows where they end.
fn rows_before_mark(lines: &[Line], width: u16) -> usize {
    if lines.is_empty() {
        return 0;
    }
    let mut marked = lines.to_vec();
    marked.push(Line::raw("·"));
    rows_drawn(&marked, width) - 1
}

/// Rows `lines` take, up to the last row with something on it.
fn rows_drawn(lines: &[Line], width: u16) -> usize {
    let most: usize = lines.iter().map(|l| most_rows(l, width)).sum();
    let area = Rect::new(0, 0, width, most.min(u16::MAX as usize) as u16);
    let mut buf = Buffer::empty(area);
    Paragraph::new(lines.to_vec()).wrap(Wrap { trim: false }).render(area, &mut buf);
    (0..area.height)
        .rev()
        .find(|&y| (0..width).any(|x| buf[(x, y)].symbol() != " "))
        .map_or(0, |y| y as usize + 1)
}

/// `Overview  Files  Conversation {n}`, the open tab highlighted, then how
/// many comments the pending review holds, if there is one.
fn tab_strip(
    open: PrTab,
    detail: &Loadable<Box<crate::pulls::model::PrDetail>>,
    files: &Loadable<Vec<crate::pulls::model::PrFile>>,
    pending: usize,
) -> Line<'static> {
    let mut spans = Vec::new();
    for tab in PrTab::ALL {
        // Files has no count until its tab loads the file list.
        let count = match (tab, detail) {
            (PrTab::Files, _) => match files {
                Loadable::Ready(files) => Some(files.len()),
                _ => None,
            },
            (PrTab::Conversation, Loadable::Ready(d)) => {
                // The description is the first entry and is not counted.
                Some(conversation(d).len() - 1)
            }
            _ => None,
        };
        let label = match count {
            Some(n) => format!(" {} {n} ", tab.title()),
            None => format!(" {} ", tab.title()),
        };
        let style = if tab == open {
            Style::default().fg(on_accent()).bg(accent()).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(dim())
        };
        spans.push(Span::styled(label, style));
    }
    if pending > 0 {
        spans.push(Span::styled(format!("   review: {pending} pending"), Style::default().fg(Color::Yellow)));
    }
    Line::from(spans)
}

fn checks_glyph(checks: ChecksState, spin: &'static str) -> (&'static str, Color) {
    match checks {
        ChecksState::Passing => ("●", Color::Green),
        ChecksState::Failing => ("●", Color::Red),
        ChecksState::Pending => (spin, Color::Yellow),
        ChecksState::NoChecks => ("·", dim()),
    }
}

fn review_text(review: ReviewDecision) -> Option<(&'static str, Color)> {
    match review {
        ReviewDecision::Approved => Some(("approved", Color::Green)),
        ReviewDecision::ChangesRequested => Some(("changes requested", Color::Red)),
        ReviewDecision::ReviewRequired => Some(("review required", Color::Yellow)),
        ReviewDecision::NotRequired => None,
    }
}

/// `{checks} {repo}#{number}{✎}  {title}  {author}  {age}  {review}`, with the
/// title taking whatever width the rest leaves. `✎` shows when the PR has
/// review comments not sent yet.
fn pr_line(pr: &PrSummary, width: usize, spin: &'static str, has_draft: bool) -> Line<'static> {
    let (glyph, glyph_color) = checks_glyph(pr.checks, spin);
    let repo = pr.pr.repo.rsplit('/').next().unwrap_or(&pr.pr.repo);
    let id = format!("{repo}#{}{}", pr.pr.number, if has_draft { "✎" } else { "" });
    let age = fmt_age(pr.updated_at);
    let review = review_text(pr.review);
    let draft = if pr.draft { "draft " } else { "" };

    let after = 2 + pr.author.chars().count() + 2 + age.chars().count()
        + review.map_or(0, |(text, _)| 2 + text.chars().count());
    let before = 2 + id.chars().count() + 2 + draft.chars().count();
    let title_width = width.saturating_sub(before + after).max(MIN_TITLE);
    let title = truncate(&pr.title, title_width);
    let pad = " ".repeat(title_width.saturating_sub(title.chars().count()));

    let mut spans = vec![
        Span::styled(glyph, Style::default().fg(glyph_color)),
        Span::raw(format!(" {id}  ")),
        Span::styled(draft, Style::default().fg(dim())),
        Span::raw(format!("{title}{pad}")),
        Span::styled(format!("  {}  {age}", pr.author), Style::default().fg(dim())),
    ];
    if let Some((text, color)) = review {
        spans.push(Span::styled(format!("  {text}"), Style::default().fg(color)));
    }
    Line::from(spans)
}

/// The footer's hints for an open PR, whose pending review is `review`.
pub(super) fn open_pr_hints(view: &PrView, review: Option<&PendingReview>) -> Vec<String> {
    if let Some(compose) = &view.compose {
        return owned(compose.target.keys());
    }
    if let Some(dialog) = &view.merge_dialog {
        let mut hints = owned(&["j/k method"]);
        if dialog.delete_branch.is_some() {
            hints.push("d delete branch".to_string());
        }
        hints.extend(owned(&["⏎ merge", "Esc cancel"]));
        return hints;
    }
    if let Some(popup) = &view.review_popup {
        return review_popup_hints(view, popup);
    }
    let mut hints = match (view.tab, &view.files.diff) {
        (PrTab::Files, Some(diff)) if diff.searching => return owned(&["type to search", "⏎ keep", "Esc drop"]),
        (PrTab::Files, Some(diff)) => {
            let mut hints = owned(&["j/k move", if diff.mark.is_some() { "v clear mark" } else { "v mark" }, "c comment"]);
            if diff.thread_rows.contains_key(&diff.cursor) {
                hints.push("⏎ thread".to_string());
            }
            hints.extend(owned(&["/ search", "n/N next match", "g/G ends", "h back to files"]));
            hints
        }
        (PrTab::Files, None) => owned(&["Tab next tab", "j/k move", "⏎ open diff", "o open on GitHub", "r refresh"]),
        (PrTab::Conversation, _) => {
            owned(&["Tab next tab", "j/k scroll", "c comment", "PgUp/PgDn page", "g/G ends", "o open on GitHub", "r refresh"])
        }
        _ => owned(&["Tab next tab", "j/k scroll", "J/K checks", "c comment", "D draft", "o open on GitHub", "r refresh"]),
    };
    // Keys that only apply some of the time are listed when they do.
    if let (Loadable::Ready(detail), true) = (&view.detail, view.tab != PrTab::Files) {
        let at = hints.len() - 2;
        if !detail.draft && detail.merge_status != MergeStatus::Conflicts {
            let key = match detail.queue {
                QueueState::NoQueue if detail.merge_methods.is_empty() => "",
                QueueState::NoQueue => "m merge",
                QueueState::NotQueued => "m queue",
                QueueState::Queued { .. } => "m unqueue",
            };
            if !key.is_empty() {
                hints.insert(at, key.to_string());
            }
        }
        if detail.merge_status == MergeStatus::Behind {
            hints.insert(at, "u update branch".to_string());
        }
        let failed = |c: &&Check| c.state == CheckState::Failed && matches!(c.target, CheckTarget::ActionsRun { .. });
        if detail.checks.iter().any(|c| failed(&c)) {
            hints.insert(at, "x re-run failed".to_string());
        }
        if view.tab == PrTab::Conversation && !detail.threads.is_empty() {
            hints.insert(at, "⏎ threads".to_string());
        }
    }
    if review.is_some() {
        hints.push("p pending".to_string());
    }
    hints.push("R review".to_string());
    hints
}

fn review_popup_hints(view: &PrView, popup: &ReviewPopup) -> Vec<String> {
    match popup {
        ReviewPopup::Thread(thread) => {
            let mut hints = owned(&["j/k scroll"]);
            if thread.thread_ids.len() > 1 {
                hints.push("n/N thread".to_string());
            }
            let resolved = match &view.detail {
                Loadable::Ready(d) => d.threads.iter().any(|t| Some(&t.id) == thread.thread_ids.get(thread.sel) && t.resolved),
                _ => false,
            };
            hints.extend(owned(&["r reply", if resolved { "x unresolve" } else { "x resolve" }, "Esc close"]));
            hints
        }
        ReviewPopup::Pending { .. } => owned(&["j/k move", "e edit", "d drop", "R submit", "Esc close"]),
        ReviewPopup::Verdict { .. } => owned(&["j/k verdict", "⏎ write message", "Esc cancel"]),
    }
}
