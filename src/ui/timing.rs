//! The duration chart: one workflow's successful runs as bars, oldest on the
//! left and the newest against the right edge, the median drawn across them
//! and a smoothed curve over them. Borrows the palette accessors, `pane`, and
//! the duration formatting from the parent `ui` module.

use super::*;
use crate::app::{App, TimingLayout, TimingStats, TimingView, TIMING_HISTORY};

/// Eighth-blocks, so a bar can stop part-way up a row.
const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Caps a bar that runs off the top of the axis.
const OVER: char = '↑';

/// Braille dot bits by position in a cell: two columns of four rows.
const DOTS: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

/// Below this the trend is noise, not a change worth an arrow.
const FLAT: i64 = 3;

/// What the vertical axis names: its top, the median line, and its floor.
struct Ticks {
    top: String,
    mid: String,
    bottom: String,
    width: usize,
}

impl Ticks {
    fn of(stats: &TimingStats) -> Self {
        let (top, mid, bottom) =
            (fmt_dur(stats.ceiling), fmt_dur(stats.median), fmt_dur(stats.floor));
        let width = top.chars().count().max(mid.chars().count()).max(bottom.chars().count());
        Self { top, mid, bottom, width }
    }
}

pub(super) fn draw_timing_pane(f: &mut Frame, app: &mut App, area: Rect) {
    let title = match &app.timing {
        Some(tv) => format!("Duration · {} › {}", short_repo(&tv.repo), tv.workflow),
        None => "Duration".to_string(),
    };
    let width = area.width.saturating_sub(4) as usize;
    let inner = pane(f, area, &truncate(&title, width), true);
    app.hit.timing_plot = Rect::default();
    let Some(tv) = &mut app.timing else { return };

    if !tv.loaded {
        note(f, inner, "Reading the workflow's history…".into());
        return;
    }
    if tv.points.is_empty() {
        let why = if tv.unknown > 0 {
            format!(
                "{} has {} successful runs, but they are past the repo's retention and \
                 GitHub no longer says how long any of them took.",
                tv.workflow, tv.unknown
            )
        } else {
            format!("No successful runs of {} in its last {TIMING_HISTORY}.", tv.workflow)
        };
        note(f, inner, format!("{why}\n\n[ and ] move to another workflow in this repo."));
        return;
    }

    let stats = tv.stats().expect("a chart with runs on it has stats");
    let ticks = Ticks::of(&stats);
    // A leading space, the label, the axis glyph — and a column of air on the
    // right so the newest bar doesn't touch the border.
    let pre_w = ticks.width + 2;
    let plot_w = (inner.width as usize).saturating_sub(pre_w + 1);
    let chart_h = inner.height.saturating_sub(3) as usize;

    tv.width = plot_w;
    let mut lines: Vec<Line> = Vec::new();
    if plot_w >= 4 && chart_h >= 2 {
        let l = tv.layout(plot_w);
        let at = tv.bar_at(&l);
        let curve = tv.smoothed();
        lines.extend(chart_rows(&l, &stats, &ticks, &curve, chart_h, plot_w, at));
        lines.push(axis_row(&l, plot_w, at, ticks.width));
        app.hit.timing_plot = Rect {
            x: inner.x + pre_w as u16,
            y: inner.y,
            width: plot_w as u16,
            height: chart_h as u16,
        };
    } else {
        // Too cramped for a chart; the numbers underneath still say everything
        // that fits.
        lines.resize(inner.height.saturating_sub(2) as usize, Line::default());
    }
    lines.push(detail_row(tv, plot_w));
    lines.push(stats_row(tv, &stats));
    f.render_widget(Paragraph::new(lines), inner);
}

/// How seconds map onto the plot's height. The axis starts at the floor rather
/// than at zero, so the height goes on what actually varies; a run at or under
/// the floor still gets a sliver, and one past the ceiling fills its column.
struct Scale {
    floor: i64,
    span: i64,
    /// Eighths of a row in the whole plot.
    full: i64,
}

impl Scale {
    fn of(stats: &TimingStats, chart_h: usize) -> Self {
        Self {
            floor: stats.floor,
            span: (stats.ceiling - stats.floor).max(1),
            full: (chart_h * 8) as i64,
        }
    }

    /// Eighths of a row a bar this long reaches.
    fn eighths(&self, secs: i64) -> i64 {
        if secs <= 0 {
            0
        } else {
            ((secs - self.floor) * self.full / self.span).clamp(1, self.full)
        }
    }

    /// The row (from the top) a bar this long has its top in — where a line
    /// at that height goes.
    fn row(&self, secs: i64) -> usize {
        let rows = (self.full / 8) as usize;
        rows - 1 - (((self.eighths(secs).max(1) - 1) / 8) as usize).min(rows - 1)
    }
}

