//! The open PR's Conversation tab: the description, comments, reviews and
//! review threads in the order they happened.

use super::overview::{markdown_lines, verdict_glyph};
use super::*;
use crate::app::Scroll;
use crate::pulls::model::{Comment, PrDetail, Review, ReviewThread, Verdict};
use chrono::{DateTime, Utc};

pub(super) enum ConvEntry<'a> {
    Description,
    Comment(&'a Comment),
    Review(&'a Review),
    /// An index into the detail's threads.
    Thread(usize),
}

/// The description, then everything else by time. A thread sits at its first
/// comment. A review with no text and no verdict beyond "commented" is left
/// out: its comments show in their threads.
pub(super) fn conversation(d: &PrDetail) -> Vec<(DateTime<Utc>, ConvEntry<'_>)> {
    let mut entries: Vec<(DateTime<Utc>, ConvEntry)> = Vec::new();
    entries.extend(d.comments.iter().map(|c| (c.created_at, ConvEntry::Comment(c))));
    entries.extend(
        d.reviews
            .iter()
            .filter(|r| !(r.body.trim().is_empty() && r.verdict == Verdict::Commented))
            .map(|r| (r.submitted_at, ConvEntry::Review(r))),
    );
    entries.extend(
        d.threads
            .iter()
            .enumerate()
            .filter_map(|(i, t)| Some((t.comments.first()?.created_at, ConvEntry::Thread(i)))),
    );
    entries.sort_by_key(|(at, _)| *at);
    // The description has no time of its own; it opens the conversation.
    entries.insert(0, (DateTime::<Utc>::MIN_UTC, ConvEntry::Description));
    entries
}

pub(super) fn draw_conversation(f: &mut Frame, scroll: &mut Scroll, d: &PrDetail, area: Rect) {
    draw_scrolling_text(f, scroll, || conversation_lines(d), area);
}

fn conversation_lines(d: &PrDetail) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (_, entry) in conversation(d) {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        match entry {
            ConvEntry::Description => {
                lines.push(header(&d.author, "description", dim(), None));
                if d.body.trim().is_empty() {
                    lines.push(Line::styled("No description.", Style::default().fg(dim())));
                } else {
                    lines.extend(markdown_lines(&d.body));
                }
            }
            ConvEntry::Comment(c) => {
                lines.push(header(&c.author, "commented", dim(), Some(c.created_at)));
                lines.extend(markdown_lines(&c.body));
            }
            ConvEntry::Review(r) => {
                let (_, words, color) = verdict_glyph(r.verdict);
                lines.push(header(&r.author, words, color, Some(r.submitted_at)));
                lines.extend(markdown_lines(&r.body));
            }
            ConvEntry::Thread(i) => thread_lines(&d.threads[i], &mut lines),
        }
    }
    lines
}

fn thread_lines(t: &ReviewThread, lines: &mut Vec<Line<'static>>) {
    let place = match t.line {
        Some(n) => format!("{}:{n}", t.path),
        None => t.path.clone(),
    };
    let mut spans = vec![Span::styled(place, Style::default().fg(accent()).add_modifier(Modifier::BOLD))];
    for (on, tag) in [(t.outdated, "outdated"), (t.resolved, "resolved")] {
        if on {
            spans.push(Span::styled(format!("  {tag}"), Style::default().fg(dim())));
        }
    }
    lines.push(Line::from(spans));
    for (n, c) in t.comments.iter().enumerate() {
        if n > 0 {
            lines.push(Line::default());
        }
        let mut head = header(&c.author, "commented", dim(), Some(c.created_at));
        head.spans.insert(0, Span::raw("  "));
        lines.push(head);
        for mut line in markdown_lines(&c.body) {
            line.spans.insert(0, Span::raw("  "));
            lines.push(line);
        }
    }
}

/// `{author}  {what}  {age}`.
fn header(author: &str, what: &str, color: Color, at: Option<DateTime<Utc>>) -> Line<'static> {
    let mut spans = vec![
        Span::styled(author.to_string(), Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {what}"), Style::default().fg(color)),
    ];
    if let Some(at) = at {
        spans.push(Span::styled(format!("  {}", fmt_age(at)), Style::default().fg(dim())));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pulls::model::{HeadRef, MergeStatus, QueueState, Side};

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

    fn at(minutes: i64) -> DateTime<Utc> {
        Utc::now() - chrono::Duration::minutes(minutes)
    }

    fn comment(body: &str, when: DateTime<Utc>) -> Comment {
        Comment { id: 1, author: "bob".into(), body: body.into(), created_at: when }
    }

    #[test]
    fn conversation_is_in_time_order_with_threads_at_their_first_comment() {
        let mut d = detail();
        d.comments = vec![comment("second", at(20))];
        d.reviews = vec![Review {
            author: "cy".into(),
            verdict: Verdict::Approved,
            body: String::new(),
            submitted_at: at(30),
        }];
        d.threads = vec![ReviewThread {
            id: "T".into(),
            resolved: false,
            outdated: false,
            path: "a.rs".into(),
            line: Some(1),
            start_line: None,
            side: Side::Right,
            comments: vec![comment("third", at(10))],
        }];
        let kinds: Vec<&str> = conversation(&d)
            .iter()
            .map(|(_, e)| match e {
                ConvEntry::Description => "description",
                ConvEntry::Review(_) => "review",
                ConvEntry::Comment(_) => "comment",
                ConvEntry::Thread(0) => "thread",
                ConvEntry::Thread(_) => "other thread",
            })
            .collect();
        assert_eq!(kinds, ["description", "review", "comment", "thread"]);
    }

    #[test]
    fn an_empty_commented_review_is_left_out() {
        let mut d = detail();
        d.reviews = vec![Review {
            author: "cy".into(),
            verdict: Verdict::Commented,
            body: " ".into(),
            submitted_at: at(5),
        }];
        assert_eq!(conversation(&d).len(), 1);
    }
}
