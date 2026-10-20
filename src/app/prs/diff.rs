//! A file's unified diff as rows, with the cursor, search and review-thread
//! lookups the diff viewer needs.

use super::Loadable;
use crate::pulls::model::{DraftComment, LineSpan, PrFile, ReviewThread, Side};
use ratatui::widgets::ListState;
use std::collections::{HashMap, HashSet};

/// One row of a diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffLine {
    /// An `@@ -a,b +c,d @@` header, whole.
    Hunk(String),
    Context { old: u32, new: u32, text: String },
    Added { new: u32, text: String },
    Removed { old: u32, text: String },
    /// `\ No newline at end of file`.
    NoNewline,
}

impl DiffLine {
    /// The side and line number a review comment on this row attaches to.
    pub fn anchor(&self) -> Option<(Side, u32)> {
        match self {
            DiffLine::Removed { old, .. } => Some((Side::Left, *old)),
            DiffLine::Added { new, .. } | DiffLine::Context { new, .. } => Some((Side::Right, *new)),
            DiffLine::Hunk(_) | DiffLine::NoNewline => None,
        }
    }

    /// The row's text without its `+`, `-` or space marker.
    pub fn text(&self) -> &str {
        match self {
            DiffLine::Hunk(text)
            | DiffLine::Context { text, .. }
            | DiffLine::Added { text, .. }
            | DiffLine::Removed { text, .. } => text,
            DiffLine::NoNewline => NO_NEWLINE,
        }
    }
}

const NO_NEWLINE: &str = "\\ No newline at end of file";

/// The starting old and new line numbers of an `@@ -a,b +c,d @@` header.
fn hunk_start(header: &str) -> Option<(u32, u32)> {
    let mut parts = header.strip_prefix("@@ ")?.split_whitespace();
    let first = |part: &str, sign: char| -> Option<u32> {
        part.strip_prefix(sign)?.split(',').next()?.parse().ok()
    };
    Some((first(parts.next()?, '-')?, first(parts.next()?, '+')?))
}

/// Reads a unified diff, numbering each row from its hunk header.
pub fn parse_patch(patch: &str) -> Vec<DiffLine> {
    let mut rows = Vec::new();
    let (mut old, mut new) = (0, 0);
    for line in patch.lines() {
        if line.starts_with("@@") {
            if let Some(start) = hunk_start(line) {
                (old, new) = start;
            }
            rows.push(DiffLine::Hunk(line.to_string()));
        } else if line.starts_with('\\') {
            rows.push(DiffLine::NoNewline);
        } else if let Some(text) = line.strip_prefix('+') {
            rows.push(DiffLine::Added { new, text: text.to_string() });
            new += 1;
        } else if let Some(text) = line.strip_prefix('-') {
            rows.push(DiffLine::Removed { old, text: text.to_string() });
            old += 1;
        } else {
            // A blank context line can arrive without its leading space.
            let text = line.strip_prefix(' ').unwrap_or(line);
            rows.push(DiffLine::Context { old, new, text: text.to_string() });
            old += 1;
            new += 1;
        }
    }
    rows
}

/// The row that shows `line` of `side`, if the diff has it.
fn row_of(lines: &[DiffLine], side: Side, line: u32) -> Option<usize> {
    lines.iter().position(|row| match (side, row) {
        (Side::Left, DiffLine::Removed { old, .. } | DiffLine::Context { old, .. }) => *old == line,
        (Side::Right, DiffLine::Added { new, .. } | DiffLine::Context { new, .. }) => *new == line,
        _ => false,
    })
}

/// Which diff rows carry review threads: row index to thread indices. A
/// thread whose lines are gone has no row.
pub fn thread_rows(lines: &[DiffLine], threads: &[ReviewThread], path: &str) -> HashMap<usize, Vec<usize>> {
    let mut rows: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, thread) in threads.iter().enumerate() {
        let Some(line) = thread.line.filter(|_| thread.path == path) else { continue };
        if let Some(at) = row_of(lines, thread.side, line) {
            rows.entry(at).or_default().push(i);
        }
    }
    rows
}

