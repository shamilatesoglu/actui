//! The log-viewer model: parses a job's raw text logs into a foldable tree of
//! `##[group]` steps, tracks the visible (post-fold) line set, derives per-step
//! durations from line timestamps, and drives in-log search. Self-contained —
//! it holds no reference to `App`.

use chrono::{DateTime, Utc};

pub struct LogGroup {
    pub collapsed: bool,
    pub body_count: usize,
    /// Wall-clock seconds for the step, derived from line timestamps.
    pub secs: Option<f64>,
}

/// Live per-step view shown while a job is still running (text logs aren't
/// available from the API until the job completes). Steps are read live from
/// `App::jobs`, so this stays current as the job is polled.
pub struct StepsView {
    pub job_id: u64,
    pub job_name: String,
    pub repo: String,
    pub cursor: usize,
}

pub struct LogsView {
    pub title: String,
    pub lines: Vec<String>,
    /// Group id each line belongs to (None = outside any group).
    pub line_group: Vec<Option<usize>>,
    pub is_header: Vec<bool>,
    pub is_endgroup: Vec<bool>,
    pub groups: Vec<LogGroup>,
    /// Source-line indices currently shown (folds applied).
    pub visible: Vec<usize>,
    /// Cursor position within `visible`.
    pub cursor: usize,
    /// Horizontal scroll offset (columns), for lines wider than the pane.
    pub hscroll: u16,
    /// In-log search.
    pub search: String,
    pub searching: bool,
    pub matches: Vec<usize>, // source-line indices containing the query (sorted)
    pub match_idx: Option<usize>,
    /// Lowercased `log_content` of every line, built on the first search so
    /// per-keystroke scans don't lowercase the whole log again.
    lower: Vec<String>,
    /// The query `matches` was last computed for (lowercased). When the new
    /// query extends it, the match set can only narrow — no full rescan.
    last_needle: String,
    pub preview_only: bool,
}

/// Strip a leading BOM, trailing CR/LF, and the ISO timestamp prefix.
pub(crate) fn log_content(raw: &str) -> &str {
    let s = raw
        .trim_start_matches('\u{feff}')
        .trim_end_matches(['\r', '\n']);
    if let Some((first, rest)) = s.split_once(' ') {
        if first.len() >= 20 && first.contains('T') && first.contains(':') {
            return rest;
        }
    }
    s
}

/// Words that mark a log line as a failure. Matched whole, so a word merely
/// glued into a path, a filename, a flag or an identifier — `Error.cpp`,
/// `-ErrorAction`, `thiserror` — is not this word. See `words`.
const ERROR_WORDS: &[&str] = &[
    "error", "errors",
    "fail", "fails", "failed", "failing", "failure", "failures",
    "fatal",
    "panic", "panics", "panicked",
    "exception", "exceptions",
    "traceback",
    "segfault",
    "abort", "aborts", "aborted", "aborting",
    "unable",
    "non-zero",
];

/// Failures no single word can carry: `fault`, `found` and `denied` each say
/// too little alone, so these are matched as whole phrases.
const ERROR_PHRASES: &[&str] = &[
    "segmentation fault",
    "command not found",
    "no such file or directory",
    "permission denied",
    "connection refused",
    "cannot find path",
    "npm err!",
];

/// Words that only mean trouble when there is at least one: a build that
/// reports `0 errors` is a build that worked.
const COUNTED_WORDS: &[&str] = &["error", "errors", "fail", "fails", "failed", "failure", "failures"];

/// Whether a log line looks like an error/failure, for the failure preview.
pub(crate) fn is_error_line(content: &str) -> bool {
    let content = content.trim_start();
    // GitHub's annotation markers (`##[error]…`) and workflow-command form
    // (`::error file=…::`) are unambiguous prefixes.
    if content.starts_with("##[error]") || content.starts_with("::error") {
        return true;
    }
    // A comma or semicolon starts a fresh tally, so Maven's `Tests run: 12,
    // Failures: 0` reads as no failures rather than as twelve of them.
    content.split([',', ';']).any(reports_a_failure)
        || ERROR_PHRASES.iter().any(|phrase| contains_ignoring_case(content, phrase))
}

