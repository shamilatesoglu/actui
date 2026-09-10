//! The duration chart: one workflow's successful runs as bars, oldest on the
//! left and the newest against the right edge, with the median drawn across
//! them. Borrows the palette accessors, `pane`, and the duration formatting
//! from the parent `ui` module.

use super::*;
use crate::app::{App, TimingLayout, TimingStats, TimingView, TIMING_HISTORY};

/// Eighth-blocks, so a bar can stop part-way up a row.
const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Caps a bar that runs off the top of the axis.
const OVER: char = '↑';

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
        note(
            f,
            inner,
            format!(
                "No successful runs of {} in its last {TIMING_HISTORY}.\n\n\
                 [ and ] move to another workflow in this repo.",
                tv.workflow
            ),
        );
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
        lines.extend(chart_rows(&l, &stats, &ticks, chart_h, plot_w, at));
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

/// The plot itself, top row first. The median is laid down first so the bars
/// draw over it: it shows through wherever a run came in under it.
fn chart_rows(
    l: &TimingLayout,
    stats: &TimingStats,
    ticks: &Ticks,
    chart_h: usize,
    plot_w: usize,
    cursor: usize,
) -> Vec<Line<'static>> {
    let full = (chart_h * 8) as i64;
    let span = (stats.ceiling - stats.floor).max(1);
    // Eighths of a row each bar reaches. The axis starts at the floor rather
    // than at zero, so the height goes on what actually varies; a run at or
    // under the floor still gets a sliver, and one past the ceiling fills its
    // column and is marked at the top.
    let height = |secs: i64| {
        if secs <= 0 {
            0
        } else {
            ((secs - stats.floor) * full / span).clamp(1, full)
        }
    };
    let med_row = chart_h - 1 - ((height(stats.median) / 8) as usize).min(chart_h - 1);

    let offset = l.offset(plot_w);
    let mut rows = Vec::with_capacity(chart_h);
    for r in 0..chart_h {
        // Eighths already covered by the rows below this one.
        let below = ((chart_h - 1 - r) * 8) as i64;
        let mut cells = Cells::new();
        for col in 0..plot_w {
            let bar = (col >= offset && (col - offset).is_multiple_of(l.step))
                .then(|| (col - offset) / l.step)
                .and_then(|i| l.bars.get(i).map(|b| (i, b)));
            let filled = bar.map_or(0, |(_, b)| (height(b.secs) - below).clamp(0, 8));
            if filled > 0 {
                let style = match bar {
                    // The cursor's bar drops the accent for the terminal's own
                    // foreground, so it reads as picked out rather than as one
                    // more blue bar.
                    Some((i, _)) if i == cursor => Style::default().add_modifier(Modifier::BOLD),
                    _ => Style::default().fg(accent()),
                };
                let over = r == 0 && bar.is_some_and(|(_, b)| b.secs > stats.ceiling);
                cells.push(if over { OVER } else { BLOCKS[filled as usize - 1] }, style);
            } else if r == med_row {
                cells.push('┄', Style::default().fg(dim()));
            } else {
                cells.push(' ', Style::default());
            }
        }
        // Top, then the median, then the floor — where two land on one row the
        // median is the one worth naming.
        let (label, tick) = if r == 0 {
            (ticks.top.as_str(), '┤')
        } else if r == med_row {
            (ticks.mid.as_str(), '┤')
        } else if r == chart_h - 1 {
            (ticks.bottom.as_str(), '┤')
        } else {
            ("", '│')
        };
        let mut spans = vec![Span::styled(
            format!(" {label:>width$}{tick}", width = ticks.width),
            Style::default().fg(dim()),
        )];
        spans.extend(cells.finish());
        rows.push(Line::from(spans));
    }
    rows
}

/// The base line, with a marker under the run the cursor is on.
fn axis_row(l: &TimingLayout, plot_w: usize, cursor: usize, label_w: usize) -> Line<'static> {
    let mark = l.col(cursor, plot_w);
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
