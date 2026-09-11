//! The duration chart's model: one workflow's successful runs, the summary
//! numbers under them, and how those runs are laid out across however many
//! columns the terminal gives us. All arithmetic, no drawing — `ui::timing`
//! turns a `Layout` into glyphs.

use crate::github::{Run, RunState, Workflow};
use chrono::{DateTime, Utc};

/// How far back the chart reads. GitHub's cap for one page, and plenty of
/// history for spotting a workflow getting slower.
pub const HISTORY: u32 = 100;

/// How many runs each side of the trend comparison looks at, at most.
const TREND_WINDOW: usize = 10;

/// Widest a run's slice of the chart gets, gap included. Past this a handful
/// of runs turns into a handful of slabs.
const MAX_STEP: usize = 12;

/// How many runs the smoothed curve looks at around each one: a tenth of the
/// history, but never fewer than this or more than that.
const SMOOTH_WINDOW: (usize, usize) = (5, 15);

/// One successful run on the chart.
#[derive(Clone)]
pub struct TimingPoint {
    pub run_id: u64,
    pub number: u64,
    /// Wall-clock seconds the run took.
    pub secs: i64,
    pub branch: String,
    pub event: String,
    pub finished: DateTime<Utc>,
    pub url: String,
}

/// What a fetch of a workflow's runs boils down to for the chart.
#[derive(Clone, Default)]
pub struct History {
    /// Successful runs with a known duration, oldest first.
    pub points: Vec<TimingPoint>,
    /// Successes GitHub no longer gives a finish time for, left off the chart.
    pub unknown: usize,
}

/// One drawn column of the chart. Where there are more runs than columns, a
/// column stands for several of them and reports the longest.
pub struct Bar {
    /// The column's height, in seconds — the longest run it covers.
    pub secs: i64,
    /// Index into `points` of that longest run: what the cursor lands on and
    /// the detail line describes.
    pub peak: usize,
    /// The runs this column covers: `first` and the `runs - 1` after it.
    pub first: usize,
    pub runs: usize,
}

impl Bar {
    /// The run in the middle of what this column covers — where the smoothed
    /// curve is read for it.
    pub fn middle(&self) -> usize {
        self.first + self.runs / 2
    }
}

/// The chart's shape for a given width.
pub struct Layout {
    pub bars: Vec<Bar>,
    /// Columns per bar: the bar itself and the gap after it. The runs spread
    /// out to fill the width they are given, up to `MAX_STEP`.
    pub step: usize,
    /// Columns the bar itself takes — the slice minus one gap column, once
    /// there is room for a gap at all.
    pub bar_w: usize,
}

impl Layout {
    fn spread(bars: Vec<Bar>, width: usize) -> Self {
        let step = (width / bars.len().max(1)).clamp(1, MAX_STEP);
        let bar_w = if step == 1 { 1 } else { step - 1 };
        Self { bars, step, bar_w }
    }

    /// How far in the first column sits. The runs are pushed against the right
    /// edge so the newest is always where the axis ends, and a short history
    /// reads as one — rather than as a chart with its right half missing.
    pub fn offset(&self, width: usize) -> usize {
        let used = self.bars.len().saturating_sub(1) * self.step + self.bar_w;
        width.saturating_sub(used)
    }

    /// The first column bar `i` is drawn in.
    pub fn col(&self, i: usize, width: usize) -> usize {
        self.offset(width) + i * self.step
    }

    /// The bar drawn at a column; the gap after a bar counts as that bar's.
    pub fn bar_of(&self, col: usize, width: usize) -> usize {
        let rel = col.saturating_sub(self.offset(width));
        (rel / self.step).min(self.bars.len().saturating_sub(1))
    }
}

/// The summary numbers under the chart.
pub struct Stats {
    pub min: i64,
    pub median: i64,
    pub max: i64,
    /// Where the top of the axis sits. One workflow that hung for half an hour
    /// would otherwise squash a hundred ordinary runs into a flat block, so the
    /// axis stops at the 95th percentile and the few runs above it are drawn
    /// full height and marked as running off the top.
    pub ceiling: i64,
    /// Where the bottom of the axis sits. CI durations cluster, so an axis from
    /// zero would spend most of its height on the part that never changes and
    /// leave no room for the part that does. The 5th percentile, with the floor
    /// named on the axis so it is clear the bars don't start at nothing.
    pub floor: i64,
    /// Percent change from the runs before last to the last few, once there is
    /// enough history to compare two windows.
    pub trend: Option<i64>,
}

