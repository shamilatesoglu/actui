//! The open PR's Files tab: the changed files on the left, one file's diff on
//! the right. Borrows the palette accessors and `pane` from the `ui` module.

use super::*;
use crate::app::{thread_rows, DiffLine, DiffView, FilesState, PrView};
use crate::pulls::model::{FileStatus, PrFile, ReviewThread};
use ratatui::widgets::ListState;

/// Widest the file list grows to.
const LIST_WIDTH: u16 = 40;
/// Columns the review-thread marker takes in the gutter.
const MARKER_WIDTH: usize = 4;
/// What a tab character is drawn as.
const TAB: &str = "    ";

pub(super) fn draw_files(f: &mut Frame, view: &mut PrView, threads: &[ReviewThread], area: Rect, spin: &'static str) {
    let FilesState { files, sel, diff } = &mut view.files;
    let files = match files {
        Loadable::Loading => {
            let line = Line::styled(format!("{spin} loading…"), Style::default().fg(dim()));
            return f.render_widget(Paragraph::new(line), area);
        }
        Loadable::Failed(e) => {
            let line = Line::styled(e.clone(), Style::default().fg(Color::Red));
            return f.render_widget(Paragraph::new(line).wrap(Wrap { trim: false }), area);
        }
        Loadable::Ready(files) if files.is_empty() => {
            let line = Line::styled("No changed files", Style::default().fg(dim()));
            return f.render_widget(Paragraph::new(line), area);
        }
        Loadable::Ready(files) => files,
    };
    let list_width = (area.width / 3).min(LIST_WIDTH);
    let [left, _, right] =
        Layout::horizontal([Constraint::Length(list_width), Constraint::Length(2), Constraint::Min(0)]).areas(area);
    draw_file_list(f, files, sel, diff.is_none(), left);

    let shown = diff.as_ref().map(|d| d.file).or(sel.selected());
    let Some(file) = shown.and_then(|at| files.get(at)) else { return };
    match (&file.patch, diff) {
        (None, _) => {
            let text = "GitHub doesn't send this diff (binary or too large). o opens it on github.com.";
            f.render_widget(Paragraph::new(text).style(Style::default().fg(dim())).wrap(Wrap { trim: false }), right);
        }
        (Some(_), None) => {
            let line = Line::styled(format!("{}  Enter opens the diff", file.path), Style::default().fg(dim()));
            f.render_widget(Paragraph::new(line), right);
        }
        (Some(_), Some(diff)) => draw_diff(f, diff, file, threads, right),
    }
}

fn draw_file_list(f: &mut Frame, files: &[PrFile], sel: &mut ListState, focused: bool, area: Rect) {
    // The highlight marker takes the first column.
    let width = area.width.saturating_sub(1) as usize;
    let items: Vec<ListItem> = files.iter().map(|file| ListItem::new(file_line(file, width))).collect();
    let list = List::new(items).highlight_style(select_style(focused)).highlight_symbol("▌");
    f.render_stateful_widget(list, area, sel);
}

fn status_color(status: FileStatus) -> Color {
    match status {
        FileStatus::Added => Color::Green,
        FileStatus::Removed => Color::Red,
        FileStatus::Modified | FileStatus::Changed => Color::Yellow,
        FileStatus::Renamed | FileStatus::Copied => Color::Cyan,
        FileStatus::Unchanged => dim(),
    }
}

/// `{status letter} {path}  +a −d`, the path cut from the left to make room.
fn file_line(file: &PrFile, width: usize) -> Line<'static> {
    let stats = format!("+{} \u{2212}{}", file.additions, file.deletions);
    let stats_width = stats.chars().count();
    // On a narrow list the numbers give way before the path does.
    let show_stats = width >= 2 + 2 + stats_width + 8;
    let path_width = width.saturating_sub(2 + if show_stats { 2 + stats_width } else { 0 });
    let path = clip_left(&file.path, path_width);
    let pad = " ".repeat(path_width.saturating_sub(path.chars().count()));
    let mut spans = vec![
        Span::styled(file.status.letter().to_string(), Style::default().fg(status_color(file.status))),
        Span::raw(format!(" {path}{pad}")),
    ];
    if show_stats {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(format!("+{}", file.additions), Style::default().fg(Color::Green)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(format!("\u{2212}{}", file.deletions), Style::default().fg(Color::Red)));
    }
    Line::from(spans)
}

/// The end of `s` that fits in `max` characters, marked with a leading `…`.
fn clip_left(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    let tail: String = s.chars().skip(count - max.saturating_sub(1)).collect();
    format!("…{tail}")
}