/// Whether one stretch of a line reports a failure.
fn reports_a_failure(tally: &str) -> bool {
    let words: Vec<&str> = words(tally).collect();
    words.iter().enumerate().any(|(i, word)| {
        if is_error_type(word) {
            return true;
        }
        if !ERROR_WORDS.iter().any(|w| word.eq_ignore_ascii_case(w)) {
            return false;
        }
        if !COUNTED_WORDS.iter().any(|w| word.eq_ignore_ascii_case(w)) {
            return true;
        }
        // A counted word takes its count from the number beside it. The one in
        // front wins; the one behind speaks only when nothing counts from the
        // front, which is how `Failures: 0` reports none of them.
        let before = i.checked_sub(1).map(|j| words[j]);
        let count = match before {
            Some(w) if says_none(w) || w.bytes().all(|b| b.is_ascii_digit()) => Some(w),
            _ => words.get(i + 1).copied(),
        };
        !count.is_some_and(says_none)
    })
}

/// Whether a word is a count of none, right beside something counted.
fn says_none(word: &str) -> bool {
    ["0", "no", "zero"].iter().any(|w| word.eq_ignore_ascii_case(w))
}

/// Split a log line into whole words. Alphanumerics plus the characters that
/// hold an identifier or a path together — `.` `/` `\` `_` `-` and a doubled
/// `::` — stay inside one word, so `Error.cpp`, `-ErrorAction` and
/// `std::error::Error` come out whole instead of shedding a bare `error`. A
/// lone `:` splits, so `error: boom` and Python's `ERROR:root:message` don't
/// hide their first word. Glue on either end is trimmed, so a sentence's
/// `failed.` is still `failed`.
fn words(line: &str) -> impl Iterator<Item = &str> {
    const GLUE: [char; 6] = ['.', '/', '\\', '_', '-', ':'];
    let bytes = line.as_bytes();
    // Only a doubled colon holds a word together; everything else is one byte.
    let joins = |i: usize| match bytes[i] {
        b':' => bytes.get(i + 1) == Some(&b':'),
        b => b.is_ascii_alphanumeric() || GLUE.contains(&(b as char)),
    };

    let mut i = 0;
    std::iter::from_fn(move || {
        while i < bytes.len() {
            while i < bytes.len() && !joins(i) {
                i += 1;
            }
            let start = i;
            while i < bytes.len() && joins(i) {
                i += if bytes[i] == b':' { 2 } else { 1 };
            }
            let word = line[start..i].trim_matches(GLUE);
            if !word.is_empty() {
                return Some(word);
            }
        }
        None
    })
}

/// Whether a word names an error type — `AssertionError`,
/// `java.lang.NullPointerException` — the way a runtime names what went
/// wrong. The capital is what makes it a name, and it keeps the `thiserror`
/// crate and a bare `std::error::Error` in a compiler note out of it.
fn is_error_type(word: &str) -> bool {
    let name = word.rsplit(['.', '/', '\\', ':']).next().unwrap_or(word);
    ["Error", "Exception"].iter().any(|suffix| {
        name.strip_suffix(suffix)
            .is_some_and(|stem| !stem.is_empty() && stem.chars().all(|c| c.is_ascii_alphanumeric()))
    })
}

