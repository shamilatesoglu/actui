//! The open PR's Overview tab: facts, reviewers, description and checks.

use super::*;
use crate::app::PrView;
use crate::pulls::model::{
    blocked_reason, Check, CheckState, PrDetail, PrRef, QueueState, Verdict,
};
use ratatui::buffer::Buffer;
use ratatui::widgets::{ListState, Widget};

/// Draws the overview in `area`: the text scrolls above, the checks list sits
/// below it with its own cursor.
pub(super) fn draw_overview(
    f: &mut Frame,
    view: &mut PrView,
    detail: &PrDetail,
    area: Rect,
    spin: &'static str,
) {
    // The checks take what they need, up to half the height.
    let check_rows = (detail.checks.len().max(1) as u16 + 1).min(area.height / 2);
    let [text_area, checks_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(check_rows)]).areas(area);

    let lines = overview_lines(&view.pr, detail);
    let rows = wrapped_rows(&lines, text_area.width);
    view.overview_max_scroll = rows.saturating_sub(text_area.height as usize).min(u16::MAX as usize) as u16;
    view.overview_scroll = view.overview_scroll.min(view.overview_max_scroll);
    let text = Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((view.overview_scroll, 0));
    f.render_widget(text, text_area);

    draw_checks(f, &detail.checks, &mut view.checks, checks_area, spin);
}

fn draw_checks(f: &mut Frame, checks: &[Check], state: &mut ListState, area: Rect, spin: &'static str) {
    let [header, list_area] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let count = if checks.is_empty() { "none".to_string() } else { checks.len().to_string() };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Checks", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {count}"), Style::default().fg(dim())),
        ])),
        header,
    );
    let items: Vec<ListItem> = checks.iter().map(|c| ListItem::new(check_line(c, spin))).collect();
    let list = List::new(items).highlight_style(select_style(true)).highlight_symbol("▌");
    f.render_stateful_widget(list, list_area, state);
}

fn check_line(check: &Check, spin: &'static str) -> Line<'static> {
    let (glyph, color) = match check.state {
        CheckState::Passed => ("✓", Color::Green),
        CheckState::Failed => ("✗", Color::Red),
        CheckState::Pending => (spin, Color::Yellow),
        CheckState::Skipped => ("–", dim()),
        CheckState::Neutral => ("·", dim()),
    };
    Line::from(vec![Span::styled(glyph, Style::default().fg(color)), Span::raw(format!(" {}", check.name))])
}

/// How many rows `lines` take once wrapped to `width` the way the paragraph
/// wraps them. Counted by drawing into a scratch buffer, since the word
/// breaking is the paragraph's own.
fn wrapped_rows(lines: &[Line], width: u16) -> usize {
    if width == 0 {
        return 0;
    }
    // Even breaking every word mid-way takes under two rows per width's worth.
    let most: usize = lines.iter().map(|l| 2 * l.width().div_ceil(width as usize) + 1).sum();
    let area = Rect::new(0, 0, width, most.min(u16::MAX as usize) as u16);
    let mut buf = Buffer::empty(area);
    Paragraph::new(lines.to_vec()).wrap(Wrap { trim: false }).render(area, &mut buf);
    (0..area.height)
        .rev()
        .find(|&y| (0..width).any(|x| buf[(x, y)].symbol() != " "))
        .map_or(0, |y| y as usize + 1)
}

fn verdict_glyph(verdict: Verdict) -> (&'static str, &'static str, Color) {
    match verdict {
        Verdict::Approved => ("✓", "approved", Color::Green),
        Verdict::ChangesRequested => ("✗", "changes requested", Color::Red),
        Verdict::Requested => ("○", "requested", Color::Yellow),
        Verdict::Commented => ("●", "commented", dim()),
        Verdict::Dismissed => ("⊘", "dismissed", dim()),
    }
}

/// The text above the checks list.
fn overview_lines(pr: &PrRef, d: &PrDetail) -> Vec<Line<'static>> {
    let mut lines = vec![kv("author", &d.author)];
    // A branch in another repo is a fork's.
    let head = if d.head.repo == pr.repo {
        d.head.branch.clone()
    } else {
        format!("{}:{}", d.head.repo, d.head.branch)
    };
    lines.push(kv("branch", &format!("{head} → {}", d.base_branch)));
    if !d.labels.is_empty() {
        lines.push(kv("labels", &d.labels.join(", ")));
    }
    let mut merge = d.merge_status.words().to_string();
    if let Some(reason) = blocked_reason(d) {
        merge = format!("{merge}: {reason}");
    }
    if let QueueState::Queued { position } = d.queue {
        merge = format!("{merge}, queued #{position}");
    }
    lines.push(kv("merge", &merge));

    for (i, r) in d.reviewers.iter().enumerate() {
        let (glyph, words, color) = verdict_glyph(r.verdict);
        let label = if i == 0 { "review" } else { "" };
        lines.push(Line::from(vec![
            Span::styled(format!("{label:>LABEL_W$}  "), Style::default().fg(dim())),
            Span::styled(glyph, Style::default().fg(color)),
            Span::raw(format!(" {}  ", r.login)),
            Span::styled(words, Style::default().fg(dim())),
        ]));
    }

    lines.push(Line::default());
    let body = markdown_lines(&d.body);
    if d.body.trim().is_empty() {
        lines.push(Line::styled("No description.", Style::default().fg(dim())));
    } else {
        lines.extend(body);
    }
    lines
}