/// The plot itself, top row first. The median is laid down under the bars, so
/// it shows through wherever a run came in under it; the curve is laid over
/// them.
#[allow(clippy::too_many_arguments)]
fn chart_rows(
    l: &TimingLayout,
    stats: &TimingStats,
    ticks: &Ticks,
    curve: &[i64],
    chart_h: usize,
    plot_w: usize,
    cursor: usize,
) -> Vec<Line<'static>> {
    let scale = Scale::of(stats, chart_h);
    let med_row = scale.row(stats.median);
    let offset = l.offset(plot_w);

    let mut grid: Vec<Vec<(char, Style)>> = Vec::with_capacity(chart_h);
    for r in 0..chart_h {
        // Eighths already covered by the rows below this one.
        let below = ((chart_h - 1 - r) * 8) as i64;
        let mut row = Vec::with_capacity(plot_w);
        for col in 0..plot_w {
            // Which bar's slice this column is in, and whether it is the bar
            // itself or the gap after it.
            let slice = col.checked_sub(offset).map(|rel| (rel / l.step, rel % l.step));
            let bar = slice.and_then(|(i, within)| l.bars.get(i).map(|b| (i, b, within < l.bar_w)));
            let filled = match bar {
                Some((_, b, true)) => (scale.eighths(b.secs) - below).clamp(0, 8),
                _ => 0,
            };
            row.push(if filled > 0 {
                let style = match bar {
                    // The cursor's bar drops the accent for the palette's plain
                    // text colour, so it reads as picked out rather than as one
                    // more blue bar.
                    Some((i, _, _)) if i == cursor => Style::default().fg(text()),
                    _ => Style::default().fg(accent()),
                };
                let over = r == 0 && bar.is_some_and(|(_, b, _)| b.secs > stats.ceiling);
                (if over { OVER } else { BLOCKS[filled as usize - 1] }, style)
            } else if r == med_row {
                ('┄', Style::default().fg(dim()))
            } else {
                (' ', Style::default())
            });
        }
        grid.push(row);
    }
    lay_curve(&mut grid, l, curve, &scale, plot_w);

    grid.into_iter()
        .enumerate()
        .map(|(r, row)| {
            // Top, then the median, then the floor — where two land on one row
            // the median is the one worth naming.
            let (label, tick) = if r == 0 {
                (ticks.top.as_str(), '┤')
            } else if r == med_row {
                (ticks.mid.as_str(), '┤')
            } else if r == chart_h - 1 {
                (ticks.bottom.as_str(), '┤')
            } else {
                ("", '│')
            };
            let mut cells = Cells::new();
            for (ch, style) in row {
                cells.push(ch, style);
            }
            let mut spans = vec![Span::styled(
                format!(" {label:>width$}{tick}", width = ticks.width),
                Style::default().fg(dim()),
            )];
            spans.extend(cells.finish());
            Line::from(spans)
        })
        .collect()
}

/// Draw the smoothed curve over the grid in braille. A cell is two dots wide
/// and four tall, so the line has four heights to choose from per row and a
/// slope between two runs reads as a slope rather than a step. Where it
/// crosses a solid bar the bar's colour moves to the background, so the bar
/// stays a bar with a line across it.
fn lay_curve(grid: &mut [Vec<(char, Style)>], l: &TimingLayout, curve: &[i64], scale: &Scale, plot_w: usize) {
    let rows = grid.len();
    let dots_h = (rows * 4) as i64;
    let mut dots = vec![0u8; rows * plot_w];
    let mut plot = |x: i64, y: i64| {
        if x >= 0 && y >= 0 && (x as usize) < plot_w * 2 && y < dots_h {
            dots[(y as usize / 4) * plot_w + x as usize / 2] |= DOTS[x as usize % 2][y as usize % 4];
        }
    };
    // One vertex per bar, at its middle, at the smoothed height there.
    let vertex = |i: usize| {
        let x = ((l.col(i, plot_w) + l.bar_w / 2) * 2) as i64;
        let eighths = scale.eighths(curve[l.bars[i].middle()]).max(1);
        (x, dots_h - 1 - ((eighths - 1) / 2).min(dots_h - 1))
    };
    let mut from = None;
    for i in 0..l.bars.len() {
        let to = vertex(i);
        match from {
            Some(from) => line(from, to, &mut plot),
            None => plot(to.0, to.1),
        }
        from = Some(to);
    }

    for (idx, &mask) in dots.iter().enumerate() {
        if mask == 0 {
            continue;
        }
        let cell = &mut grid[idx / plot_w][idx % plot_w];
        let mut style = Style::default().fg(Color::Yellow);
        if cell.0 == '█' {
            if let Some(colour) = cell.1.fg {
                style = style.bg(colour);
            }
        }
        *cell = (char::from_u32(0x2800 + mask as u32).unwrap_or(' '), style);
    }
}

