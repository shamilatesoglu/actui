//! The PRs list: your review requests and your own open PRs, one row each.
//! Borrows the palette accessors and `pane` from the parent `ui` module.

use super::*;
use crate::app::{App, PrRow};
use crate::pulls::model::{ChecksState, PrSummary, ReviewDecision};

/// Narrowest the title column shrinks to before it stops making room.
const MIN_TITLE: usize = 12;

pub(super) fn draw_prs_pane(f: &mut Frame, app: &mut App, area: Rect) {
    let inner = pane(f, area, "Pull requests", true);
    app.hit.prs_pane = inner;
    // The highlight marker takes the first column.
    let width = inner.width.saturating_sub(1) as usize;
    let spin = SPINNER[app.spinner];
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
            PrRow::Pr(pr) => ListItem::new(pr_line(pr, width, spin)),
        })
        .collect();
    let list = List::new(items).highlight_style(select_style(true)).highlight_symbol("▌");
    f.render_stateful_widget(list, inner, &mut app.prs.list.state);
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

/// `{checks} {repo}#{number}  {title}  {author}  {age}  {review}`, with the
/// title taking whatever width the rest leaves.
fn pr_line(pr: &PrSummary, width: usize, spin: &'static str) -> Line<'static> {
    let (glyph, glyph_color) = checks_glyph(pr.checks, spin);
    let repo = pr.pr.repo.rsplit('/').next().unwrap_or(&pr.pr.repo);
    let id = format!("{repo}#{}", pr.pr.number);
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