/// Duration chart for one workflow, shown as a dedicated body pane (`w`).
pub struct TimingView {
    pub repo: String,
    pub workflow_id: u64,
    /// The workflow's name, for the title — known before its runs arrive.
    pub workflow: String,
    /// Every workflow in the repo, so `[`/`]` can move between them. Empty
    /// until the listing comes back.
    pub workflows: Vec<Workflow>,
    /// Successful runs, oldest first.
    pub points: Vec<TimingPoint>,
    /// Successes with no duration to plot — see `History::unknown`.
    pub unknown: usize,
    pub loaded: bool,
    /// Index into `points` of the run under the cursor.
    pub cursor: usize,
    /// Chart width at the last frame, so the movement keys know how many runs
    /// a column stands for.
    pub width: usize,
    /// A workflow step asked for before the workflow list had arrived, applied
    /// once it does.
    pub pending_step: Option<i32>,
}

impl TimingView {
    pub fn new(repo: String, workflow_id: u64, workflow: String) -> Self {
        Self {
            repo,
            workflow_id,
            workflow,
            workflows: Vec::new(),
            points: Vec::new(),
            unknown: 0,
            loaded: false,
            cursor: 0,
            width: 0,
            pending_step: None,
        }
    }

    /// Point at a different workflow of the same repo, keeping the workflow
    /// list we already have.
    pub fn retarget(&mut self, workflow_id: u64, workflow: String) {
        self.workflow_id = workflow_id;
        self.workflow = workflow;
        self.points.clear();
        self.unknown = 0;
        self.loaded = false;
        self.cursor = 0;
    }

    /// Take a fresh (or cached) reading, leaving the cursor on the newest run.
    pub fn set_history(&mut self, history: History) {
        self.points = history.points;
        self.unknown = history.unknown;
        self.loaded = true;
        self.cursor = self.points.len().saturating_sub(1);
    }

    pub fn selected(&self) -> Option<&TimingPoint> {
        self.points.get(self.cursor)
    }

    /// Lay the runs out across `width` columns: a slice each while they fit,
    /// widening to fill the room there is, otherwise folded into buckets so a
    /// long history compresses instead of running off the edge.
    pub fn layout(&self, width: usize) -> Layout {
        let n = self.points.len();
        if n == 0 || width == 0 {
            return Layout { bars: Vec::new(), step: 1, bar_w: 1 };
        }
        let secs = |i: usize| self.points[i].secs;
        if n <= width {
            let bars = (0..n).map(|i| Bar { secs: secs(i), peak: i, first: i, runs: 1 }).collect();
            return Layout::spread(bars, width);
        }
        // More runs than columns: each column covers an even slice of them and
        // reports its longest, so a spike survives the fold.
        let bars = (0..width)
            .map(|c| {
                let first = c * n / width;
                let last = ((c + 1) * n / width).max(first + 1);
                let peak = (first..last).max_by_key(|&i| secs(i)).unwrap_or(first);
                Bar { secs: secs(peak), peak, first, runs: last - first }
            })
            .collect();
        Layout { bars, step: 1, bar_w: 1 }
    }

    /// Move the cursor by whole columns, so it keeps step with what's drawn
    /// even where one column stands for several runs.
    pub fn move_cursor(&mut self, delta: i32) {
        let l = self.layout(self.width);
        if l.bars.is_empty() {
            return;
        }
        let at = self.bar_at(&l) as i32;
        let to = (at + delta).clamp(0, l.bars.len() as i32 - 1) as usize;
        self.cursor = l.bars[to].peak;
    }

    /// Put the cursor on the column at `col`, counted from the chart's left
    /// edge — where a click landed.
    pub fn click_col(&mut self, col: usize) {
        let l = self.layout(self.width);
        if l.bars.is_empty() {
            return;
        }
        self.cursor = l.bars[l.bar_of(col, self.width)].peak;
    }

    /// Jump to the oldest or newest run on the chart.
    pub fn jump(&mut self, oldest: bool) {
        if self.points.is_empty() {
            return;
        }
        self.cursor = if oldest { 0 } else { self.points.len() - 1 };
    }

