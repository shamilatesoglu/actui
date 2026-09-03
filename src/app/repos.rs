//! The repos sidebar: every watched repo, in a predictable order, plus the
//! scope it puts on the runs list. Row 0 of the pane is "All repos" — no scope
//! at all — so a repo in `rows` sits at pane index + 1.

use crate::config::Config;
use crate::github::{Run, RunState};
use crate::state::State;
use chrono::{DateTime, Utc};
use ratatui::widgets::ListState;
use std::collections::{HashMap, HashSet};

/// UI ticks per column of marquee movement (the tick is 120ms).
const SLIDE_TICKS: u8 = 2;

/// What decides the order below the pinned repos.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    /// A→Z, so a repo is always where you last saw it.
    Alpha,
    /// The repos you work in most, decayed week by week.
    Used,
}

impl Sort {
    /// Anything unrecognised reads as the predictable one.
    fn from_config(name: &str) -> Self {
        match name {
            "used" | "frecency" => Sort::Used,
            _ => Sort::Alpha,
        }
    }
}

/// One repo row: the repo plus a rollup of its runs.
pub struct RepoRow {
    pub name: String,
    pub pinned: bool,
    /// Runs queued or in progress.
    pub active: usize,
    pub failed: usize,
    /// Latest run activity, for the age column.
    pub last: Option<DateTime<Utc>>,
}

pub struct ReposPane {
    /// What the user asked for (config `sidebar`, toggled with `p`).
    pub visible: bool,
    /// True while a name is too long for the pane and is sliding past. Set at
    /// draw time; it's what tells the clock there's an animation to run.
    pub sliding: bool,
    /// The marquee's position, in columns.
    pub step: usize,
    /// Sub-counter, so the marquee moves slower than the 120ms UI tick.
    ticks: u8,
    /// Whether it actually made it on screen — a narrow terminal drops it, and
    /// a pane you can't see must not filter the runs list. Set while drawing.
    pub shown: bool,
    /// What orders the rows below the pinned repos.
    pub sort: Sort,
    pub rows: Vec<RepoRow>,
    pub state: ListState,
    /// Repos to keep at the top, in the order the config lists them.
    pinned: Vec<String>,
    /// Every repo the sweep watches, including those with no recent runs.
    known: Vec<String>,
}

impl ReposPane {
    pub fn new(cfg: &Config) -> Self {
        let mut state = ListState::default();
        state.select(Some(0)); // "All repos"
        Self {
            visible: cfg.sidebar,
            sort: Sort::from_config(&cfg.sort),
            sliding: false,
            step: 0,
            ticks: 0,
            shown: false,
            rows: Vec::new(),
            state,
            pinned: cfg.pinned.clone(),
            known: Vec::new(),
        }
    }

    /// Advance the marquee. Returns true when the screen needs a repaint —
    /// only while a name is actually mid-slide, so an idle sidebar costs
    /// nothing.
    pub fn tick(&mut self) -> bool {
        if !self.shown || !self.sliding {
            self.ticks = 0;
            return false;
        }
        self.ticks += 1;
        if self.ticks < SLIDE_TICKS {
            return false;
        }
        self.ticks = 0;
        self.step += 1;
        true
    }

    /// The repos, plus the "All repos" row above them.
    pub fn len(&self) -> usize {
        self.rows.len() + 1
    }

    /// The repo the cursor is on; `None` on the "All repos" row.
    pub fn selected_repo(&self) -> Option<&str> {
        let i = self.state.selected()?;
        Some(self.rows.get(i.checked_sub(1)?)?.name.as_str())
    }

    /// Put the cursor on a repo by name, scoping the runs list to it. False
    /// when that repo isn't listed.
    pub fn select_repo(&mut self, name: &str) -> bool {
        match self.rows.iter().position(|r| r.name == name) {
            Some(at) => {
                self.select(at + 1);
                true
            }
            None => false,
        }
    }

    /// The repo the runs list is limited to, if any.
    pub fn scope(&self) -> Option<&str> {
        self.shown.then(|| self.selected_repo()).flatten()
    }