/// Whether `needle` — which must be lowercase — appears anywhere in
/// `haystack`, ignoring case.
fn contains_ignoring_case(haystack: &str, needle: &str) -> bool {
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

/// Parse the RFC3339 timestamp GitHub prefixes onto each log line.
fn line_time(raw: &str) -> Option<DateTime<Utc>> {
    let tok = raw.trim_start_matches('\u{feff}').split(' ').next()?;
    DateTime::parse_from_rfc3339(tok)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

impl LogsView {
    pub fn new(title: String, text: &str) -> Self {
        let lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
        let n = lines.len();
        let mut line_group = vec![None; n];
        let mut is_header = vec![false; n];
        let mut is_endgroup = vec![false; n];
        let mut groups: Vec<LogGroup> = Vec::new();
        let mut had_problem: Vec<bool> = Vec::new();
        let mut g_start: Vec<Option<DateTime<Utc>>> = Vec::new();
        let mut g_end: Vec<Option<DateTime<Utc>>> = Vec::new();
        let mut current: Option<usize> = None;

        for (i, raw) in lines.iter().enumerate() {
            let c = log_content(raw);
            let t = line_time(raw);
            if c.starts_with("##[group]") {
                let gid = groups.len();
                groups.push(LogGroup { collapsed: true, body_count: 0, secs: None });
                had_problem.push(false);
                g_start.push(t);
                g_end.push(t);
                is_header[i] = true;
                line_group[i] = Some(gid);
                current = Some(gid);
            } else if c.starts_with("##[endgroup]") {
                is_endgroup[i] = true;
                line_group[i] = current;
                if let (Some(g), Some(t)) = (current, t) {
                    g_end[g] = Some(t);
                }
                current = None;
            } else {
                line_group[i] = current;
                if let Some(g) = current {
                    groups[g].body_count += 1;
                    if let Some(t) = t {
                        g_end[g] = Some(t);
                    }
                    if c.starts_with("##[error]") || c.starts_with("##[warning]") {
                        had_problem[g] = true;
                    }
                }
            }
        }
        // Auto-expand groups that contain an error/warning so problems aren't hidden.
        for (g, problem) in had_problem.iter().enumerate() {
            if *problem {
                groups[g].collapsed = false;
            }
        }
        // Per-step duration from first→last line timestamp.
        for g in 0..groups.len() {
            if let (Some(a), Some(b)) = (g_start[g], g_end[g]) {
                groups[g].secs = Some((b - a).num_milliseconds().max(0) as f64 / 1000.0);
            }
        }

        let mut v = Self {
            title,
            lines,
            line_group,
            is_header,
            is_endgroup,
            groups,
            visible: Vec::new(),
            cursor: 0,
            hscroll: 0,
            search: String::new(),
            searching: false,
            matches: Vec::new(),
            match_idx: None,
            lower: Vec::new(),
            last_needle: String::new(),
            preview_only: false,
        };
        v.recompute_visible();
        v
    }

    pub fn has_groups(&self) -> bool {
        !self.groups.is_empty()
    }

    pub(crate) fn recompute_visible(&mut self) {
        let prev = self.visible.get(self.cursor).copied();
        if self.preview_only {
            let n = self.lines.len();
            let mut should_include = vec![false; n];
            for (i, line) in self.lines.iter().enumerate() {
                if is_error_line(log_content(line)) {
                    // The error line, plus three lines of context either side.
                    should_include[i.saturating_sub(3)..(i + 4).min(n)].fill(true);
                }
            }

            self.visible.clear();
            let mut in_gap = false;
            for (i, keep) in should_include.iter().enumerate() {
                if *keep {
                    if in_gap {
                        self.visible.push(usize::MAX);
                        in_gap = false;
                    }
                    self.visible.push(i);
                } else if !self.visible.is_empty() {
                    in_gap = true;
                }
            }
        } else {
            self.visible = (0..self.lines.len())
                .filter(|&i| {
                    if self.is_endgroup[i] {
                        return false; // endgroup markers never render
                    }
                    match self.line_group[i] {
                        None => true,
                        Some(g) => self.is_header[i] || !self.groups[g].collapsed,
                    }
                })
                .collect();
        }
        self.cursor = prev
            .and_then(|p| self.visible.iter().position(|&i| i == p))
            .unwrap_or_else(|| self.cursor.min(self.visible.len().saturating_sub(1)));
    }

    pub fn move_cursor(&mut self, delta: i32) {
        if self.visible.is_empty() {
            return;
        }
        let n = self.visible.len();
        let mut c = self.cursor as i32 + delta;
        c = c.clamp(0, n as i32 - 1);
        if self.visible[c as usize] == usize::MAX {
            let step = if delta >= 0 { 1 } else { -1 };
            let mut next_c = c + step;
            while next_c >= 0 && next_c < n as i32 {
                if self.visible[next_c as usize] != usize::MAX {
                    c = next_c;
                    break;
                }
                next_c += step;
            }
        }
        self.cursor = c as usize;
    }

    /// Put the cursor on an absolute row of the visible lines — what dragging
    /// the scrollbar asks for.
    pub fn cursor_to_index(&mut self, at: usize) {
        if self.visible.is_empty() {
            return;
        }
        self.cursor = at.min(self.visible.len() - 1);
        if self.visible[self.cursor] == usize::MAX {
            self.move_cursor(1); // never rest on a gap marker
        }
    }

    pub fn cursor_to(&mut self, top: bool) {
        self.cursor = if top {
            0
        } else {
            self.visible.len().saturating_sub(1)
        };
        if !self.visible.is_empty() && self.visible[self.cursor] == usize::MAX {
            self.move_cursor(if top { 1 } else { -1 });
        }
    }

    /// Fold/unfold the group the cursor sits in, keeping the cursor on its header.
    pub fn toggle_fold(&mut self) {
        let Some(&src) = self.visible.get(self.cursor) else {
            return;
        };
        if src == usize::MAX {
            return;
        }
        let Some(g) = self.line_group[src] else {
            return;
        };
        self.groups[g].collapsed = !self.groups[g].collapsed;
        let header = (0..self.lines.len()).find(|&i| self.is_header[i] && self.line_group[i] == Some(g));
        self.recompute_visible();
        if let Some(h) = header {
            if let Some(pos) = self.visible.iter().position(|&i| i == h) {
                self.cursor = pos;
            }
        }
    }

    pub fn set_all_collapsed(&mut self, collapsed: bool) {
        for g in &mut self.groups {
            g.collapsed = collapsed;
        }
        self.recompute_visible();
    }

    fn current_src(&self) -> Option<usize> {
        let &src = self.visible.get(self.cursor)?;
        if src == usize::MAX {
            None
        } else {
            Some(src)
        }
    }

    /// Move the cursor to a source line, expanding its group if folded.
    fn cursor_to_src(&mut self, src: usize) {
        if let Some(g) = self.line_group[src] {
            if self.groups[g].collapsed {
                self.groups[g].collapsed = false;
                self.recompute_visible();
            }
        }
        if let Some(pos) = self.visible.iter().position(|&i| i == src) {
            self.cursor = pos;
        }
    }

    /// Recompute matches for the current query and jump to the nearest one.
    pub fn update_search(&mut self) {
        self.match_idx = None;
        if self.search.is_empty() {
            self.matches.clear();
            self.last_needle.clear();
            return;
        }
        let q = self.search.to_lowercase();
        if self.lower.len() != self.lines.len() {
            self.lower = self.lines.iter().map(|l| log_content(l).to_lowercase()).collect();
        }
        if !self.last_needle.is_empty() && q.starts_with(&self.last_needle) {
            // Typing extended the needle: narrow the previous match set.
            let lower = &self.lower;
            self.matches.retain(|&i| lower[i].contains(&q));
        } else {
            self.matches = (0..self.lines.len())
                .filter(|&i| !self.is_endgroup[i] && self.lower[i].contains(&q))
                .collect();
        }
        self.last_needle = q;
        // Reveal every group that holds a hit so matches are reachable.
        for &i in &self.matches {
            if let Some(g) = self.line_group[i] {
                self.groups[g].collapsed = false;
            }
        }
        self.recompute_visible();
        if self.matches.is_empty() {
            return;
        }
        // Jump to the first match at or after the cursor (wrapping).
        let here = self.current_src().unwrap_or(0);
        let idx = self.matches.iter().position(|&m| m >= here).unwrap_or(0);
        self.match_idx = Some(idx);
        self.cursor_to_src(self.matches[idx]);
    }

    pub fn next_match(&mut self, dir: i32) {
        if self.matches.is_empty() {
            return;
        }
        let n = self.matches.len() as i32;
        let cur = self.match_idx.map(|i| i as i32).unwrap_or(-1);
        let ni = (((cur + dir) % n) + n) % n;
        self.match_idx = Some(ni as usize);
        self.cursor_to_src(self.matches[ni as usize]);
    }

    pub fn clear_search(&mut self) {
        self.search.clear();
        self.searching = false;
        self.matches.clear();
        self.match_idx = None;
        self.last_needle.clear();
    }

    pub fn is_match(&self, src: usize) -> bool {
        self.matches.binary_search(&src).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mirrors the real GitHub log shape: timestamps, a BOM, two groups
    // (one clean, one containing an error), and endgroup markers.
    const SAMPLE: &str = "\u{feff}2026-06-03T09:13:47.0000000Z Current runner version\n\
2026-06-03T09:13:47.0000000Z ##[group]GITHUB_TOKEN Permissions\n\
2026-06-03T09:13:47.0000000Z Contents: read\n\
2026-06-03T09:13:47.0000000Z Metadata: read\n\
2026-06-03T09:13:47.0000000Z ##[endgroup]\n\
2026-06-03T09:13:47.0000000Z Secret source: Actions\n\
2026-06-03T09:13:47.0000000Z ##[group]Run build\n\
2026-06-03T09:13:47.0000000Z ##[error]boom\n\
2026-06-03T09:13:47.0000000Z ##[endgroup]\n";

    fn src_lines(lv: &LogsView) -> Vec<&str> {
        lv.visible.iter().map(|&i| log_content(&lv.lines[i])).collect()
    }

    #[test]
    fn folds_clean_group_autoexpands_error_group() {
        let lv = LogsView::new("t".into(), SAMPLE);
        assert_eq!(lv.groups.len(), 2);
        // Group 0 (clean) starts collapsed; group 1 (has error) auto-expands.
        assert!(lv.groups[0].collapsed);
        assert!(!lv.groups[1].collapsed);
        // endgroup markers and the collapsed body are hidden.
        let v = src_lines(&lv);
        assert_eq!(
            v,
            vec![
                "Current runner version",
                "##[group]GITHUB_TOKEN Permissions",
                "Secret source: Actions",
                "##[group]Run build",
                "##[error]boom",
            ]
        );
        assert_eq!(lv.groups[0].body_count, 2);
    }

    #[test]
    fn toggle_expands_and_collapses() {
        let mut lv = LogsView::new("t".into(), SAMPLE);
        lv.cursor = 1; // on the "GITHUB_TOKEN Permissions" header
        lv.toggle_fold();
        let v = src_lines(&lv);
        assert!(v.contains(&"Contents: read"));
        assert!(v.contains(&"Metadata: read"));
        // Cursor stays on the header after toggling.
        assert_eq!(log_content(&lv.lines[lv.visible[lv.cursor]]), "##[group]GITHUB_TOKEN Permissions");
        lv.toggle_fold();
        assert!(!src_lines(&lv).contains(&"Contents: read"));
    }

    #[test]
    fn search_finds_and_reveals_folded_hits() {
        let mut lv = LogsView::new("t".into(), SAMPLE);
        // "read" lives inside the collapsed clean group.
        assert!(lv.groups[0].collapsed);
        lv.search = "read".into();
        lv.update_search();
        assert_eq!(lv.matches.len(), 2); // "Contents: read", "Metadata: read"
        assert!(!lv.groups[0].collapsed); // search revealed the folded group
        // Both matches are now visible and cursor sits on the first.
        assert_eq!(log_content(&lv.lines[lv.visible[lv.cursor]]), "Contents: read");
        lv.next_match(1);
        assert_eq!(log_content(&lv.lines[lv.visible[lv.cursor]]), "Metadata: read");
        lv.next_match(1); // wraps
        assert_eq!(log_content(&lv.lines[lv.visible[lv.cursor]]), "Contents: read");
    }

    #[test]
    fn computes_step_duration_from_timestamps() {
        let text = "2026-06-03T09:13:47.0000000Z ##[group]Build\n\
2026-06-03T09:13:49.5000000Z compiling\n\
2026-06-03T09:13:50.0000000Z ##[endgroup]\n";
        let lv = LogsView::new("t".into(), text);
        let s = lv.groups[0].secs.expect("duration");
        assert!((s - 3.0).abs() < 0.01, "expected ~3.0s, got {s}");
    }

    #[test]
    fn extending_a_search_narrows_then_shrinking_rescans() {
        let mut lv = LogsView::new("t".into(), SAMPLE);
        lv.search = "read".into();
        lv.update_search();
        assert_eq!(lv.matches.len(), 2);
        // Extending the needle narrows the existing match set.
        lv.search = "readx".into();
        lv.update_search();
        assert!(lv.matches.is_empty());
        // Shrinking it falls back to a full rescan and recovers the matches.
        lv.search = "read".into();
        lv.update_search();
        assert_eq!(lv.matches.len(), 2);
    }

    #[test]
    fn search_no_match_is_empty() {
        let mut lv = LogsView::new("t".into(), SAMPLE);
        lv.search = "zzzznope".into();
        lv.update_search();
        assert!(lv.matches.is_empty());
        assert!(lv.match_idx.is_none());
    }

    #[test]
    fn expand_and_fold_all() {
        let mut lv = LogsView::new("t".into(), SAMPLE);
        lv.set_all_collapsed(false);
        assert!(src_lines(&lv).contains(&"Contents: read"));
        lv.set_all_collapsed(true);
        let v = src_lines(&lv);
        assert!(!v.contains(&"Contents: read"));
        assert!(!v.contains(&"##[error]boom")); // even the error group folds on "fold all"
    }

    #[test]
    fn preview_only_shows_error_context() {
        let mut lv = LogsView::new("t".into(), SAMPLE);
        lv.preview_only = true;
        lv.recompute_visible();
        assert!(lv.visible.contains(&7)); // error line
        assert!(lv.visible.contains(&6)); // context before
        assert!(lv.visible.contains(&5)); // context before
        assert!(lv.visible.contains(&4)); // context before (with 3-line context)
        assert!(lv.visible.contains(&8)); // context after
        assert!(!lv.visible.contains(&3)); // unrelated body line excluded
    }

    /// Lines a run really did fail on. Every one is output a tool actually
    /// prints: GitHub's markers, compilers, test runners, shells, package
    /// managers and PowerShell.
    const FAILING_LINES: &[&str] = &[
        // GitHub's own markers.
        "##[error]Process completed with exit code 1.",
        "::error file=app.js,line=1::Missing semicolon",
        // Plain prose.
        "the build failed",
        "Error: something broke",
        "2 errors, 0 warnings",
        "1 error generated.",
        "fatal: unable to access repo",
        "process aborted",
        "step(fail)",
        // Compilers and build tools.
        "error[E0308]: mismatched types",
        "error: could not compile `actui` (bin \"actui\") due to 2 previous errors",
        r"src\app\Error.cpp(42,17): error C2065: 'x': undeclared identifier",
        "LINK : fatal error LNK1104: cannot open file 'foo.lib'",
        "error TS2345: Argument of type 'string' is not assignable",
        "make: *** [Makefile:12: all] Error 1",
        "FAILED: build.ninja",
        // Test runners.
        "test result: FAILED. 41 passed; 1 failed; 0 ignored",
        "assertion `left == right` failed",
        "E   AssertionError: assert 1 == 2",
        // Runtimes.
        "thread 'main' panicked at src/lib.rs",
        "ValueError: invalid literal for int() with base 10",
        "Exception in thread \"main\" java.lang.IllegalStateException",
        "panic: runtime error: index out of range",
        "Traceback (most recent call last):",
        "terminate called after throwing an exception",
        "java.lang.NullPointerException: Cannot invoke \"String.length()\"",
        "Segmentation fault (core dumped)",
        // Shells and package managers.
        "bash: line 1: cargo: command not found",
        "cp: cannot stat 'x': No such file or directory",
        "Permission denied (publickey).",
        "npm ERR! code ELIFECYCLE",
        "ERROR:root:database is locked",
        "docker: Error response from daemon",
        "The command '/bin/sh -c npm ci' returned a non-zero code: 1",
        // PowerShell.
        r"Get-Item: Cannot find path 'C:\nope' because it does not exist.",
        r"    + CategoryInfo          : ObjectNotFound: (C:\nope:String) [Get-Item], ItemNotFoundException",
    ];

    /// Lines that only mention an error — in a path, a filename, a flag, an
    /// identifier, a crate name, or a count of zero. None of these is a
    /// failure and none may be flagged.
    const CLEAN_LINES: &[&str] = &[
        // Substrings that aren't the word.
        "Run set -o pipefail",
        "export SHELLOPTS=pipefail",
        "downloaded hispanic-locale.tar",
        "collaborators added",
        // Flags and parameters.
        "Get-ChildItem -Path $dir -ErrorAction Stop",
        "Remove-Item -Recurse -ErrorAction SilentlyContinue",
        "$ErrorActionPreference = \"Stop\"",
        "cargo build --error-format=json",
        // Filenames and paths.
        "Compiling Error.cpp",
        r"cl /c src\app\Error.cpp",
        r"Get-Content .\Error.log",
        "cargo build 2> errors.txt",
        // Crate, package and identifier names.
        "   Compiling thiserror v1.0.69",
        "   Compiling error-chain v0.12.4",
        "  Downloading error-stack v0.4.1",
        "set_error_handler(handler)",
        "test app::logs::tests::flags_every_failing_line ... ok",
        "if %ERRORLEVEL% neq 0 exit /b 1",
        "Error-free build in 2.3s",
        "if err != nil { return fmt.Errorf(\"%w\", err) }",
        "note: required because of the impl of std::error::Error",
        // Counts of zero.
        "0 errors, 0 warnings",
        "[INFO] Tests run: 12, Failures: 0, Errors: 0, Skipped: 0",
        "Build succeeded. 0 Warning(s) 0 Error(s)",
        "test result: ok. 41 passed; 0 failed; 0 ignored",
        "Write-Host \"No errors found\"",
        // Warnings are not errors.
        "warning: unused variable: `x`",
        "##[warning]Node.js 16 actions are deprecated",
    ];

    #[test]
    fn reads_odd_lines_without_panicking() {
        // Words are sliced out by byte index, so a multi-byte char next to one
        // must not land mid-character.
        assert!(is_error_line("❌ error: boom"));
        assert!(is_error_line("日本語 failed"));
        assert!(!is_error_line("─── build ───"));
        assert!(!is_error_line("✅ all good"));
        // Nothing to read, or nothing but glue.
        assert!(!is_error_line(""));
        assert!(!is_error_line("   "));
        assert!(!is_error_line("::::: ... ---"));
        // A very long line is still just a line.
        assert!(!is_error_line(&"a".repeat(10_000)));
    }

    #[test]
    fn flags_every_failing_line() {
        let missed: Vec<&&str> = FAILING_LINES.iter().filter(|l| !is_error_line(l)).collect();
        assert!(missed.is_empty(), "missed {} real failures: {missed:#?}", missed.len());
    }

    #[test]
    fn flags_no_line_that_merely_mentions_an_error() {
        let wrong: Vec<&&str> = CLEAN_LINES.iter().filter(|l| is_error_line(l)).collect();
        assert!(wrong.is_empty(), "{} false positives: {wrong:#?}", wrong.len());
    }
}