    /// Which column the cursor's run is drawn in. Zero when there are no
    /// columns — a pane too narrow to plot anything still asks.
    pub fn bar_at(&self, l: &Layout) -> usize {
        if l.bars.is_empty() {
            return 0;
        }
        l.bars
            .iter()
            .position(|b| b.peak == self.cursor)
            // A resize can fold the cursor's run in beside a longer one, which
            // leaves it off the peaks. Fall back to the nearest column.
            .unwrap_or_else(|| {
                l.bars
                    .iter()
                    .position(|b| b.peak > self.cursor)
                    .unwrap_or(l.bars.len() - 1)
            })
    }

    /// Min, median, max, and which way the recent runs are heading.
    pub fn stats(&self) -> Option<Stats> {
        if self.points.is_empty() {
            return None;
        }
        let mut secs: Vec<i64> = self.points.iter().map(|p| p.secs).collect();
        secs.sort_unstable();
        Some(Stats {
            min: secs[0],
            median: median(&secs),
            max: secs[secs.len() - 1],
            ceiling: percentile_up(&secs, 95).max(1),
            floor: percentile(&secs, 5),
            trend: self.trend(),
        })
    }

    /// The runs with the noise taken out: each one replaced by the median of
    /// the few around it. A median rather than a mean, because the one run
    /// that hung would otherwise drag the curve up for a window either side of
    /// it — the very spike the curve is meant to see past.
    pub fn smoothed(&self) -> Vec<i64> {
        let n = self.points.len();
        let (least, most) = SMOOTH_WINDOW;
        let half = (n / 10).clamp(least, most) / 2;
        (0..n)
            .map(|i| median_of(&self.points[i.saturating_sub(half)..(i + half + 1).min(n)]))
            .collect()
    }

    /// Percent change between the median of the last few runs and the median of
    /// the few before them. `None` until there is enough history to mean
    /// anything.
    fn trend(&self) -> Option<i64> {
        let n = self.points.len();
        let win = (n / 2).min(TREND_WINDOW);
        if win < 4 {
            return None;
        }
        let recent = median_of(&self.points[n - win..]);
        let before = median_of(&self.points[n - win * 2..n - win]);
        if before == 0 {
            return None;
        }
        Some((recent - before) * 100 / before)
    }
}

/// The successful runs of a fetch, oldest first. GitHub returns newest first,
/// a run still going has no duration to plot, and one GitHub has housekept no
/// longer says how long it took.
pub fn points_of(runs: &[Run]) -> History {
    let mut history = History::default();
    for r in runs.iter().filter(|r| r.state() == RunState::Success && !r.is_pending()) {
        let Some(secs) = r.duration_secs() else {
            history.unknown += 1;
            continue;
        };
        history.points.push(TimingPoint {
            run_id: r.id,
            number: r.run_number,
            secs,
            branch: r.head_branch.clone().unwrap_or_default(),
            event: r.event.clone(),
            finished: r.last_activity(),
            url: r.html_url.clone(),
        });
    }
    history.points.sort_by_key(|p| (p.finished, p.number));
    history
}

fn median_of(points: &[TimingPoint]) -> i64 {
    let mut secs: Vec<i64> = points.iter().map(|p| p.secs).collect();
    secs.sort_unstable();
    median(&secs)
}

/// The value `pct` percent of the way up an already-sorted slice, rounded down.
fn percentile(sorted: &[i64], pct: usize) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[(sorted.len() - 1) * pct / 100]
}

/// The same, rounded up — so a handful of runs keeps its real slowest instead
/// of having it trimmed off as though it were an outlier.
fn percentile_up(sorted: &[i64], pct: usize) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[((sorted.len() - 1) * pct).div_ceil(100)]
}