/// The side and lines a comment on rows `a` to `b` (either order) covers.
/// A range of only context lines is on the new side. Removed lines put it on
/// the old side, where its context lines count by their old numbers. Added
/// and removed lines can't share a range, and a range can't leave its hunk.
pub fn range_span(lines: &[DiffLine], a: usize, b: usize) -> Result<(Side, LineSpan), &'static str> {
    const NOT_A_LINE: &str = "Only code lines can be commented on";
    let rows = lines.get(a.min(b)..=a.max(b)).ok_or(NOT_A_LINE)?;
    if rows.iter().any(|row| matches!(row, DiffLine::Hunk(_))) {
        return Err("A range has to stay inside one hunk");
    }
    let removed = rows.iter().any(|row| matches!(row, DiffLine::Removed { .. }));
    let added = rows.iter().any(|row| matches!(row, DiffLine::Added { .. }));
    let side = match (removed, added) {
        (true, true) => return Err("A range has to stay on one side of the diff"),
        (true, false) => Side::Left,
        (false, _) => Side::Right,
    };
    let numbers: Vec<u32> = rows
        .iter()
        .filter_map(|row| match (side, row) {
            (Side::Left, DiffLine::Removed { old, .. } | DiffLine::Context { old, .. }) => Some(*old),
            (Side::Right, DiffLine::Added { new, .. } | DiffLine::Context { new, .. }) => Some(*new),
            _ => None,
        })
        .collect();
    match (numbers.first(), numbers.last()) {
        (Some(&first), Some(&last)) => Ok((side, LineSpan::new(first, last))),
        _ => Err(NOT_A_LINE),
    }
}

/// Which diff rows carry draft comments: the last line of each one's span,
/// where GitHub will show it.
pub fn draft_rows(lines: &[DiffLine], drafts: &[DraftComment], path: &str) -> HashSet<usize> {
    drafts
        .iter()
        .filter(|d| d.path == path)
        .filter_map(|d| row_of(lines, d.side, d.span.end()))
        .collect()
}

/// An open diff.
pub struct DiffView {
    /// Index into the file list.
    pub file: usize,
    pub lines: Vec<DiffLine>,
    pub cursor: usize,
    pub scroll: usize,
    /// The row a range comment starts from, set with `v`; the cursor is its other end.
    pub mark: Option<usize>,
    pub search: String,
    /// The search box is taking keys.
    pub searching: bool,
    /// Rows that contain the search text, in order.
    pub matches: Vec<usize>,
    /// Digits in the widest line number, for the gutters.
    pub number_width: usize,
    /// Rows the diff showed in the last frame, which a page key moves by.
    pub height: usize,
    /// Diff row to the review threads on it, kept up to date by `FilesState::sync_threads`.
    pub thread_rows: HashMap<usize, Vec<usize>>,
}

