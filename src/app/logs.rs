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
        };
        v.recompute_visible();
        v
    }

    pub fn has_groups(&self) -> bool {
        !self.groups.is_empty()
    }

    fn recompute_visible(&mut self) {
        let prev = self.visible.get(self.cursor).copied();
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
        self.cursor = prev
            .and_then(|p| self.visible.iter().position(|&i| i == p))
            .unwrap_or_else(|| self.cursor.min(self.visible.len().saturating_sub(1)));
    }

    pub fn move_cursor(&mut self, delta: i32) {
        if self.visible.is_empty() {
            return;
        }
        let c = self.cursor as i32 + delta;
        self.cursor = c.clamp(0, self.visible.len() as i32 - 1) as usize;
    }

    pub fn cursor_to(&mut self, top: bool) {
        self.cursor = if top {
            0
        } else {
            self.visible.len().saturating_sub(1)
        };
    }

    /// Fold/unfold the group the cursor sits in, keeping the cursor on its header.
    pub fn toggle_fold(&mut self) {
        let Some(&src) = self.visible.get(self.cursor) else {
            return;
        };
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
        self.visible.get(self.cursor).copied()
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
        self.matches.clear();
        self.match_idx = None;
        if self.search.is_empty() {
            return;
        }
        let q = self.search.to_lowercase();
        for i in 0..self.lines.len() {
            if self.is_endgroup[i] {
                continue;
            }
            if log_content(&self.lines[i]).to_lowercase().contains(&q) {
                self.matches.push(i);
            }
        }
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
}
