//! Widths of the body's panes and of the runs table's columns, plus the divider
//! a mouse drag is moving. Everything here is remembered between sessions, so a
//! layout you set stays set.

use crate::state::State;
use ratatui::layout::Rect;
use std::collections::HashMap;

/// How wide the repos sidebar can be dragged, and what it starts at.
pub const MIN_SIDEBAR: u16 = 16;
pub const MAX_SIDEBAR: u16 = 60;
pub const DEFAULT_SIDEBAR: u16 = 28;

/// Likewise for the detail pane on the right.
pub const MIN_DETAIL: u16 = 26;
pub const MAX_DETAIL: u16 = 80;
pub const DEFAULT_DETAIL: u16 = 34;

/// What the runs table keeps for itself: the sidebar is dropped, and the detail
/// pane refuses to grow, rather than squeezing the runs below this.
pub const MIN_RUNS: u16 = 42;
/// Runs plus detail — below this there's no room for a sidebar at all.
pub const MIN_BODY_WIDTH: u16 = MIN_RUNS + MIN_DETAIL;

/// A resizable column of the runs table, in the order they're drawn. The status
/// glyph isn't here: it's one character wide either way.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Column {
    Repo,
    Workflow,
    Branch,
    Event,
    Actor,
    Dur,
    Age,
}

impl Column {
    pub const MIN: u16 = 4;
    pub const MAX: u16 = 48;

    /// Name in the state file.
    pub fn key(self) -> &'static str {
        match self {
            Column::Repo => "repo",
            Column::Workflow => "workflow",
            Column::Branch => "branch",
            Column::Event => "event",
            Column::Actor => "actor",
            Column::Dur => "dur",
            Column::Age => "age",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        [
            Column::Repo,
            Column::Workflow,
            Column::Branch,
            Column::Event,
            Column::Actor,
            Column::Dur,
            Column::Age,
        ]
        .into_iter()
        .find(|c| c.key() == key)
    }

    pub fn title(self) -> &'static str {
        match self {
            Column::Repo => "Repository",
            Column::Workflow => "Workflow",
            Column::Branch => "Branch",
            Column::Event => "Event",
            Column::Actor => "Actor",
            Column::Dur => "Dur",
            Column::Age => "Age",
        }
    }

    pub fn default_width(self) -> u16 {
        match self {
            Column::Repo => 24,
            Column::Workflow => 23,
            Column::Branch => 16,
            Column::Actor => 12,
            Column::Event | Column::Dur => 8,
            Column::Age => 6,
        }
    }
}

/// What a drag is moving.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Divider {
    /// Between the repos sidebar and the runs table.
    Sidebar,
    /// Between the runs table and the detail pane.
    Detail,
    /// A table column's right edge; carries where that column starts on screen.
    Column(Column, u16),
}

pub struct Panes {
    pub sidebar: u16,
    pub detail: u16,
    /// Only the columns you've actually resized; the rest use their default.
    columns: HashMap<Column, u16>,
    /// The divider a drag is moving, if any.
    pub dragging: Option<Divider>,
    /// Where each drawn column sits: (column, first x, last x). Recorded while
    /// drawing, so a drag on the header can find the separator it grabbed.
    edges: Vec<(Column, u16, u16)>,
    /// Screen row of the table's header.
    header_y: u16,
}

impl Panes {
    pub fn new(state: &State) -> Self {
        Self {
            sidebar: state.sidebar_width().unwrap_or(DEFAULT_SIDEBAR).clamp(MIN_SIDEBAR, MAX_SIDEBAR),
            detail: state.detail_width().unwrap_or(DEFAULT_DETAIL).clamp(MIN_DETAIL, MAX_DETAIL),
            columns: state
                .columns()
                .iter()
                .filter_map(|(k, w)| Some((Column::from_key(k)?, (*w).clamp(Column::MIN, Column::MAX))))
                .collect(),
            dragging: None,
            edges: Vec::new(),
            header_y: 0,
        }
    }

    /// Copy the current layout into the state, for saving.
    pub fn store(&self, state: &mut State) {
        state.set_sidebar_width(self.sidebar);
        state.set_detail_width(self.detail);
        state.set_columns(self.columns.iter().map(|(c, w)| (c.key().to_string(), *w)).collect());
    }

    /// Back to the widths actui ships with.
    pub fn reset(&mut self) {
        self.sidebar = DEFAULT_SIDEBAR;
        self.detail = DEFAULT_DETAIL;
        self.columns.clear();
    }

    pub fn column_width(&self, col: Column) -> u16 {
        self.columns.get(&col).copied().unwrap_or_else(|| col.default_width())
    }

    /// Resolve the drawn width of each column, shrinking the widest first when
    /// they don't all fit. `avail` is what's left after the marker, the status
    /// glyph, and the gap between every pair of columns.
    pub fn fit(&self, shown: &[Column], avail: u16) -> Vec<u16> {
        let mut widths: Vec<u16> = shown.iter().map(|c| self.column_width(*c)).collect();
        let gaps = shown.len().saturating_sub(1) as u16;
        let mut total: u16 = widths.iter().sum::<u16>() + gaps;
        while total > avail {
            let widest = widths
                .iter()
                .enumerate()
                .max_by_key(|(_, w)| **w)
                .map(|(i, _)| i);
            match widest {
                Some(i) if widths[i] > Column::MIN => {
                    widths[i] -= 1;
                    total -= 1;
                }
                _ => break,
            }
        }
        widths
    }

    /// Record where the drawn columns landed, for hit-testing a drag.
    pub fn record_columns(&mut self, first_x: u16, header_y: u16, drawn: &[(Column, u16)]) {
        self.header_y = header_y;
        self.edges.clear();
        let mut x = first_x;
        for (col, w) in drawn {
            self.edges.push((*col, x, x + w.saturating_sub(1)));
            x += w + 1; // the gap between columns
        }
    }