/// Every dot on the straight line from one point to the other (Bresenham).
fn line((x0, y0): (i64, i64), (x1, y1): (i64, i64), plot: &mut impl FnMut(i64, i64)) {
    let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
    let (sx, sy) = ((x1 - x0).signum(), (y1 - y0).signum());
    let (mut x, mut y, mut err) = (x0, y0, dx + dy);
    loop {
        plot(x, y);
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

/// The base line, with a marker under the middle of the cursor's bar.
fn axis_row(l: &TimingLayout, plot_w: usize, cursor: usize, label_w: usize) -> Line<'static> {
    let mark = l.col(cursor, plot_w) + l.bar_w / 2;
    Line::from(vec![
        Span::styled(
            format!("{:width$}└{}", "", "─".repeat(mark), width = label_w + 1),
            Style::default().fg(dim()),
        ),
        Span::styled("▲", Style::default().fg(accent()).add_modifier(Modifier::BOLD)),
        Span::styled("─".repeat(plot_w.saturating_sub(mark + 1)), Style::default().fg(dim())),
    ])
}

/// What the cursor is on: the run, its branch, what triggered it, and when.
fn detail_row(tv: &TimingView, plot_w: usize) -> Line<'static> {
    let Some(p) = tv.selected() else {
        return Line::default();
    };
    let mut text = format!(" #{} · {}", p.number, fmt_dur(p.secs));
    if !p.branch.is_empty() {
        text.push_str(&format!(" · {}", truncate(&p.branch, 24)));
    }
    text.push_str(&format!(" · {} · {} ago", event_label(&p.event), fmt_age(p.finished)));
    // A folded column stands for several runs; say which one this is.
    let l = tv.layout(plot_w);
    let runs = l.bars.get(tv.bar_at(&l)).map_or(1, |b| b.runs);
    if runs > 1 {
        text.push_str(&format!(" · longest of {runs}"));
    }
    Line::from(Span::styled(text, Style::default().add_modifier(Modifier::BOLD)))
}

/// The numbers under the chart: how much history it covers, the spread, and
/// which way the recent runs are heading.
fn stats_row(tv: &TimingView, stats: &TimingStats) -> Line<'static> {
    let n = tv.points.len();
    let over = match tv.points.first() {
        Some(p) => format!(" over {}", fmt_age(p.finished)),
        None => String::new(),
    };
    let mut spans = vec![Span::styled(
        format!(
            " {n} run{}{over} · min {} · median {} · max {}",
            if n == 1 { "" } else { "s" },
            fmt_dur(stats.min),
            fmt_dur(stats.median),
            fmt_dur(stats.max),
        ),
        Style::default().fg(dim()),
    )];
    // Successes left off because GitHub no longer says how long they took.
    if tv.unknown > 0 {
        spans.push(Span::styled(
            format!(" · {} with no known duration", tv.unknown),
            Style::default().fg(dim()),
        ));
    }
    if let Some(t) = stats.trend {
        let (glyph, color) = if t > FLAT {
            ("▲", Color::Red)
        } else if t < -FLAT {
            ("▼", Color::Green)
        } else {
            ("▬", dim())
        };
        spans.push(Span::styled(" · trend ", Style::default().fg(dim())));
        spans.push(Span::styled(
            format!("{glyph} {t:+}%"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

fn note(f: &mut Frame, area: Rect, text: String) {
    f.render_widget(
        Paragraph::new(text).style(Style::default().fg(dim())).wrap(Wrap { trim: true }),
        area,
    );
}

/// Collects a row of the plot, merging neighbouring cells that share a style so
/// a hundred columns don't become a hundred spans.
struct Cells {
    spans: Vec<Span<'static>>,
    run: String,
    style: Style,
}

impl Cells {
    fn new() -> Self {
        Self { spans: Vec::new(), run: String::new(), style: Style::default() }
    }

    fn push(&mut self, ch: char, style: Style) {
        if !self.run.is_empty() && style != self.style {
            self.flush();
        }
        self.style = style;
        self.run.push(ch);
    }

    fn flush(&mut self) {
        if !self.run.is_empty() {
            self.spans.push(Span::styled(std::mem::take(&mut self.run), self.style));
        }
    }

    fn finish(mut self) -> Vec<Span<'static>> {
        self.flush();
        self.spans
    }
}
