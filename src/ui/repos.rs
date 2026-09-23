//! The repos sidebar: "All repos" (no scope), then the repos themselves, each
//! with a rollup of its runs. Borrows the palette accessors and the `pane`
//! primitive from the parent `ui` module.

use super::*;
use crate::app::{App, Focus, Scrollable};

pub(super) fn draw_repos(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Repos;
    let at = app.repos.state.selected().unwrap_or(0);
    let title = format!("Repos {}/{}", at + 1, app.repos.len());
    let (running, queued, failed, _) = app.counts();

    let content = pane(f, area, &title, focused);
    app.hit.repos = content;
    // The selection marker takes the first column.
    let w = content.width.saturating_sub(1) as usize;
    let step = app.repos.step;

    let spin = SPINNER[app.spinner];
    let all = rollup(running, queued, failed, None, spin);
    let mut lines = vec![row_line("All repos", false, all, w, step)];
    lines.extend(app.repos.rows.iter().map(|r| {
        let right = rollup(r.running, r.queued, r.failed, r.last, spin);
        row_line(&r.name, r.pinned, right, w, step)
    }));
    // Something is mid-slide, so the clock has an animation to keep running.
    app.repos.sliding = lines.iter().any(|(_, sliding)| *sliding);

    let items: Vec<ListItem> = lines.into_iter().map(|(l, _)| ListItem::new(l)).collect();
    let list = List::new(items)
        .highlight_style(select_style(focused))
        .highlight_symbol(if focused { "▌" } else { " " });
    f.render_stateful_widget(list, content, &mut app.repos.state);

    let (rows, offset) = (app.repos.len(), app.repos.state.offset());
    scrollbar(
        f,
        &mut app.panes,
        Scrollable::Repos,
        area.inner(Margin { vertical: 1, horizontal: 0 }),
        rows,
        content.height as usize,
        offset,
    );
}

/// A repo's counts, right-aligned on its row: running, queued and failed runs
/// when there are any, else how long ago it last ran. Each uses the icon its
/// runs have in the runs list.
fn rollup(
    running: usize,
    queued: usize,
    failed: usize,
    last: Option<DateTime<Utc>>,
    spin: &str,
) -> Vec<Span<'static>> {
    let mut right = Vec::new();
    let counts = [
        (spin, running, Color::Yellow),
        ("○", queued, Color::Cyan),
        ("●", failed, Color::Red),
    ];
    for (icon, n, color) in counts {
        if n > 0 {
            right.push(Span::styled(format!("{icon} {n} "), Style::default().fg(color)));
        }
    }
    if right.is_empty() {
        if let Some(ts) = last {
            right.push(Span::styled(format!("{} ", fmt_age(ts)), Style::default().fg(dim())));
        }
    }
    right
}

/// One row: a pin marker, the repo, and its `rollup` right-aligned. Returns
/// the line and whether the name had to slide to fit.
fn row_line(
    name: &str,
    pinned: bool,
    right: Vec<Span<'static>>,
    width: usize,
    step: usize,
) -> (Line<'static>, bool) {
    let right_w: usize = right.iter().map(|s| s.content.chars().count()).sum();

    // Pin column + a space, then the name padded out to where the rollup starts.
    let name_w = width.saturating_sub(right_w + 2);
    let (label, sliding) = slide(name, name_w, step);
    let pad = " ".repeat(name_w.saturating_sub(label.chars().count()));
    let pin = if pinned { "★" } else { " " };

    let mut spans = vec![
        Span::styled(pin.to_string(), Style::default().fg(accent())),
        Span::raw(format!(" {label}{pad}")),
    ];
    spans.extend(right);
    (Line::from(spans), sliding)
}

/// What separates the end of a sliding name from its start coming round again.
const GAP: &str = " · ";
/// Steps' worth of pause at the start of each pass, so a name can be read from
/// the beginning before it moves.
const HOLD: usize = 6;

/// Fit a repo name into `max` columns: shown whole when it fits, else slid left
/// a column per step and wrapped around, so the whole name goes past instead of
/// being cut off. Returns the visible text and whether it's moving.
fn slide(name: &str, max: usize, step: usize) -> (String, bool) {
    if name.chars().count() <= max || max == 0 {
        return (name.to_string(), false);
    }
    let cycle: Vec<char> = name.chars().chain(GAP.chars()).collect();
    let at = step % (cycle.len() + HOLD);
    let from = at.saturating_sub(HOLD);
    (cycle.iter().cycle().skip(from).take(max).collect(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_that_fits_stays_put() {
        for step in 0..40 {
            assert_eq!(slide("org/api", 20, step), ("org/api".to_string(), false));
        }
    }

    #[test]
    fn a_long_name_holds_then_slides_and_wraps() {
        let name = "shamilatesoglu/actui";
        let at = |step| slide(name, 10, step).0;

        // It opens on the start of the name and holds there.
        assert_eq!(at(0), "shamilates");
        assert_eq!(at(HOLD), "shamilates");
        // Then one column per step.
        assert_eq!(at(HOLD + 1), "hamilateso");
        assert_eq!(at(HOLD + 3), "milatesogl");
        // The cycle repeats exactly, with the gap going past on the way round.
        let period = name.chars().count() + GAP.chars().count() + HOLD;
        assert_eq!(at(period), at(0));
        assert!((0..period).map(at).any(|s| s.contains('·')));
        assert!(slide(name, 10, 0).1, "a name this long reports as sliding");
    }
}