/// Plain-text rendering of GitHub markdown: just enough to read a PR
/// description. Wrapping is left to the caller's `Paragraph`.
pub(super) fn markdown_lines(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_code = false;
    for raw in without_comments(text) {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code = !in_code;
        } else if in_code {
            lines.push(Line::styled(raw, Style::default().fg(pale())));
        } else if let Some(heading) = heading(trimmed) {
            lines.push(Line::styled(
                links_as_text(heading),
                Style::default().fg(accent()).add_modifier(Modifier::BOLD),
            ));
        } else if let Some(item) = bullet(&raw) {
            lines.push(Line::from(links_as_text(&item)));
        } else {
            lines.push(Line::from(links_as_text(&raw)));
        }
    }
    lines
}

/// The lines of `text` without `<!-- … -->` comments (PR templates are full
/// of them) and without `\r`. A line that held only a comment goes away.
fn without_comments(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_comment = false;
    for raw in text.replace('\r', "").lines() {
        let mut kept = String::new();
        let mut rest = raw;
        let mut touched = in_comment;
        loop {
            if in_comment {
                match rest.find("-->") {
                    Some(i) => {
                        rest = &rest[i + 3..];
                        in_comment = false;
                    }
                    None => break,
                }
            } else {
                match rest.find("<!--") {
                    Some(i) => {
                        kept.push_str(&rest[..i]);
                        rest = &rest[i + 4..];
                        in_comment = true;
                        touched = true;
                    }
                    None => {
                        kept.push_str(rest);
                        break;
                    }
                }
            }
        }
        if !(touched && kept.trim().is_empty()) {
            out.push(kept);
        }
    }
    out
}

/// The text of a `#` heading line.
fn heading(line: &str) -> Option<&str> {
    let text = line.trim_start_matches('#');
    let hashes = line.len() - text.len();
    ((1..=6).contains(&hashes)).then(|| text.strip_prefix(' ')).flatten().map(str::trim)
}

/// A `- ` or `* ` list line with its bullet as `• `.
fn bullet(line: &str) -> Option<String> {
    let text = line.trim_start();
    let indent = &line[..line.len() - text.len()];
    let item = text.strip_prefix("- ").or_else(|| text.strip_prefix("* "))?;
    Some(format!("{indent}• {item}"))
}

/// `[text](url)` as `text (url)`, which reads fine without a mouse.
fn links_as_text(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let link = after.find("](").and_then(|mid| {
            let (url, tail) = after[mid + 2..].split_once(')')?;
            Some((&after[..mid], url, tail))
        });
        match link {
            Some((label, url, tail)) => {
                // `![alt](url)` is an image; its `!` goes too.
                out.push_str(rest[..open].strip_suffix('!').unwrap_or(&rest[..open]));
                out.push_str(&format!("{label} ({url})"));
                rest = tail;
            }
            None => {
                out.push_str(&rest[..=open]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pulls::model::{HeadRef, MergeStatus};

    fn detail() -> PrDetail {
        PrDetail {
            node_id: "PR_7".into(),
            title: "Fix login".into(),
            body: String::new(),
            author: "ana".into(),
            url: "https://github.com/org/api/pull/7".into(),
            head: HeadRef { branch: "main".into(), repo: "org/api".into(), oid: "abc".into() },
            base_branch: "release".into(),
            draft: false,
            labels: vec![],
            merge_status: MergeStatus::Clean,
            reviewers: vec![],
            checks: vec![],
            comments: vec![],
            reviews: vec![],
            threads: vec![],
            merge_methods: vec![],
            queue: QueueState::NoQueue,
        }
    }

    #[test]
    fn markdown_hides_template_comments_and_carriage_returns() {
        let text = "## Summary\r\n<!-- describe\r\nthe change -->\r\nFixes [#3](https://x/3)\r\n```\r\nlet a = 1;\r\n```";
        let lines = markdown_lines(text);
        let texts: Vec<String> = lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        assert_eq!(texts, ["Summary", "Fixes #3 (https://x/3)", "let a = 1;"]);
        assert!(lines.iter().flat_map(|l| &l.spans).all(|s| !s.content.contains('\r')));
        assert!(lines[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn a_long_description_scrolls_to_its_last_word() {
        let mut detail = detail();
        detail.body = (1..=60).map(|n| format!("word{n}")).collect::<Vec<_>>().join(" ") + " THEEND";
        let mut view = PrView {
            pr: PrRef { repo: "org/api".into(), number: 7 },
            detail: crate::app::Loadable::Loading,
            tab: crate::app::PrTab::Overview,
            overview_scroll: 0,
            overview_max_scroll: 0,
            checks: ListState::default(),
            files: Default::default(),
        };
        let draw = |view: &mut PrView| {
            let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(23, 12)).unwrap();
            term.draw(|f| draw_overview(f, view, &detail, f.area(), "*")).unwrap();
            format!("{:?}", term.backend().buffer())
        };
        draw(&mut view);
        assert!(view.overview_max_scroll > 0);
        view.overview_scroll = view.overview_max_scroll;
        assert!(draw(&mut view).contains("THEEND"));
    }

    #[test]
    fn a_blank_description_reads_as_none() {
        let mut detail = detail();
        detail.body = "  \n\r\n ".into();
        let lines = overview_lines(&PrRef { repo: "org/api".into(), number: 7 }, &detail);
        let last: String = lines.last().unwrap().spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(last, "No description.");
    }

    #[test]
    fn bullets_and_inline_comments() {
        let lines = markdown_lines("- one <!-- hidden --> two\n  * nested\n![logo](https://x/l.png)\nplain [ok");
        let texts: Vec<String> = lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        assert_eq!(texts, ["• one  two", "  • nested", "logo (https://x/l.png)", "plain [ok"]);
    }
}