    pub fn in_scope(&self, repo: &str) -> bool {
        self.scope().is_none_or(|s| s == repo)
    }

    pub fn move_sel(&mut self, delta: i32) {
        let cur = self.state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, self.len() as i32 - 1) as usize;
        self.state.select(Some(next));
    }

    pub fn jump(&mut self, top: bool) {
        self.state.select(Some(if top { 0 } else { self.len() - 1 }));
    }

    pub fn select(&mut self, i: usize) {
        self.state.select(Some(i.min(self.len() - 1)));
    }

    /// Back to "All repos".
    pub fn clear_scope(&mut self) {
        self.state.select(Some(0));
    }

    /// Repos the sweep is watching, whether or not they have runs to show. This
    /// is what lets you reach a repo that hasn't built in weeks.
    pub fn set_known(&mut self, names: Vec<String>) {
        self.known = names;
    }

    /// Rebuild the rows from the current runs. Pinned repos come first, in the
    /// order they're configured; the rest are alphabetical, or ordered by use
    /// under `sort = "used"`. The cursor stays on the same repo across a
    /// reorder.
    ///
    /// Under `used`, only a refresh re-sorts (`resort`): working in a repo
    /// raises its score, and re-sorting on every keystroke would shuffle the
    /// list under the cursor while you're moving through it. Alphabetical order
    /// can't shift under you, so it always sorts. The counts update either way.
    pub fn rebuild(&mut self, runs: &[Run], resort: bool, uses: &State) {
        let mut rollup: HashMap<&str, (usize, usize, Option<DateTime<Utc>>)> = HashMap::new();
        for r in runs {
            let e = rollup.entry(r.repository.full_name.as_str()).or_default();
            match r.state() {
                RunState::Queued | RunState::Running => e.0 += 1,
                RunState::Failure => e.1 += 1,
                _ => {}
            }
            e.2 = e.2.max(Some(r.updated_at));
        }

        let mut seen: HashSet<&str> = HashSet::new();
        let mut names: Vec<&str> = Vec::new();
        let sources = self
            .pinned
            .iter()
            .chain(self.known.iter())
            .map(String::as_str)
            .chain(rollup.keys().copied());
        for name in sources {
            if seen.insert(name) {
                names.push(name);
            }
        }

        let mut rows: Vec<RepoRow> = names
            .into_iter()
            .map(|name| {
                let (active, failed, last) = rollup.get(name).copied().unwrap_or_default();
                RepoRow {
                    name: name.to_string(),
                    pinned: self.pin_rank(name).is_some(),
                    active,
                    failed,
                    last,
                }
            })
            .collect();
        if self.sort == Sort::Alpha || resort || self.rows.is_empty() {
            rows.sort_by(|a, b| {
                let pins = self
                    .pin_rank(&a.name)
                    .unwrap_or(usize::MAX)
                    .cmp(&self.pin_rank(&b.name).unwrap_or(usize::MAX));
                let alpha = || a.name.to_lowercase().cmp(&b.name.to_lowercase());
                match self.sort {
                    Sort::Alpha => pins.then_with(alpha),
                    Sort::Used => pins
                        .then_with(|| uses.score(&b.name).total_cmp(&uses.score(&a.name)))
                        .then_with(|| b.last.cmp(&a.last))
                        .then_with(alpha),
                }
            });
        } else {
            // Hold the order you're looking at; repos we haven't shown yet go last.
            let at: HashMap<String, usize> = self
                .rows
                .iter()
                .enumerate()
                .map(|(i, r)| (r.name.clone(), i))
                .collect();
            rows.sort_by_key(|r| at.get(&r.name).copied().unwrap_or(usize::MAX));
        }

        let keep = self.selected_repo().map(str::to_string);
        self.rows = rows;
        // Keep the cursor on the same repo, or back at "All repos" when that
        // repo is no longer listed.
        if !keep.is_some_and(|name| self.select_repo(&name)) {
            self.clear_scope();
        }
    }

    fn pin_rank(&self, name: &str) -> Option<usize> {
        self.pinned.iter().position(|p| p.eq_ignore_ascii_case(name))
    }
}