/// Median of an already-sorted slice; the mean of the middle two when even.
fn median(sorted: &[i64]) -> i64 {
    match sorted.len() {
        0 => 0,
        n if n % 2 == 1 => sorted[n / 2],
        n => (sorted[n / 2 - 1] + sorted[n / 2]) / 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::{Run, RunRepo};
    use chrono::Duration;

    /// A finished run of `secs`, `mins_ago` minutes back.
    fn run(id: u64, number: u64, secs: i64, mins_ago: i64, conclusion: &str) -> Run {
        let end = Utc::now() - Duration::minutes(mins_ago);
        Run {
            id,
            name: Some("CI".into()),
            display_title: String::new(),
            head_branch: Some("main".into()),
            head_sha: String::new(),
            run_number: number,
            workflow_id: 7,
            event: "push".into(),
            status: "completed".into(),
            conclusion: Some(conclusion.into()),
            html_url: format!("https://github.com/org/api/actions/runs/{id}"),
            created_at: end - Duration::seconds(secs),
            updated_at: end,
            run_started_at: Some(end - Duration::seconds(secs)),
            actor: None,
            repository: RunRepo { full_name: "org/api".into() },
        }
    }

    /// Newest first, the way GitHub hands them over.
    fn view(durations: &[i64]) -> TimingView {
        let runs: Vec<Run> = durations
            .iter()
            .rev()
            .enumerate()
            .map(|(i, &s)| {
                let n = durations.len() - i;
                run(n as u64, n as u64, s, i as i64, "success")
            })
            .collect();
        let mut v = TimingView::new("org/api".into(), 7, "CI".into());
        v.set_history(points_of(&runs));
        v
    }

    #[test]
    fn only_finished_successes_are_plotted() {
        let mut runs = vec![run(1, 1, 60, 5, "success"), run(2, 2, 30, 4, "failure")];
        runs.push(run(3, 3, 90, 3, "cancelled"));
        let mut pending = run(4, 4, 10, 2, "success");
        pending.run_number = 0; // an optimistic dispatch placeholder
        runs.push(pending);

        let mut v = TimingView::new("org/api".into(), 7, "CI".into());
        v.set_history(points_of(&runs));
        assert_eq!(v.points.len(), 1);
        assert_eq!(v.points[0].secs, 60);
        assert_eq!(v.unknown, 0);
    }

    #[test]
    fn a_run_github_has_housekept_is_counted_but_not_plotted() {
        // Finished in six minutes; GitHub then touched it when its logs expired,
        // four hundred days on. Read naively that is a 9600-hour run.
        let mut old = run(1, 1, 360, 60, "success");
        old.updated_at = old.started_at() + Duration::days(400) + Duration::seconds(360);
        let runs = vec![run(2, 2, 90, 5, "success"), old];

        let mut v = TimingView::new("org/api".into(), 7, "CI".into());
        v.set_history(points_of(&runs));
        assert_eq!(v.points.len(), 1, "the housekept run is left off");
        assert_eq!(v.points[0].secs, 90);
        assert_eq!(v.unknown, 1, "but it is counted, so the chart can say so");
    }

    #[test]
    fn runs_are_ordered_oldest_first_with_the_cursor_on_the_newest() {
        let v = view(&[10, 20, 30]);
        let numbers: Vec<u64> = v.points.iter().map(|p| p.number).collect();
        assert_eq!(numbers, vec![1, 2, 3]);
        assert_eq!(v.selected().unwrap().number, 3);
    }

    #[test]
    fn stats_report_the_spread() {
        let s = view(&[60, 120, 180, 240]).stats().unwrap();
        assert_eq!(s.min, 60);
        assert_eq!(s.max, 240);
        assert_eq!(s.median, 150); // mean of the middle two
    }

    #[test]
    fn a_short_history_keeps_both_its_ends() {
        // Five runs is too few to call any of them an outlier.
        let s = view(&[60, 120, 180, 240, 300]).stats().unwrap();
        assert_eq!(s.floor, 60);
        assert_eq!(s.ceiling, 300);
    }

    #[test]
    fn one_slow_run_does_not_flatten_the_rest() {
        // Ninety-nine ordinary runs and one that hung.
        let mut durations = vec![120; 99];
        durations.push(3600);
        let s = view(&durations).stats().unwrap();
        assert_eq!(s.max, 3600);
        assert_eq!(s.ceiling, 120, "the axis follows the runs, not the outlier");
        assert_eq!(s.floor, 120);

        // A spread with no outlier keeps its own top.
        let s = view(&(60..=160).collect::<Vec<_>>()).stats().unwrap();
        assert_eq!(s.max, 160);
        assert_eq!(s.ceiling, 155);
        assert_eq!(s.floor, 65, "and the bottom follows the quickest, near enough");
    }

    #[test]
    fn a_short_history_has_no_trend_to_report() {
        assert!(view(&[60, 61, 62]).stats().unwrap().trend.is_none());
        assert!(view(&[]).stats().is_none());
    }

    #[test]
    fn the_trend_compares_the_last_runs_with_the_ones_before() {
        // Four at 100s, then four at 150s: half again as long.
        let v = view(&[100, 100, 100, 100, 150, 150, 150, 150]);
        assert_eq!(v.stats().unwrap().trend, Some(50));

        // And the other way round.
        let v = view(&[150, 150, 150, 150, 100, 100, 100, 100]);
        assert_eq!(v.stats().unwrap().trend, Some(-33));
    }

    #[test]
    fn the_smoothed_curve_sees_past_a_single_spike() {
        // A steady two minutes with one run that hung.
        let mut durations = vec![120; 20];
        durations[10] = 3600;
        let curve = view(&durations).smoothed();
        assert_eq!(curve.len(), 20);
        assert!(curve.iter().all(|&s| s == 120), "the spike never reaches the curve: {curve:?}");

        // A real shift does: the curve follows the runs up.
        let v = view(&[100, 100, 100, 100, 100, 100, 200, 200, 200, 200, 200, 200]);
        let curve = v.smoothed();
        assert_eq!(curve[0], 100);
        assert_eq!(curve[11], 200);
        assert!(curve.windows(2).all(|w| w[0] <= w[1]), "and never dips on the way: {curve:?}");
    }

    #[test]
    fn the_curve_exists_for_a_handful_of_runs_too() {
        assert_eq!(view(&[10, 30, 20]).smoothed(), vec![20, 20, 20]);
        assert!(view(&[]).smoothed().is_empty());
    }

    #[test]
    fn runs_widen_to_fill_the_chart() {
        // Twenty runs across eighty-four columns: four each, three of bar.
        let twenty: Vec<i64> = (1..=20).collect();
        let l = view(&twenty).layout(84);
        assert_eq!((l.step, l.bar_w), (4, 3));
        // Ten runs get twice that.
        let l = view(&twenty[..10]).layout(84);
        assert_eq!((l.step, l.bar_w), (8, 7));
        // Three runs stop at the cap rather than becoming three slabs.
        let l = view(&[10, 20, 30]).layout(400);
        assert_eq!((l.step, l.bar_w), (MAX_STEP, MAX_STEP - 1));
        // Two columns a run leaves no room for a gap wider than the bar.
        let l = view(&twenty).layout(40);
        assert_eq!((l.step, l.bar_w), (2, 1));
        // Twice as many runs as columns to put them in, and they close up.
        let l = view(&[10, 20, 30, 40, 50, 60]).layout(9);
        assert_eq!((l.step, l.bar_w), (1, 1));
    }

    #[test]
    fn runs_get_a_column_each_while_they_fit() {
        let v = view(&[10, 20, 30]);
        let l = v.layout(40);
        assert_eq!(l.bars.len(), 3);
        assert_eq!(l.bars.iter().map(|b| b.secs).collect::<Vec<_>>(), vec![10, 20, 30]);
        assert!(l.bars.iter().all(|b| b.runs == 1));

        // Just enough room for one column each, and no more.
        let l = v.layout(3);
        assert_eq!(l.bars.len(), 3);
        assert_eq!(l.step, 1);
    }

    #[test]
    fn a_long_history_folds_into_the_columns_there_are() {
        let durations: Vec<i64> = (1..=100).collect();
        let v = view(&durations);
        let l = v.layout(10);
        assert_eq!(l.bars.len(), 10);
        assert_eq!(l.step, 1);
        // Every run is covered exactly once, and each column reports its longest.
        assert_eq!(l.bars.iter().map(|b| b.runs).sum::<usize>(), 100);
        assert_eq!(l.bars[0].secs, 10);
        assert_eq!(l.bars[9].secs, 100);
        // Each column knows the middle of what it stands for.
        assert_eq!(l.bars[0].middle(), 5);
        assert_eq!(l.bars[9].middle(), 95);
    }

    #[test]
    fn an_empty_chart_lays_out_to_nothing() {
        let v = view(&[]);
        assert!(v.layout(40).bars.is_empty());
        assert!(view(&[10]).layout(0).bars.is_empty());
    }

    #[test]
    fn the_cursor_moves_by_column_not_by_run() {
        let durations: Vec<i64> = (1..=100).collect();
        let mut v = view(&durations);
        v.width = 10;
        assert_eq!(v.selected().unwrap().secs, 100);

        // One press steps a whole column back, not a single run.
        v.move_cursor(-1);
        assert_eq!(v.selected().unwrap().secs, 90);
        v.move_cursor(-2);
        assert_eq!(v.selected().unwrap().secs, 70);

        // And it stops at the ends rather than wrapping.
        v.move_cursor(-100);
        assert_eq!(v.selected().unwrap().secs, 10);
        v.move_cursor(-1);
        assert_eq!(v.selected().unwrap().secs, 10);
        v.move_cursor(500);
        assert_eq!(v.selected().unwrap().secs, 100);
    }

    #[test]
    fn a_cursor_folded_out_of_sight_still_finds_a_column() {
        // Column-per-run: the cursor sits on a run that is not any column's
        // peak once the chart narrows.
        let mut v = view(&[10, 90, 20, 80, 30, 70]);
        v.width = 6;
        v.cursor = 4; // the 30s run
        v.width = 2;
        let l = v.layout(2);
        assert!(v.bar_at(&l) < l.bars.len());
        v.move_cursor(0);
        assert!(v.selected().is_some());
    }

    #[test]
    fn a_chart_with_no_columns_is_still_answerable() {
        // A pane too narrow to plot anything still asks which bar the cursor is
        // on, so the answer has to exist.
        let v = view(&[10, 20, 30]);
        assert_eq!(v.bar_at(&v.layout(0)), 0);
        assert_eq!(view(&[]).bar_at(&v.layout(0)), 0);
    }

    #[test]
    fn the_newest_run_sits_against_the_right_edge() {
        // Three runs in forty columns: twelve each, eleven of bar, so the
        // first sits five in and the last ends on the final column.
        let v = view(&[10, 20, 30]);
        let l = v.layout(40);
        assert_eq!(l.offset(40), 5);
        assert_eq!(l.col(0, 40), 5);
        assert_eq!(l.col(1, 40), 17);
        assert_eq!(l.col(2, 40) + l.bar_w, 40);
        // Clicking finds the bar drawn there; the gap after a bar is its own.
        assert_eq!(l.bar_of(5, 40), 0);
        assert_eq!(l.bar_of(16, 40), 0);
        assert_eq!(l.bar_of(17, 40), 1);
        assert_eq!(l.bar_of(39, 40), 2);
        assert_eq!(l.bar_of(0, 40), 0);

        // A full chart starts at the left edge, with nothing to spare.
        let durations: Vec<i64> = (1..=100).collect();
        let full = view(&durations).layout(10);
        assert_eq!(full.offset(10), 0);
        assert_eq!(full.col(9, 10), 9);
    }

    #[test]
    fn clicking_a_column_selects_the_run_drawn_there() {
        let mut v = view(&[10, 20, 30]);
        v.width = 40;
        v.click_col(5);
        assert_eq!(v.selected().unwrap().secs, 10);
        v.click_col(39);
        assert_eq!(v.selected().unwrap().secs, 30);
        // A click past the last column stays on it.
        v.click_col(500);
        assert_eq!(v.selected().unwrap().secs, 30);
    }

    #[test]
    fn jumping_lands_on_the_ends() {
        let mut v = view(&[10, 20, 30]);
        v.jump(true);
        assert_eq!(v.selected().unwrap().number, 1);
        v.jump(false);
        assert_eq!(v.selected().unwrap().number, 3);
    }

    #[test]
    fn retargeting_clears_the_chart_but_keeps_the_workflow_list() {
        let mut v = view(&[10, 20]);
        v.workflows = vec![Workflow {
            id: 9,
            name: "Deploy".into(),
            path: ".github/workflows/deploy.yml".into(),
            state: "active".into(),
        }];
        v.retarget(9, "Deploy".into());
        assert_eq!(v.workflow_id, 9);
        assert!(v.points.is_empty());
        assert!(!v.loaded);
        assert_eq!(v.workflows.len(), 1);
    }
}