impl DiffView {
    pub fn new(file: usize, patch: &str) -> Self {
        let lines = parse_patch(patch);
        let widest = lines
            .iter()
            .filter_map(|row| match row {
                DiffLine::Context { old, new, .. } => Some(*old.max(new)),
                DiffLine::Added { new, .. } => Some(*new),
                DiffLine::Removed { old, .. } => Some(*old),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        DiffView {
            file,
            lines,
            cursor: 0,
            scroll: 0,
            mark: None,
            search: String::new(),
            searching: false,
            matches: Vec::new(),
            number_width: widest.to_string().len(),
            height: 0,
            thread_rows: HashMap::new(),
        }
    }

    /// Moves the cursor by `delta` rows, stopping at the first and last.
    pub fn move_cursor(&mut self, delta: i32) {
        let last = self.lines.len().saturating_sub(1) as i64;
        self.cursor = (self.cursor as i64 + delta as i64).clamp(0, last) as usize;
    }

    /// Searches rows for `q` ignoring case, and moves to the first match at or
    /// below the cursor.
    pub fn set_search(&mut self, q: &str) {
        self.search = q.to_string();
        self.matches.clear();
        if q.is_empty() {
            return;
        }
        let q = q.to_lowercase();
        self.matches = (0..self.lines.len())
            .filter(|&i| self.lines[i].text().to_lowercase().contains(&q))
            .collect();
        if let Some(&first) = self.matches.iter().find(|&&i| i >= self.cursor).or(self.matches.first()) {
            self.cursor = first;
        }
    }

    /// Moves to the next match, or the previous one, wrapping around.
    pub fn next_match(&mut self, forward: bool) {
        let found = if forward {
            self.matches.iter().find(|&&i| i > self.cursor).or(self.matches.first())
        } else {
            self.matches.iter().rev().find(|&&i| i < self.cursor).or(self.matches.last())
        };
        if let Some(&at) = found {
            self.cursor = at;
        }
    }
}

/// The Files tab: the changed files and the diff opened from them.
#[derive(Default)]
pub struct FilesState {
    pub files: Loadable<Vec<PrFile>>,
    /// Selected index into the file list.
    pub sel: ListState,
    pub diff: Option<DiffView>,
    /// A read of the files has been sent, so a second one is not.
    asked: bool,
}

impl FilesState {
    /// Whether the files should be read now: they are neither here nor on
    /// their way. Counts as sending the read.
    pub fn needs_fetch(&mut self) -> bool {
        let wanted = match self.files {
            Loadable::Ready(_) => false,
            Loadable::Loading => !self.asked,
            Loadable::Failed(_) => true,
        };
        self.asked |= wanted;
        wanted
    }

    /// Looks up the open diff's review threads again. Called when the diff
    /// opens, its lines change, or the PR's threads are read.
    pub fn sync_threads(&mut self, threads: &[ReviewThread]) {
        let Some(diff) = &mut self.diff else { return };
        let Loadable::Ready(files) = &self.files else { return };
        if let Some(file) = files.get(diff.file) {
            diff.thread_rows = thread_rows(&diff.lines, threads, &file.path);
        }
    }

    fn list(&self) -> &[PrFile] {
        match &self.files {
            Loadable::Ready(files) => files,
            _ => &[],
        }
    }

    /// The file the diff pane is about: the open diff's, else the selected one.
    pub fn shown_file(&self) -> Option<&PrFile> {
        let at = self.diff.as_ref().map(|d| d.file).or(self.sel.selected())?;
        self.list().get(at)
    }

    pub fn move_sel(&mut self, delta: i32) {
        let count = self.list().len();
        if count > 0 {
            let at = self.sel.selected().unwrap_or(0) as i64;
            self.sel.select(Some((at + delta as i64).clamp(0, count as i64 - 1) as usize));
        }
    }

    /// Opens the selected file's diff. A file GitHub sent no diff for has
    /// nothing to open.
    pub fn open_selected(&mut self) {
        let Some(at) = self.sel.selected() else { return };
        if let Some(patch) = self.list().get(at).and_then(|f| f.patch.as_deref()) {
            self.diff = Some(DiffView::new(at, patch));
        }
    }

    /// Takes in a finished read. The selection and the open diff stay on
    /// their files when those are still in the list, and a failed refresh
    /// leaves what was shown.
    pub fn set(&mut self, result: Result<Vec<PrFile>, String>) {
        let files = match result {
            Ok(files) => files,
            Err(e) => {
                if !matches!(self.files, Loadable::Ready(_)) {
                    self.files = Loadable::Failed(e);
                }
                return;
            }
        };
        let old: &[PrFile] = match &self.files {
            Loadable::Ready(files) => files,
            _ => &[],
        };
        let path_at = |at: usize| old.get(at).map(|f| f.path.clone());
        let sel_path = self.sel.selected().and_then(path_at);
        let sel_index = self.sel.selected().unwrap_or(0);
        let open = self.diff.take().map(|d| (path_at(d.file), old.get(d.file).and_then(|f| f.patch.clone()), d));

        let find = |path: &Option<String>| path.as_ref().and_then(|p| files.iter().position(|f| &f.path == p));
        let sel = find(&sel_path).or(if files.is_empty() { None } else { Some(sel_index.min(files.len() - 1)) });
        self.diff = open.and_then(|(path, old_patch, mut diff)| {
            let at = find(&path)?;
            let patch = files[at].patch.as_deref()?;
            if old_patch.as_deref() == Some(patch) {
                diff.file = at;
                return Some(diff);
            }
            let mut fresh = DiffView::new(at, patch);
            fresh.searching = diff.searching;
            fresh.set_search(&diff.search);
            fresh.cursor = diff.cursor.min(fresh.lines.len().saturating_sub(1));
            fresh.scroll = diff.scroll.min(fresh.cursor);
            Some(fresh)
        });
        self.sel.select(sel);
        self.files = Loadable::Ready(files);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pulls::model::{Comment, ReviewThread};

    fn thread(path: &str, line: Option<u32>, side: Side) -> ReviewThread {
        ReviewThread {
            id: "T".into(),
            resolved: false,
            outdated: line.is_none(),
            path: path.into(),
            line,
            start_line: None,
            side,
            comments: Vec::<Comment>::new(),
        }
    }

    fn ctx(old: u32, new: u32, text: &str) -> DiffLine {
        DiffLine::Context { old, new, text: text.into() }
    }

    #[test]
    fn parse_patch_numbers_both_sides() {
        let patch = "@@ -10,3 +10,4 @@ fn a()\n ctx\n-old\n+new\n+added\n ctx2\n\\ No newline at end of file";
        assert_eq!(
            parse_patch(patch),
            [
                DiffLine::Hunk("@@ -10,3 +10,4 @@ fn a()".into()),
                ctx(10, 10, "ctx"),
                DiffLine::Removed { old: 11, text: "old".into() },
                DiffLine::Added { new: 11, text: "new".into() },
                DiffLine::Added { new: 12, text: "added".into() },
                ctx(12, 13, "ctx2"),
                DiffLine::NoNewline,
            ]
        );
    }

    #[test]
    fn a_second_hunk_restarts_the_numbers_and_a_bare_blank_line_is_context() {
        let rows = parse_patch("@@ -1 +1 @@\n-a\n+b\n@@ -20,2 +20,2 @@\n\n x");
        assert_eq!(rows[3], DiffLine::Hunk("@@ -20,2 +20,2 @@".into()));
        assert_eq!(rows[4], ctx(20, 20, ""));
        assert_eq!(rows[5], ctx(21, 21, "x"));
    }

    #[test]
    fn anchors_follow_the_side_a_comment_would_use() {
        assert_eq!(DiffLine::Removed { old: 4, text: String::new() }.anchor(), Some((Side::Left, 4)));
        assert_eq!(DiffLine::Added { new: 5, text: String::new() }.anchor(), Some((Side::Right, 5)));
        assert_eq!(ctx(3, 4, "").anchor(), Some((Side::Right, 4)));
        assert_eq!(DiffLine::Hunk(String::new()).anchor(), None);
        assert_eq!(DiffLine::NoNewline.anchor(), None);
    }

    #[test]
    fn outdated_threads_have_no_row() {
        let lines = parse_patch("@@ -10,3 +10,4 @@\n ctx\n-old\n+new\n+added\n ctx2");
        let threads = [
            thread("a.rs", None, Side::Right),
            thread("a.rs", Some(12), Side::Right),
            thread("b.rs", Some(12), Side::Right),
            thread("a.rs", Some(11), Side::Left),
        ];
        let rows = thread_rows(&lines, &threads, "a.rs");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[&4], [1]);
        assert_eq!(lines[4], DiffLine::Added { new: 12, text: "added".into() });
        assert_eq!(rows[&2], [3]);
    }

    #[test]
    fn an_old_side_range_takes_in_context_by_old_numbers() {
        // 0 hunk, 1 removed 1, 2 context 2/1, 3 removed 3.
        let lines = parse_patch("@@ -1,3 +1,1 @@\n-a\n b\n-c");
        assert_eq!(range_span(&lines, 1, 3), Ok((Side::Left, LineSpan::Range { start: 1, end: 3 })));
        assert_eq!(range_span(&lines, 2, 3), Ok((Side::Left, LineSpan::Range { start: 2, end: 3 })));
    }

    #[test]
    fn a_new_side_range_takes_in_context_by_new_numbers() {
        // 0 hunk, 1 added 1, 2 context 1/2, 3 added 3.
        let lines = parse_patch("@@ -1,1 +1,3 @@\n+a\n b\n+c");
        assert_eq!(range_span(&lines, 3, 1), Ok((Side::Right, LineSpan::Range { start: 1, end: 3 })));
        assert_eq!(range_span(&lines, 2, 2), Ok((Side::Right, LineSpan::One(2))), "context alone is the new side");
    }

    #[test]
    fn a_range_breaks_on_the_other_side_or_a_hunk_header() {
        let lines = parse_patch("@@ -1,2 +1,2 @@\n-a\n b\n+c\n@@ -9 +9 @@\n d");
        assert_eq!(range_span(&lines, 1, 3), Err("A range has to stay on one side of the diff"));
        assert_eq!(range_span(&lines, 2, 5), Err("A range has to stay inside one hunk"));
        assert_eq!(range_span(&lines, 0, 0), Err("A range has to stay inside one hunk"));
    }

    fn huge_patch() -> String {
        let mut patch = String::from("@@ -1,15000 +1,15000 @@\n");
        for i in 0..15_000 {
            patch.push_str(&format!("-old {i}\n+new {i}\n"));
        }
        patch
    }

    #[test]
    fn a_huge_diff_parses_and_jumps_quickly() {
        let patch = huge_patch();
        let mut view = DiffView::new(0, &patch);
        assert_eq!(view.lines.len(), 30_001);
        view.move_cursor(i32::MAX);
        assert_eq!(view.cursor, 30_000);
        assert_eq!(view.lines[view.cursor].text(), "new 14999");
        view.move_cursor(i32::MIN);
        assert_eq!(view.cursor, 0);
    }

    #[test]
    fn search_finds_rows_ignoring_case_and_steps_with_wraparound() {
        let mut view = DiffView::new(0, "@@ -1,3 +1,3 @@\n a Foo\n-b\n+foo bar\n c FOO");
        view.cursor = 2;
        view.set_search("foo");
        assert_eq!(view.matches, [1, 3, 4]);
        assert_eq!(view.cursor, 3, "the first match below the cursor");
        view.next_match(true);
        assert_eq!(view.cursor, 4);
        view.next_match(true);
        assert_eq!(view.cursor, 1);
        view.next_match(false);
        assert_eq!(view.cursor, 4);
        view.set_search("");
        assert!(view.matches.is_empty());
    }
}
