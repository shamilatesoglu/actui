//! The PRs list: your review requests and your own open PRs, one row each.
//! Borrows the palette accessors and `pane` from the parent `ui` module.

mod overview;

use super::*;
use crate::app::{App, Loadable, PrRow, PrTab};
use crate::pulls::model::{ChecksState, PrSummary, ReviewDecision};
use overview::draw_overview;

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

/// The open PR: its title, the tab strip and the tab's body, across the
/// whole body area.
pub(super) fn draw_open_pr(f: &mut Frame, app: &mut App, area: Rect) {
    // The list is hidden, so its rows take no clicks.
    app.hit.prs_pane = Rect::default();
    let spin = SPINNER[app.spinner];
    let Some(view) = app.prs.open.as_mut() else { return };
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
    f.render_widget(Paragraph::new(tab_strip(view.tab, &detail)), tabs_row);

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
            PrTab::Files | PrTab::Conversation => {
                f.render_widget(Paragraph::new(Line::styled("Not built yet", Style::default().fg(dim()))), body);
            }
        },
    }
    view.detail = detail;
}

/// `Overview  Files  Conversation {n}`, the open tab highlighted.
fn tab_strip(open: PrTab, detail: &Loadable<Box<crate::pulls::model::PrDetail>>) -> Line<'static> {
    let mut spans = Vec::new();
    for tab in PrTab::ALL {
        // Files has no count until its tab loads the file list.
        let count = match (tab, detail) {
            (PrTab::Conversation, Loadable::Ready(d)) => {
                Some(d.comments.len() + d.reviews.len() + d.threads.len())
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