/// What the header over the diff says about the search.
fn search_text(diff: &DiffView) -> Option<String> {
    if diff.search.is_empty() && !diff.searching {
        return None;
    }
    let cursor = if diff.searching { "▏" } else { "" };
    let found = match diff.matches.len() {
        0 if diff.search.is_empty() => String::new(),
        0 => "  no matches".to_string(),
        1 => "  1 match".to_string(),
        n => format!("  {n} matches"),
    };
    Some(format!("  /{}{cursor}{found}", diff.search))
}

fn draw_diff(f: &mut Frame, diff: &mut DiffView, file: &PrFile, threads: &[ReviewThread], area: Rect) {
    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let mut title = vec![Span::styled(file.path.clone(), Style::default().add_modifier(Modifier::BOLD))];
    if let Some(search) = search_text(diff) {
        title.push(Span::styled(search, Style::default().fg(accent())));
    }
    f.render_widget(Paragraph::new(Line::from(title)), head);

    // Only the rows on screen are built, so a huge diff costs no more to draw
    // than a small one.
    let rows = body.height as usize;
    diff.height = rows;
    if diff.cursor < diff.scroll {
        diff.scroll = diff.cursor;
    } else if diff.cursor >= diff.scroll + rows {
        diff.scroll = diff.cursor + 1 - rows;
    }
    diff.scroll = diff.scroll.min(diff.lines.len().saturating_sub(rows));

    let on_row = thread_rows(&diff.lines, threads, &file.path);
    let marked = diff.mark.map(|m| m.min(diff.cursor)..=m.max(diff.cursor));
    let end = (diff.scroll + rows).min(diff.lines.len());
    let lines: Vec<Line> = (diff.scroll..end)
        .map(|i| {
            let threads_here = on_row.get(&i).map_or(0, Vec::len);
            let style = if i == diff.cursor {
                select_style(true)
            } else if marked.as_ref().is_some_and(|r| r.contains(&i)) {
                Style::default().bg(bg_sel_dim())
            } else {
                Style::default()
            };
            let found = diff.matches.binary_search(&i).is_ok();
            diff_row(diff, i, threads_here, body.width as usize, found).style(style)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), body);
}

/// `{old} {new} {💬n}{+|-| }{text}`, the text cut to the pane.
fn diff_row(diff: &DiffView, at: usize, thread_count: usize, width: usize, found: bool) -> Line<'static> {
    let line = &diff.lines[at];
    let digits = diff.number_width;
    let number = |n: Option<u32>| match n {
        Some(n) => format!("{n:>digits$}"),
        None => " ".repeat(digits),
    };
    let (old, new) = match line {
        DiffLine::Context { old, new, .. } => (Some(*old), Some(*new)),
        DiffLine::Added { new, .. } => (None, Some(*new)),
        DiffLine::Removed { old, .. } => (Some(*old), None),
        DiffLine::Hunk(_) | DiffLine::NoNewline => (None, None),
    };
    let marker = if thread_count > 0 { format!("\u{1F4AC}{thread_count}") } else { String::new() };
    let marker_pad = " ".repeat(MARKER_WIDTH.saturating_sub(Span::raw(marker.clone()).width()));

    let (sign, color) = match line {
        DiffLine::Added { .. } => ("+", Some(Color::Green)),
        DiffLine::Removed { .. } => ("-", Some(Color::Red)),
        DiffLine::Context { .. } => (" ", None),
        DiffLine::Hunk(_) | DiffLine::NoNewline => ("", Some(dim())),
    };
    let gutter_width = digits * 2 + 2 + MARKER_WIDTH + sign.len();
    let text = clip(line.text(), width.saturating_sub(gutter_width));
    let mut text_style = color.map_or(Style::default(), |c| Style::default().fg(c));
    if found {
        text_style = text_style.add_modifier(Modifier::UNDERLINED);
    }
    Line::from(vec![
        Span::styled(format!("{} {} ", number(old), number(new)), Style::default().fg(dim())),
        Span::styled(marker, Style::default().fg(accent())),
        Span::raw(marker_pad),
        Span::styled(sign, text_style),
        Span::styled(text, text_style),
    ])
}

/// The start of `text` that fits in `width` columns, tabs drawn as spaces.
fn clip(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let (shown, columns) = match c {
            '\t' => (None, TAB.len()),
            c if c.is_control() => (Some('\u{FFFD}'), 1),
            c if c.is_ascii() => (Some(c), 1),
            c => (Some(c), Span::raw(c.to_string()).width()),
        };
        if used + columns > width {
            break;
        }
        used += columns;
        match shown {
            Some(c) => out.push(c),
            None => out.push_str(TAB),
        }
    }
    out
}