    /// The divider under a press, if it landed on one. Column separators sit on
    /// the table's header row; the pane dividers are the borders between panes.
    pub fn divider_at(&self, x: u16, y: u16, body: Rect, sidebar_shown: bool) -> Option<Divider> {
        if y == self.header_y {
            if let Some((col, start, _)) = self
                .edges
                .iter()
                .find(|(_, _, end)| x == *end || x == end + 1)
            {
                return Some(Divider::Column(*col, *start));
            }
        }
        if y < body.y || y >= body.bottom() {
            return None;
        }
        if sidebar_shown {
            let edge = body.x + self.sidebar;
            if x + 1 == edge || x == edge {
                return Some(Divider::Sidebar);
            }
        }
        let edge = body.right().saturating_sub(self.detail);
        if x + 1 == edge || x == edge {
            return Some(Divider::Detail);
        }
        None
    }

    /// Move whatever is being dragged to the pointer.
    pub fn drag_to(&mut self, x: u16, body: Rect, sidebar_shown: bool) {
        match self.dragging {
            Some(Divider::Sidebar) => self.sidebar = x.saturating_sub(body.x) + 1,
            Some(Divider::Detail) => self.detail = body.right().saturating_sub(x),
            Some(Divider::Column(col, start)) => {
                let width = x.saturating_sub(start) + 1;
                self.columns.insert(col, width.clamp(Column::MIN, Column::MAX));
            }
            None => return,
        }
        self.clamp(body.width, sidebar_shown);
    }

    /// Keep the pane widths inside their bounds and inside the terminal, so a
    /// drag — or a resize of the window — can't starve the runs table.
    pub fn clamp(&mut self, body_width: u16, sidebar_shown: bool) {
        let side = if sidebar_shown { self.sidebar } else { 0 };
        let detail_room = body_width.saturating_sub(side + MIN_RUNS);
        self.detail = self.detail.clamp(MIN_DETAIL, MAX_DETAIL.min(detail_room).max(MIN_DETAIL));

        let sidebar_room = body_width.saturating_sub(MIN_BODY_WIDTH);
        self.sidebar = self
            .sidebar
            .clamp(MIN_SIDEBAR, MAX_SIDEBAR.min(sidebar_room).max(MIN_SIDEBAR));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> Rect {
        Rect::new(0, 5, 120, 10)
    }

    #[test]
    fn columns_shrink_from_the_widest_until_they_fit() {
        let panes = Panes::new(&State::default());
        let shown = [Column::Repo, Column::Workflow, Column::Age];
        // 24 + 23 + 6 + 2 gaps = 55.
        assert_eq!(panes.fit(&shown, 55), vec![24, 23, 6]);
        // Squeezed by 5, taken off the widest columns, never off Age.
        let tight = panes.fit(&shown, 50);
        assert_eq!(tight.iter().sum::<u16>(), 48);
        assert_eq!(tight[2], 6);
        assert!(tight[0] >= 21 && tight[1] >= 21);
    }

    #[test]
    fn a_press_on_a_pane_border_finds_its_divider() {
        let mut panes = Panes::new(&State::default());
        let b = body();
        // The sidebar's right border and the runs pane's left border.
        assert_eq!(panes.divider_at(b.x + panes.sidebar, 8, b, true), Some(Divider::Sidebar));
        assert_eq!(panes.divider_at(b.x + panes.sidebar - 1, 8, b, true), Some(Divider::Sidebar));
        // Nothing there when the sidebar isn't drawn.
        assert_eq!(panes.divider_at(b.x + panes.sidebar, 8, b, false), None);
        // The detail pane's left border.
        let edge = b.right() - panes.detail;
        assert_eq!(panes.divider_at(edge, 8, b, true), Some(Divider::Detail));
        // Not on a row outside the body.
        assert_eq!(panes.divider_at(edge, 2, b, true), None);
        panes.dragging = Some(Divider::Detail);
        panes.drag_to(edge - 10, b, true);
        assert_eq!(panes.detail, DEFAULT_DETAIL + 10);
    }

    #[test]
    fn a_press_on_a_column_separator_finds_its_column() {
        let mut panes = Panes::new(&State::default());
        // Well clear of the pane borders, so only the column check can match.
        panes.record_columns(40, 6, &[(Column::Repo, 24), (Column::Workflow, 23)]);
        // Repo runs 40..=63, so its separator is at 63 and the gap at 64.
        assert_eq!(panes.divider_at(63, 6, body(), true), Some(Divider::Column(Column::Repo, 40)));
        assert_eq!(panes.divider_at(64, 6, body(), true), Some(Divider::Column(Column::Repo, 40)));
        // Only on the header row.
        assert_eq!(panes.divider_at(63, 7, body(), true), None);

        panes.dragging = Some(Divider::Column(Column::Repo, 40));
        panes.drag_to(55, body(), true);
        assert_eq!(panes.column_width(Column::Repo), 16);
        assert_eq!(panes.column_width(Column::Workflow), 23, "the others are untouched");
    }

    #[test]
    fn panes_never_squeeze_the_runs_table() {
        let mut panes = Panes::new(&State::default());
        let b = Rect::new(0, 5, 100, 10);
        panes.dragging = Some(Divider::Sidebar);
        panes.drag_to(90, b, true);
        assert!(panes.sidebar <= 100 - MIN_BODY_WIDTH);

        panes.dragging = Some(Divider::Detail);
        panes.drag_to(0, b, true);
        assert!(panes.sidebar + panes.detail + MIN_RUNS <= 100);
    }
}
