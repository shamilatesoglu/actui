//! All rendering. `draw` is called every frame with the current `App`. The
//! persistent layout (header, tabs, the runs/detail/jobs panes, logs, footer)
//! lives here; the floating modal overlays are in the `overlays` submodule.

mod overlays;
mod repos;
mod timing;

use crate::app::{
    is_error_line, log_content, App, Bar, Column, Divider, Filter, Focus, Mode, Panes, RunnerRow,
    RunnerStatus, Scrollable,
};
use crate::github::{Job, Run, RunState, RunTag, Step};
use ansi_to_tui::IntoText;
use chrono::{DateTime, Utc};
use overlays::*;
use repos::draw_repos;
use timing::draw_timing_pane;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Scrollbar,
    ScrollbarOrientation, ScrollbarState, Table, Tabs, Wrap,
};
use ratatui::Frame;
use std::sync::{OnceLock, RwLock};

/// Palette tints that must differ between light and dark backgrounds. Status
/// colors (red/green/yellow/cyan) are left to the terminal's own palette so
/// they already adapt; only these custom tints need a per-mode value. The
/// exception is `soft_red`, which says something the terminal's one red
/// can't: this is a guess at an error, not a run that really failed.
#[derive(Clone, Copy)]
pub struct Theme {
    accent: Color,
    dim: Color,
    bg_sel: Color,     // selection background (focused pane)
    bg_sel_dim: Color, // selection background (unfocused pane)
    pale: Color,       // unfocused selected text
    popup_bg: Color,   // popup window fill
    soft_red: Color,   // a guessed error, quieter than a real failure's red
    text: Color,       // the strongest foreground: what stands out from the accent
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            accent: Color::Rgb(137, 180, 250),
            dim: Color::Rgb(127, 132, 156),
            bg_sel: Color::Rgb(49, 50, 68),
            bg_sel_dim: Color::Rgb(40, 41, 56),
            pale: Color::Rgb(166, 173, 200),
            popup_bg: Color::Rgb(30, 31, 48),
            soft_red: Color::Rgb(235, 160, 172),
            text: Color::Rgb(205, 214, 244),
        }
    }
    pub fn light() -> Self {
        Self {
            accent: Color::Rgb(30, 102, 245),
            dim: Color::Rgb(108, 111, 133),
            bg_sel: Color::Rgb(188, 200, 240),
            bg_sel_dim: Color::Rgb(220, 224, 232),
            pale: Color::Rgb(76, 79, 105),
            popup_bg: Color::Rgb(230, 233, 239),
            soft_red: Color::Rgb(179, 80, 92),
            text: Color::Rgb(76, 79, 105),
        }
    }
}

fn theme_store() -> &'static RwLock<Theme> {
    static CURRENT: OnceLock<RwLock<Theme>> = OnceLock::new();
    CURRENT.get_or_init(|| RwLock::new(Theme::dark()))
}

/// Set the active palette (called from the main loop when the system theme,
/// or the configured override, resolves to light or dark).
pub fn set_theme(t: Theme) {
    *theme_store().write().unwrap() = t;
}

fn theme() -> Theme {
    *theme_store().read().unwrap()
}

fn accent() -> Color {
    theme().accent
}
fn dim() -> Color {
    theme().dim
}
fn bg_sel() -> Color {
    theme().bg_sel
}
fn bg_sel_dim() -> Color {
    theme().bg_sel_dim
}
fn pale() -> Color {
    theme().pale
}
fn popup_bg() -> Color {
    theme().popup_bg
}
fn soft_red() -> Color {
    theme().soft_red
}
fn text() -> Color {
    theme().text
}

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Row/list selection highlight: bright when focused, dim when not.
fn select_style(focused: bool) -> Style {
    if focused {
        Style::default().bg(bg_sel()).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(pale()).bg(bg_sel_dim())
    }
}

/// A pane's title: a highlighted (inverted) tab when focused, plain dim when not.
fn pane_title(title: &str, focused: bool) -> Span<'static> {
    if focused {
        Span::styled(
            format!(" {title} "),
            Style::default().fg(Color::Black).bg(accent()).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(format!(" {title} "), Style::default().fg(dim()).add_modifier(Modifier::BOLD))
    }
}

/// A bordered pane: rounded border (accent when focused, dim when not) with a
/// highlighted title tab. Returns the inner content Rect.
fn pane(f: &mut Frame, area: Rect, title: &str, focused: bool) -> Rect {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if focused { accent() } else { dim() }))
        .title(pane_title(title, focused));
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

/// A bordered popup window: rounded border + filled background so it stands out,
/// with a bold title. Returns the inner content Rect.
fn popup(f: &mut Frame, area: Rect, title: &str, accent: Color) -> Rect {
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent))
        .style(Style::default().bg(popup_bg()))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    inner
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4), // header: borders + brand row + breadcrumb/status row
            Constraint::Length(1), // tabs
            Constraint::Min(3),    // body
            Constraint::Length(1), // footer
        ])
        .split(f.area());

    app.hit.tabs = chunks[1];
    app.hit.runners_tab = Rect::default();
    app.hit.timing_tab = Rect::default();
    // Cleared here so stale rects can't take clicks in the views that drop the
    // panes; draw_repos and draw_body set them again when they draw.
    app.hit.repos = Rect::default();
    app.hit.body = Rect::default();
    app.panes.record_dividers(&[]);
    app.panes.clear_scrollbars();

    draw_header(f, app, chunks[0]);
    draw_tabs(f, app, chunks[1]);
    draw_body(f, app, chunks[2]);
    draw_footer(f, app, chunks[3]);

    match app.mode {
        Mode::Help => draw_help(f),
        Mode::Dispatch => draw_dispatch(f, app),
        Mode::Confirm => draw_confirm(f, app),
        Mode::Errors => draw_errors(f, app),
        Mode::Artifacts => draw_artifacts(f, app),
        Mode::Approval => draw_approval(f, app),
        Mode::Annotations => draw_annotations(f, app),
        Mode::RefPicker => draw_ref_picker(f, app),
        _ => {}
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let (running, queued, failed, success) = app.counts();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent()));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let spin = if app.loading {
        format!(" {} ", SPINNER[app.spinner])
    } else {
        "   ".into()
    };

    let line1 = Line::from(vec![
        Span::styled("  actui ", Style::default().fg(accent()).add_modifier(Modifier::BOLD)),
        Span::styled(format!("@{}  ", app.user), Style::default().fg(dim())),
        Span::styled(spin, Style::default().fg(Color::Yellow)),
        chip("●", running, Color::Yellow),
        chip("○", queued, Color::Cyan),
        chip("●", failed, Color::Red),
        chip("●", success, Color::Green),
        Span::styled(format!("  {} runs", app.runs.len()), Style::default().fg(dim())),
    ]);

    let mut right = Vec::new();
    if let Some(secs) = app.paused_secs {
        right.push(Span::styled(
            format!("  rate-limited · resuming in {secs}s ", ),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    } else if app.loading && app.repos_total > 0 {
        right.push(Span::styled(
            format!("  scanning {}/{} repos ", app.repos_done, app.repos_total),
            Style::default().fg(Color::Yellow),
        ));
    }
    if !app.errors.is_empty() {
        right.push(Span::styled(
            format!(" ⚠ {} (E) ", app.errors.len()),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(rl) = &app.rate {
        let c = if rl.remaining < 200 { Color::Red } else { dim() };
        let pct = if rl.limit > 0 {
            (rl.remaining as f64 / rl.limit as f64 * 100.0).round() as u32
        } else {
            0
        };
        right.push(Span::styled(
            format!(" api {pct}% "),
            Style::default().fg(c),
        ));
    }
    if let Some(ts) = app.last_refresh {
        right.push(Span::styled(
            format!(" updated {} ", fmt_age(ts)),
            Style::default().fg(dim()),
        ));
    }
    let status = Line::from(right).alignment(Alignment::Right);
    let crumbs = Line::from(breadcrumb(app));

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(inner);
    f.render_widget(Paragraph::new(line1), split[0]);
    // Breadcrumb (left) and status (right) share the second header row.
    f.render_widget(Paragraph::new(crumbs), split[1]);
    f.render_widget(Paragraph::new(status), split[1]);
}

fn chip(icon: &str, n: usize, color: Color) -> Span<'static> {
    Span::styled(
        format!(" {icon} {n} "),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
}

/// `Runs › Jobs › Logs` with the current depth highlighted.
fn breadcrumb(app: &App) -> Vec<Span<'static>> {
    let logs = app.mode == Mode::Logs;
    let at_jobs = !logs && app.focus == Focus::Jobs;
    let at_runs = !logs && app.focus == Focus::Runs;
    let at_repos = !logs && app.focus == Focus::Repos;
    let crumb = |label: &'static str, active: bool, reached: bool| {
        let style = if active {
            Style::default().fg(accent()).add_modifier(Modifier::BOLD)
        } else if reached {
            Style::default().fg(Color::White)
        } else {
            Style::default().fg(dim())
        };
        Span::styled(label, style)
    };
    let sep = || Span::styled(" › ", Style::default().fg(dim()));
    let mut out = vec![Span::raw("  ")];
    if app.repos.shown {
        out.push(crumb("Repos", at_repos, true));
        out.push(sep());
    }
    out.extend([
        crumb("Runs", at_runs, true),
        sep(),
        crumb("Jobs", at_jobs, at_jobs || logs),
        sep(),
        crumb("Logs", logs, logs),
    ]);
    out
}

/// The tabs row: which runs you're filtering to on the left, and which screen
/// you're on over on the right.
fn draw_tabs(f: &mut Frame, app: &mut App, area: Rect) {
    let elsewhere = app.mode == Mode::Runners || app.mode == Mode::Timing;
    let titles: Vec<Line> = Filter::ALL
        .iter()
        .map(|filt| Line::from(format!(" {} ", filt.label())))
        .collect();
    let sel = Filter::ALL.iter().position(|x| *x == app.filter).unwrap_or(0);
    // The filter belongs to the runs list, so it stops shouting while another
    // screen is up.
    let selected = if elsewhere {
        Style::default().fg(dim()).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Black).bg(accent()).add_modifier(Modifier::BOLD)
    };
    let tabs = Tabs::new(titles)
        .select(sel)
        .style(Style::default().fg(dim()))
        .highlight_style(selected)
        .divider("");
    f.render_widget(tabs, area);

    // The screens pack in from the right, and only while they still clear the
    // filter labels — half a switcher written over "Success" helps nobody.
    let floor = area.x + Filter::ALL.iter().map(|f| f.label().chars().count() as u16 + 2).sum::<u16>();
    let mut right = area.right();
    app.hit.runners_tab =
        screen_tab(f, &mut right, floor, area, " ⚙ Runners  s ", app.mode == Mode::Runners);
    app.hit.timing_tab =
        screen_tab(f, &mut right, floor, area, " ◷ Duration  w ", app.mode == Mode::Timing);
}

/// Draw one screen switcher ending at `right`, moving `right` left past it.
/// Hands back where it landed (empty when there wasn't room) so clicks resolve.
fn screen_tab(
    f: &mut Frame,
    right: &mut u16,
    floor: u16,
    area: Rect,
    label: &str,
    active: bool,
) -> Rect {
    let width = label.chars().count() as u16;
    if right.saturating_sub(width) < floor {
        return Rect::default();
    }
    let at = Rect { x: *right - width, width, ..area };
    *right = at.x;
    let style = if active {
        Style::default().fg(Color::Black).bg(accent()).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(dim())
    };
    f.render_widget(Paragraph::new(Span::styled(label.to_string(), style)), at);
    at
}

fn draw_body(f: &mut Frame, app: &mut App, area: Rect) {
    // The org-runners view keeps the sidebar it's reached from beside it.
    if app.mode == Mode::Runners {
        app.hit.body = area;
        let (rest, dividers) = draw_sidebar(f, app, area);
        draw_runners_pane(f, app, rest);
        app.panes.record_dividers(&dividers);
        return;
    }
    // So does the duration chart.
    if app.mode == Mode::Timing {
        app.hit.body = area;
        let (rest, dividers) = draw_sidebar(f, app, area);
        draw_timing_pane(f, app, rest);
        app.panes.record_dividers(&dividers);
        return;
    }
    // Live steps open as a third pane so the run detail + jobs list stay
    // visible — you keep your place in the jobs list while watching steps.
    if app.steps_pane_open() {
        app.hit.body = area;
        let (rest, mut dividers) = draw_sidebar(f, app, area);
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(20),
                Constraint::Length(app.panes.detail),
                Constraint::Length(app.panes.steps),
            ])
            .split(rest);
        draw_table(f, app, cols[0]);
        draw_detail(f, app, cols[1]);
        draw_steps_pane(f, app, cols[2]);
        dividers.push((Divider::Detail, cols[1].x, cols[1].right()));
        dividers.push((Divider::Steps, cols[2].x, cols[2].right()));
        app.panes.record_dividers(&dividers);
        return;
    }
    // Text logs take the full width: CI log lines are long, and the runs list
    // adds nothing while reading one job's output. Esc returns to the panes.
    if app.mode == Mode::Logs && app.logs.is_some() {
        draw_logs_pane(f, app, area);
        return;
    }
    // Recorded before the split: a divider drag measures against the whole body.
    app.hit.body = area;
    let (body, mut dividers) = draw_sidebar(f, app, area);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(20), Constraint::Length(app.panes.detail)])
        .split(body);
    draw_table(f, app, cols[0]);
    draw_detail(f, app, cols[1]);
    dividers.push((Divider::Detail, cols[1].x, cols[1].right()));
    app.panes.record_dividers(&dividers);
}

/// Draw the repos sidebar, when it's on screen, and hand back what's left of
/// the body for the other panes — plus the divider it put there.
fn draw_sidebar(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
) -> (Rect, Vec<(Divider, u16, u16)>) {
    if !app.repos.shown {
        return (area, Vec::new());
    }
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(app.panes.sidebar), Constraint::Min(20)])
        .split(area);
    draw_repos(f, app, cols[0]);
    (cols[1], vec![(Divider::Sidebar, cols[1].x, area.x)])
}

fn draw_table(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Runs;
    // The scope (if any) and a position indicator, so long lists stay orientable.
    let scoped = app.repos.scope().map(short_repo);
    let scope = scoped.as_ref().map(|s| format!(" · {s}")).unwrap_or_default();
    let title = if app.view.is_empty() {
        format!("Runs{scope}")
    } else {
        let at = app.table_state.selected().map_or(0, |i| i + 1);
        format!("Runs{scope} {at}/{}", app.view.len())
    };
    let content = pane(f, area, &title, focused);
    app.hit.runs = content;

    if app.view.is_empty() {
        let msg = if app.loading {
            "Loading workflow runs…".to_string()
        } else if !app.search.is_empty() {
            "No runs match your search.".to_string()
        } else if let Some(repo) = &scoped {
            format!("No recent runs in {repo}.\nPress d to dispatch a workflow there.")
        } else {
            "No runs found for this filter.".to_string()
        };
        let p = Paragraph::new(msg)
            .style(Style::default().fg(dim()))
            .alignment(Alignment::Center);
        f.render_widget(p, content);
        return;
    }

    // Adaptive columns: drop the lower-value ones as the pane narrows so the
    // essentials (status, repo, workflow, age) are never pushed off-screen. A
    // repo scope makes the repository column redundant, and the rest of the
    // table gets the width it was taking.
    let scoped = scoped.is_some();
    let budget = content.width + if scoped { Column::Repo.default_width() } else { 0 };
    let wide = budget >= 100; // event + actor
    let medium = budget >= 72; // duration
    let branch = budget >= 55; // branch, once the essentials are covered
    // The tag column earns its place only when there's width to spare and
    // something on the list actually tagged — otherwise it's a blank stripe.
    let tagged = budget >= 66 && app.any_tag_shown();

    let mut shown = Vec::new();
    if !scoped {
        shown.push(Column::Repo);
    }
    shown.push(Column::Workflow);
    if tagged {
        shown.push(Column::Tag);
    }
    if branch {
        shown.push(Column::Branch);
    }
    if wide {
        shown.push(Column::Event);
        shown.push(Column::Actor);
    }
    if medium {
        shown.push(Column::Dur);
    }
    shown.push(Column::Age);

    // Each column is as wide as you dragged it (or its default), and the
    // leftover goes to one trailing spacer — so spare width shows up as a
    // margin on the right instead of a hole in the middle of a row.
    // LEAD, plus the gap before the trailing spacer column.
    let avail = content.width.saturating_sub(LEAD + 1);
    let drawn = app.panes.fit(&shown, avail);

    let mut head = vec![Cell::from("")];
    let mut widths = vec![Constraint::Length(2)];
    for (col, w) in shown.iter().zip(&drawn) {
        head.push(Cell::from(col.title()));
        widths.push(Constraint::Length(*w));
    }
    head.push(Cell::from(""));
    widths.push(Constraint::Min(0));
    let header = Row::new(head).style(Style::default().fg(accent()).add_modifier(Modifier::BOLD));

    let spin = SPINNER[app.spinner];
    // Collected (owned), so the table can render against the real
    // `table_state` — keeping its scroll offset is what lets mouse clicks
    // map back to rows.
    let rows: Vec<Row> = app.view.iter().map(|&i| {
        let r = &app.runs[i];
        let (icon, color) = state_glyph(r.state());
        let mut cells = vec![Cell::from(Span::styled(icon, Style::default().fg(color)))];
        let tag = app.tag_of(r.id);
        cells.extend(
            shown.iter().zip(&drawn).map(|(col, w)| cell(r, tag, *col, *w as usize, spin)),
        );
        cells.push(Cell::from("")); // the trailing spacer
        Row::new(cells)
    }).collect();

    // Where the columns landed, so a drag on the header finds its separator.
    let placed: Vec<(Column, u16)> = shown.iter().copied().zip(drawn).collect();
    app.panes.record_columns(content.x + LEAD, content.y, &placed);

    // Both panes show their selection; the unfocused one dims it (lazyactions).
    let hl = select_style(focused);
    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(hl)
        .highlight_symbol(if focused { "▌" } else { " " });

    f.render_stateful_widget(table, content, &mut app.table_state);

    // Scroll position feedback once the list outgrows the viewport. Rendered
    // after the table so the offset reflects this frame.
    let viewport = content.height.saturating_sub(1) as usize; // header row
    scrollbar(
        f,
        &mut app.panes,
        Scrollable::Runs,
        area.inner(Margin { vertical: 1, horizontal: 0 }),
        app.view.len(),
        viewport,
        app.table_state.offset(),
    );
}

fn draw_detail(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Jobs;
    let inner = pane(f, area, "Detail", focused);

    let Some(run) = app.selected_run() else {
        f.render_widget(
            Paragraph::new("Select a run to see details.").style(Style::default().fg(dim())),
            inner,
        );
        return;
    };

    // The tag row earns its own line rather than crowding out the run info.
    let tag = app.selected_tag().cloned();
    let info_h = if tag.is_some() { 10 } else { 9 };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(info_h), Constraint::Min(3)])
        .split(inner);

    let (icon, color) = state_glyph(run.state());
    // Held-for-approval runs read as "queued"; call it out so `a` makes sense.
    // A dispatch we're still waiting on says so rather than claiming progress.
    let (label, label_color) = if run.is_pending() {
        ("dispatching", Color::Yellow)
    } else if run.needs_approval() {
        ("awaiting approval", Color::Yellow)
    } else {
        (state_label(run.state()), color)
    };
    let number = if run.is_pending() {
        format!("  {}", SPINNER[app.spinner])
    } else {
        format!("  #{}", run.run_number)
    };
    let mut first = vec![
        Span::styled(format!("{icon} "), Style::default().fg(color).add_modifier(Modifier::BOLD)),
        Span::styled(label, Style::default().fg(label_color).add_modifier(Modifier::BOLD)),
        Span::styled(number, Style::default().fg(dim())),
    ];
    if run.needs_approval() {
        first.push(Span::styled("  · press a", Style::default().fg(dim())));
    }
    let mut info = vec![Line::from(first)];
    // Right under the state line: this is what the run produced, and up here it
    // survives a short pane instead of being the first row cut.
    if let Some(t) = &tag {
        info.push(tag_line(t, rows[0].width));
    }
    info.extend([
        kv("repo", &run.repository.full_name),
        kv("flow", run.workflow_name()),
        kv("title", run.title()),
        kv("branch", run.head_branch.as_deref().unwrap_or("-")),
        kv("event", &run.event),
        kv("actor", run.actor.as_ref().map(|a| a.login.as_str()).unwrap_or("-")),
        kv("started", &fmt_dt(run.run_started_at.unwrap_or(run.created_at))),
    ]);
    info.truncate(rows[0].height as usize);
    // `trim: false`: these rows are right-aligned labels, and trimming would
    // strip the very padding that lines them up.
    f.render_widget(Paragraph::new(info).wrap(Wrap { trim: false }), rows[0]);

    draw_jobs(f, app, rows[1]);
}

fn draw_jobs(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Jobs;
    // Divider rule between the run info and the jobs list within the Detail pane.
    let title = if focused { " Jobs · ⏎ logs " } else { " Jobs " };
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(if focused { accent() } else { dim() }))
        .title(pane_title(title.trim(), focused));
    let inner = block.inner(area);
    f.render_widget(block, area);
    app.hit.jobs = inner;

    if app.jobs.is_empty() {
        // Distinguish "the run truly has no jobs" from "still fetching".
        let loaded = app.jobs_run_id.is_some()
            && app.jobs_run_id == app.selected_run().map(|r| r.id);
        let msg = if loaded { "No jobs for this run." } else { "Loading jobs…" };
        f.render_widget(Paragraph::new(msg).style(Style::default().fg(dim())), inner);
        return;
    }

    let jobs_len = app.jobs.len();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(jobs_len.clamp(3, 6) as u16),
            Constraint::Min(3),
        ])
        .split(inner);

    let items: Vec<ListItem> = app
        .jobs
        .iter()
        .map(|j| {
            let (icon, color) = status_glyph(&j.status, j.conclusion.as_deref());
            ListItem::new(Line::from(vec![
                Span::styled(format!("{icon} "), Style::default().fg(color)),
                Span::raw(truncate(&j.name, 28)),
                Span::styled(format!("  {}", job_dur(j)), Style::default().fg(dim())),
            ]))
        })
        .collect();

    let list = List::new(items)
        .highlight_style(select_style(focused))
        .highlight_symbol(if focused { "▌" } else { " " });
    f.render_stateful_widget(list, chunks[0], &mut app.jobs_state);

    // Title + body track the selected job: a log with error-looking lines is
    // titled "Possible Errors · N" and lists every spot one appears. The count
    // is a guess from the text, so the title says so — `v` is the pane that
    // knows, off GitHub's own annotations.
    let mut title = " Log Preview ".to_string();
    let mut title_style = Style::default().fg(dim());
    let mut body: Vec<Line> = Vec::new();
    let mut body_msg: Option<&str> = None;

    if let Some(j) = app.selected_job() {
        if j.is_running() {
            body_msg = Some("Job is running (live steps are active).");
        } else if let Some(text) = app.logs_cache.get(&j.id) {
            let (lines, errors) = get_error_preview_lines(text, j.conclusion.as_deref());
            if errors > 0 {
                title = format!(" Possible Errors · {errors} ");
                title_style = Style::default().fg(soft_red()).add_modifier(Modifier::BOLD);
            }
            body = lines;
        } else {
            body_msg = Some("Loading preview…");
        }
    }

    let preview_block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(dim()))
        .title(Span::styled(title, title_style));
    let preview_inner = preview_block.inner(chunks[1]);
    f.render_widget(preview_block, chunks[1]);

    if let Some(msg) = body_msg {
        f.render_widget(
            Paragraph::new(msg).style(Style::default().fg(dim())).wrap(Wrap { trim: true }),
            preview_inner,
        );
    } else {
        f.render_widget(Paragraph::new(body).wrap(Wrap { trim: true }), preview_inner);
    }
}

/// Draw a scrollbar for a list that has outgrown its pane, and record where it
/// landed so a press on it can drive the list. Nothing is drawn — and nothing
/// recorded — while everything fits.
fn scrollbar(
    f: &mut Frame,
    panes: &mut Panes,
    target: Scrollable,
    area: Rect,
    len: usize,
    viewport: usize,
    position: usize,
) {
    if len <= viewport || area.height < 3 {
        return;
    }
    // The widget counts scroll positions, not rows: given the row count its
    // thumb stops short of the end of the track, and a drag can't reach the
    // bottom of the list.
    let mut state = ScrollbarState::new(len - viewport + 1)
        .viewport_content_length(viewport)
        .position(position);
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight),
        area,
        &mut state,
    );
    panes.record_scrollbar(Bar { target, area, len, viewport, position });
}

/// One preview line: its number in the log, dim, then the highlighted text.
fn numbered_log_line(index: usize, line: &str) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("{:>4} │ ", index + 1),
        Style::default().fg(dim()),
    )];
    spans.extend(highlight_log(line).spans);
    Line::from(spans)
}

/// Build the job-log preview lines, returning them alongside the count of
/// error/fail lines found (0 → a clean log, so the caller keeps the plain
/// "Log Preview" title).
fn get_error_preview_lines(log_text: &str, conclusion: Option<&str>) -> (Vec<Line<'static>>, usize) {
    let lines: Vec<&str> = log_text.lines().collect();
    let n = lines.len();
    let mut should_include = vec![false; n];
    let mut error_count = 0usize;

    for (idx, line) in lines.iter().enumerate() {
        if is_error_line(log_content(line)) {
            error_count += 1;
            // The error line, plus three lines of context either side.
            should_include[idx.saturating_sub(3)..(idx + 4).min(n)].fill(true);
        }
    }

    if error_count == 0 {
        if matches!(conclusion, Some("failure") | Some("timed_out")) {
            let start = n.saturating_sub(15);
            let mut preview = vec![Line::from(vec![
                Span::styled("Nothing here looks like an error. Showing the end of the log:", Style::default().fg(dim()))
            ])];
            for (i, line) in lines.iter().enumerate().skip(start) {
                preview.push(numbered_log_line(i, line));
            }
            return (preview, 0);
        } else {
            return (
                vec![Line::from(vec![Span::styled(
                    "Nothing in the log looks like an error.",
                    Style::default().fg(dim()),
                )])],
                0,
            );
        }
    }

    let mut preview = Vec::new();
    let mut in_gap = false;

    for (i, keep) in should_include.iter().enumerate() {
        if *keep {
            if in_gap {
                preview.push(Line::from(vec![
                    Span::styled("  ...", Style::default().fg(dim()))
                ]));
                in_gap = false;
            }
            preview.push(numbered_log_line(i, lines[i]));
        } else if !preview.is_empty() {
            in_gap = true;
        }
    }

    (preview, error_count)
}

/// The org self-hosted runners view: a dedicated, full-width body pane with a
/// selectable list of runners grouped by org, and a tally on the last row.
fn draw_runners_pane(f: &mut Frame, app: &mut App, area: Rect) {
    let inner = pane(f, area, "Org runners", true);
    let Some(rv) = &app.runners else {
        app.hit.runners_pane = inner;
        return;
    };

    // Reserve the last row for the status tally.
    let body = Rect { height: inner.height.saturating_sub(1), ..inner };

    if !rv.loaded {
        app.hit.runners_pane = body;
        f.render_widget(
            Paragraph::new("Discovering organizations & runners…").style(Style::default().fg(dim())),
            body,
        );
        return;
    }
    if rv.rows.is_empty() {
        app.hit.runners_pane = body;
        f.render_widget(
            Paragraph::new(
                "No organizations found.\n\nYou're not a member of an org, or the token can't \
                 read your org memberships.",
            )
            .style(Style::default().fg(dim()))
            .wrap(Wrap { trim: true }),
            body,
        );
        return;
    }

    // With the detail pane open, split the body: list on the left, the selected
    // runner's details on the right.
    let detail_open = rv.detail_open && rv.selected_runner().is_some();
    let (list_area, detail_area) = if detail_open {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(body);
        (cols[0], Some(cols[1]))
    } else {
        (body, None)
    };
    app.hit.runners_pane = list_area;

    let items: Vec<ListItem> = rv.rows.iter().map(runner_row_item).collect();
    let list = List::new(items)
        .highlight_style(select_style(true))
        .highlight_symbol("▌");
    let mut state = rv.state.clone();
    f.render_stateful_widget(list, list_area, &mut state);

    let (rows, offset) = (rv.rows.len(), state.offset());
    scrollbar(
        f,
        &mut app.panes,
        Scrollable::Runners,
        list_area,
        rows,
        list_area.height as usize,
        offset,
    );

    if let Some(da) = detail_area {
        draw_runner_detail(f, rv, da);
    }

    let by = Rect { x: inner.x, y: inner.y + inner.height - 1, width: inner.width, height: 1 };
    let (online, offline, busy) = rv.totals();
    let mut tally = vec![
        Span::raw(" "),
        Span::styled(format!("{online} online  "), Style::default().fg(Color::Green)),
    ];
    if busy > 0 {
        tally.push(Span::styled(format!("{busy} busy  "), Style::default().fg(Color::Yellow)));
    }
    tally.push(Span::styled(format!("{offline} offline  "), Style::default().fg(dim())));
    tally.push(Span::styled(
        "· j/k move · ⏎ details · o open · r refresh · Esc back ",
        Style::default().fg(dim()),
    ));
    f.render_widget(Paragraph::new(Line::from(tally)).alignment(Alignment::Right), by);
}

/// Detail side pane for the selected runner: its identity, live status, OS, and
/// the full label set (which the list row truncates).
fn draw_runner_detail(f: &mut Frame, rv: &crate::app::RunnersView, area: Rect) {
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(dim()))
        .title(pane_title("Runner", true));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(RunnerRow::Runner { name, status, os, labels }) = rv.selected_runner() else {
        return;
    };
    let (glyph, gcolor, state_text, scolor) = match status {
        RunnerStatus::Busy => ("●", Color::Yellow, "online · busy", Color::Yellow),
        RunnerStatus::Online => ("●", Color::Green, "online · idle", Color::Green),
        RunnerStatus::Offline => ("○", dim(), "offline", dim()),
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("{glyph} "), Style::default().fg(gcolor).add_modifier(Modifier::BOLD)),
            Span::styled(name.clone(), Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::raw(""),
        kv("org", rv.selected_org().unwrap_or("-")),
        Line::from(vec![
            Span::styled(format!("{:>LABEL_W$}  ", "status"), Style::default().fg(dim())),
            Span::styled(state_text, Style::default().fg(scolor)),
        ]),
        kv("os", if os.is_empty() { "-" } else { os }),
        Line::raw(""),
        Line::from(Span::styled(
            format!("  labels ({})", labels.len()),
            Style::default().fg(dim()),
        )),
    ];
    if labels.is_empty() {
        lines.push(Line::from(Span::styled("  (none)", Style::default().fg(dim()))));
    } else {
        for l in labels {
            lines.push(Line::from(vec![
                Span::styled("  • ", Style::default().fg(accent())),
                Span::raw(l.clone()),
            ]));
        }
    }
    // Right-aligned labels again — trimming would undo the alignment.
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn runner_row_item(row: &RunnerRow) -> ListItem<'static> {
    match row {
        RunnerRow::Note(text) => {
            ListItem::new(Line::from(Span::styled(text.clone(), Style::default().fg(Color::Yellow))))
        }
        RunnerRow::Header { org, detail, detail_err } => {
            let acc = Style::default().fg(accent()).add_modifier(Modifier::BOLD);
            let dc = if *detail_err { Color::Red } else { dim() };
            ListItem::new(Line::from(vec![
                Span::styled("▸ ", acc),
                Span::styled(org.clone(), acc),
                Span::styled(format!("  {}", truncate(detail, 60)), Style::default().fg(dc)),
            ]))
        }
        RunnerRow::Runner { name, status, os, labels } => {
            let (icon, color, label, label_c) = match status {
                RunnerStatus::Busy => ("●", Color::Yellow, "busy", Color::Yellow),
                RunnerStatus::Online => ("●", Color::Green, "idle", Color::Green),
                RunnerStatus::Offline => ("○", dim(), "offline", dim()),
            };
            let mut spans = vec![
                Span::raw("  "),
                Span::styled(format!("{icon} "), Style::default().fg(color)),
                Span::raw(truncate(name, 28)),
                Span::styled(format!("  {label}"), Style::default().fg(label_c)),
            ];
            if !os.is_empty() {
                spans.push(Span::styled(format!("  {os}"), Style::default().fg(dim())));
            }
            if !labels.is_empty() {
                spans.push(Span::styled(
                    format!("  [{}]", labels.join(", ")),
                    Style::default().fg(dim()),
                ));
            }
            ListItem::new(Line::from(spans))
        }
    }
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    // The runs search prompt always wins while you're typing in it.
    if app.mode == Mode::Search {
        let line = Line::from(vec![
            Span::styled(" /", Style::default().fg(accent()).add_modifier(Modifier::BOLD)),
            Span::raw(app.search.clone()),
            Span::styled("▏", Style::default().fg(accent())),
            Span::styled("  (Esc clear · Enter keep)", Style::default().fg(dim())),
        ]);
        f.render_widget(Paragraph::new(line), area);
        return;
    }
    // A transient status notice (cancel/dispatch/error/etc.), if not yet expired.
    if let Some((msg, is_err)) = app.status() {
        let style = if is_err {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Green)
        };
        f.render_widget(Paragraph::new(Span::styled(format!(" {msg}"), style)), area);
        return;
    }
    // Hints in priority order, and the ones to keep whatever the width.
    let (mut hints, keep): (Vec<String>, Vec<&str>) = if app.mode == Mode::Runners {
        (
            owned(&["org self-hosted runners", "j/k move", "⏎ details", "o open on GitHub", "r refresh"]),
            vec!["Esc back"],
        )
    } else if app.mode == Mode::Timing {
        (
            owned(&[
                "successful run durations",
                "h/l move",
                "⏎ go to run",
                "o open on GitHub",
                "[ ] workflow",
                "r refresh",
            ]),
            vec!["Esc back"],
        )
    } else if app.steps_pane_open() {
        (
            owned(&["live steps", "updates automatically", "⏎ try logs", "j/k move"]),
            vec!["Esc close"],
        )
    } else if app.mode == Mode::Logs {
        let errors = if app.logs.as_ref().is_some_and(|lv| lv.preview_only) { " [errors]" } else { "" };
        (
            vec![
                "j/k move".into(),
                "←/→ scroll".into(),
                "⏎ fold".into(),
                "e/f all".into(),
                format!("p preview{errors}"),
                "/ search".into(),
                "n/N".into(),
                "s save".into(),
            ],
            vec!["Esc close"],
        )
    } else {
        match app.focus {
            Focus::Runs => {
                let mut hints = owned(&["j/k move", "⏎/l jobs", "/ search", "o open", "d dispatch"]);
                // Only advertise `a approve` when the selected run is actually held.
                if app.selected_run().is_some_and(|r| r.needs_approval()) {
                    hints.push("a approve".into());
                }
                // Offer `v failures` once the selected run has failed.
                if app.selected_run().is_some_and(|r| r.state() == RunState::Failure) {
                    hints.push("v failures".into());
                }
                // And `t` only once the run has actually tagged something —
                // named for what it opens.
                if let Some(t) = app.selected_tag() {
                    hints.push(format!("t {}", t.label()));
                }
                hints.extend(owned(&[
                    "c cancel",
                    "x/X rerun",
                    "A artifacts",
                    "s runners",
                    "w durations",
                    "p repos",
                ]));
                (hints, vec!["? help", "q quit"])
            }
            Focus::Jobs => (
                owned(&[
                    "j/k job",
                    "⏎/l logs",
                    "R rerun job",
                    "v failures",
                    "A artifacts",
                    "s runners",
                    "←/Esc back",
                    "o open",
                ]),
                vec!["? help", "q quit"],
            ),
            Focus::Repos => (
                owned(&[
                    "j/k repo",
                    "⏎/→ runs",
                    "d dispatch here",
                    "< > resize",
                    "= reset",
                    "Esc all repos",
                    "p hide",
                ]),
                vec!["? help", "q quit"],
            ),
        }
    };
    // A kept search filter stays visible (and dismissable) while it's active.
    if app.mode != Mode::Logs && !app.search.is_empty() {
        hints.insert(0, format!("/{} · Esc clear", app.search));
    }
    let line = hint_line(&hints, &keep, area.width as usize);
    f.render_widget(
        Paragraph::new(Span::styled(line, Style::default().fg(dim()))),
        area,
    );
}

fn owned(hints: &[&str]) -> Vec<String> {
    hints.iter().map(|h| (*h).to_string()).collect()
}

/// Fit as many hints as the footer has room for, most important first, so a
/// narrow terminal drops the ones it can't hold instead of cutting a key in
/// half. `keep` always makes it: those are how you find everything else.
fn hint_line(hints: &[String], keep: &[&str], width: usize) -> String {
    const SEP: &str = " · ";
    let tail = keep.join(SEP);
    // A leading space, and the separator before the tail.
    let room = width.saturating_sub(tail.chars().count() + if tail.is_empty() { 1 } else { 4 });

    let mut line = String::new();
    for hint in hints {
        let sep = if line.is_empty() { 0 } else { SEP.chars().count() };
        if line.chars().count() + sep + hint.chars().count() > room {
            break;
        }
        if !line.is_empty() {
            line.push_str(SEP);
        }
        line.push_str(hint);
    }
    if !tail.is_empty() {
        if !line.is_empty() {
            line.push_str(SEP);
        }
        line.push_str(&tail);
    }
    format!(" {line}")
}

// -- overlays ---------------------------------------------------------------

fn draw_logs_pane(f: &mut Frame, app: &mut App, area: Rect) {
    let title = match &app.logs {
        Some(lv) => {
            format!("Logs · {}", truncate(&lv.title, area.width.saturating_sub(10) as usize))
        }
        None => return,
    };
    let inner = pane(f, area, &title, true);

    // Reserve the last row for a status indicator.
    let body = Rect { height: inner.height.saturating_sub(1), ..inner };
    app.hit.logs_h = body.height;
    let lv = app.logs.as_ref().unwrap();
    let height = body.height as usize;
    let shown = lv.visible.len();

    // Center the cursor in the viewport (pure function of cursor + sizes).
    let scroll = center_scroll(lv.cursor, height, shown);

    let text: Vec<Line> = lv
        .visible
        .iter()
        .enumerate()
        .skip(scroll)
        .take(height)
        .map(|(row, &src)| {
            if src == usize::MAX {
                let gutter = if row == lv.cursor {
                    Span::styled("▌", Style::default().fg(accent()))
                } else {
                    Span::raw(" ")
                };
                return Line::from(vec![
                    gutter,
                    Span::raw(" "),
                    Span::styled("  ...", Style::default().fg(dim())),
                ]);
            }
            let in_group = lv.line_group[src].is_some();
            let line = if lv.is_header[src] {
                // Step node: ▸/▾ arrow, name, line count + duration.
                let g = lv.line_group[src].unwrap();
                let collapsed = lv.groups[g].collapsed;
                let arrow = if collapsed { "▸" } else { "▾" };
                let name = log_content(&lv.lines[src])
                    .strip_prefix("##[group]")
                    .unwrap_or("")
                    .to_string();
                let acc = Style::default().fg(accent()).add_modifier(Modifier::BOLD);
                let mut spans = vec![
                    Span::styled(format!("{arrow} "), acc),
                    Span::styled(name, acc),
                ];
                let mut meta = String::new();
                if collapsed && lv.groups[g].body_count > 0 {
                    meta.push_str(&format!("  {} lines", lv.groups[g].body_count));
                }
                if let Some(secs) = lv.groups[g].secs {
                    meta.push_str(&format!("  {}", fmt_secs(secs)));
                }
                if !meta.is_empty() {
                    spans.push(Span::styled(meta, Style::default().fg(dim())));
                }
                Line::from(spans)
            } else if !lv.search.is_empty() && lv.is_match(src) {
                highlight_match(&lv.lines[src], &lv.search)
            } else {
                highlight_log(&lv.lines[src])
            };
            // Cursor gutter + tree connector for lines inside a step.
            let gutter = if row == lv.cursor {
                Span::styled("▌", Style::default().fg(accent()))
            } else {
                Span::raw(" ")
            };
            let mut spans = vec![gutter, Span::raw(" ")];
            if in_group && !lv.is_header[src] {
                spans.push(Span::styled("│ ", Style::default().fg(dim())));
            }
            spans.extend(line.spans);
            Line::from(spans)
        })
        .collect();
    // Horizontal scroll for lines wider than the pane (Left/Right adjust it).
    f.render_widget(Paragraph::new(text).scroll((0, lv.hscroll)), body);

    scrollbar(
        f,
        &mut app.panes,
        Scrollable::Logs,
        area.inner(Margin { vertical: 1, horizontal: 0 }),
        shown,
        height,
        scroll,
    );

    let by = Rect { x: inner.x, y: inner.y + inner.height - 1, width: inner.width, height: 1 };
    if lv.searching {
        // Live search prompt.
        let line = Line::from(vec![
            Span::styled(" /", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw(lv.search.clone()),
            Span::styled("▏", Style::default().fg(Color::Yellow)),
            Span::styled(
                format!("  {} matches  (Enter keep · Esc cancel)", lv.matches.len()),
                Style::default().fg(dim()),
            ),
        ]);
        f.render_widget(Paragraph::new(line), by);
    } else {
        let pos = if shown == 0 { 0 } else { lv.cursor + 1 };
        let search = if !lv.search.is_empty() {
            let m = lv.match_idx.map(|i| i + 1).unwrap_or(0);
            format!(" · match {m}/{} (n/N)", lv.matches.len())
        } else {
            String::new()
        };
        let folds = if lv.has_groups() && !lv.preview_only { " · ⏎ fold · e/f all" } else { "" };
        let preview_mode = if lv.preview_only { " [errors preview]" } else { "" };
        let hs = if lv.hscroll > 0 { format!(" · →{}", lv.hscroll) } else { String::new() };
        let bar = format!(" {pos}/{shown}{preview_mode} · j/k{folds} · p preview · / search{search} · s save{hs} · Esc close ");
        f.render_widget(
            Paragraph::new(Span::styled(bar, Style::default().fg(dim()))).alignment(Alignment::Right),
            by,
        );
    }
}

/// Live step view for a running job (text logs aren't available yet).
fn draw_steps_pane(f: &mut Frame, app: &App, area: Rect) {
    let Some(sv) = &app.steps_view else { return };
    let steps = app.steps_view_steps();
    let title = format!("Live steps · {}", truncate(&sv.job_name, area.width.saturating_sub(14) as usize));
    let inner = pane(f, area, &title, true);

    let body = Rect { height: inner.height.saturating_sub(1), ..inner };
    if steps.is_empty() {
        f.render_widget(
            Paragraph::new("Waiting for the job to start…").style(Style::default().fg(dim())),
            body,
        );
    } else {
        let height = body.height as usize;
        let scroll = center_scroll(sv.cursor, height, steps.len());
        let lines: Vec<Line> = steps
            .iter()
            .enumerate()
            .skip(scroll)
            .take(height)
            .map(|(i, s)| {
                let (icon, color) = status_glyph(&s.status, s.conclusion.as_deref());
                let running = s.status == "in_progress";
                let name_style = if running {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                let gutter = if i == sv.cursor {
                    Span::styled("▌", Style::default().fg(accent()))
                } else {
                    Span::raw(" ")
                };
                Line::from(vec![
                    gutter,
                    Span::styled(format!(" {icon} "), Style::default().fg(color)),
                    Span::styled(s.name.clone(), name_style),
                    Span::styled(format!("  {}", step_dur(s)), Style::default().fg(dim())),
                ])
            })
            .collect();
        f.render_widget(Paragraph::new(lines), body);
    }

    let by = Rect { x: inner.x, y: inner.y + inner.height - 1, width: inner.width, height: 1 };
    let bar = " live · full logs load when the job finishes · j/k · c cancel run · Esc close ";
    f.render_widget(
        Paragraph::new(Span::styled(bar, Style::default().fg(dim()))).alignment(Alignment::Right),
        by,
    );
}

/// Status glyph shared by jobs and steps (both carry the same GitHub
/// `status` + `conclusion` model, so they render identically).
fn status_glyph(status: &str, conclusion: Option<&str>) -> (&'static str, Color) {
    match status {
        "in_progress" => ("●", Color::Yellow),
        "queued" | "waiting" | "pending" => ("○", Color::Cyan),
        "completed" => match conclusion {
            Some("success") => ("●", Color::Green),
            Some("failure") | Some("timed_out") => ("●", Color::Red),
            Some("cancelled") => ("◌", dim()),
            Some("skipped") => ("○", dim()),
            _ => ("·", dim()),
        },
        _ => ("·", dim()),
    }
}

fn step_dur(s: &Step) -> String {
    match (s.started_at, s.completed_at) {
        (Some(a), Some(b)) => fmt_dur((b - a).num_seconds().max(0)),
        (Some(a), None) => format!("{}…", fmt_dur((Utc::now() - a).num_seconds().max(0))),
        _ => String::new(),
    }
}

/// Render a matching log line with every occurrence of `query` highlighted.
fn highlight_match(raw: &str, query: &str) -> Line<'static> {
    let content = log_content(raw);
    // Byte offsets from the lowercased copy only line up when the text is ASCII.
    if !content.is_ascii() || content.contains('\x1b') {
        return highlight_log(raw);
    }
    let hay = content.to_ascii_lowercase();
    let needle = query.to_ascii_lowercase();
    let hit = Style::default().bg(Color::Yellow).fg(Color::Black).add_modifier(Modifier::BOLD);
    let mut spans = Vec::new();
    let mut start = 0;
    while let Some(rel) = hay[start..].find(&needle) {
        let at = start + rel;
        if at > start {
            spans.push(Span::raw(content[start..at].to_string()));
        }
        let end = at + needle.len();
        spans.push(Span::styled(content[at..end].to_string(), hit));
        start = end;
    }
    if start < content.len() {
        spans.push(Span::raw(content[start..].to_string()));
    }
    Line::from(spans)
}

// -- helpers ----------------------------------------------------------------

/// Width the detail panes right-align their labels in.
const LABEL_W: usize = 8;

fn kv(k: &str, v: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{k:>LABEL_W$}  "), Style::default().fg(dim())),
        Span::raw(v.to_string()),
    ])
}

/// What the run put on its commit: the tag, then — once a release is published
/// under it — the release's name. Kept to one line, since the rest of the pane
/// is one line per fact and a name long enough to wrap would push the run's own
/// details off a short pane.
fn tag_line(t: &RunTag, width: u16) -> Line<'static> {
    let mut spans = vec![
        Span::styled(format!("{:>LABEL_W$}  ", t.label()), Style::default().fg(dim())),
        Span::styled(
            t.name.clone(),
            // An unreleased tag is worth showing but isn't the headline.
            Style::default()
                .fg(if t.released { accent() } else { dim() })
                .add_modifier(Modifier::BOLD),
        ),
    ];
    let mut used = LABEL_W + 2 + t.name.chars().count();
    if t.prerelease {
        spans.push(Span::styled("  pre", Style::default().fg(Color::Yellow)));
        used += 5;
    }
    // The name comes along only when enough of it fits to be worth reading.
    let room = (width as usize).saturating_sub(used + 2);
    if let Some(name) = t.title.as_deref().filter(|_| room >= 8) {
        spans.push(Span::styled(
            format!("  {}", truncate(name, room)),
            Style::default().fg(dim()),
        ));
    }
    Line::from(spans)
}

fn state_glyph(s: RunState) -> (&'static str, Color) {
    // Single shape, meaning carried by color (filled = terminal/active, hollow = waiting).
    match s {
        RunState::Running => ("●", Color::Yellow),
        RunState::Queued => ("○", Color::Cyan),
        RunState::Success => ("●", Color::Green),
        RunState::Failure => ("●", Color::Red),
        RunState::Cancelled => ("◌", dim()),
        RunState::Skipped => ("○", dim()),
        RunState::Other => ("·", dim()),
    }
}

fn state_label(s: RunState) -> &'static str {
    match s {
        RunState::Running => "in progress",
        RunState::Queued => "queued",
        RunState::Success => "success",
        RunState::Failure => "failure",
        RunState::Cancelled => "cancelled",
        RunState::Skipped => "skipped",
        RunState::Other => "unknown",
    }
}

fn job_dur(j: &Job) -> String {
    match (j.started_at, j.completed_at) {
        (Some(s), Some(e)) => fmt_dur((e - s).num_seconds().max(0)),
        (Some(s), None) => format!("{}…", fmt_dur((Utc::now() - s).num_seconds().max(0))),
        _ => String::new(),
    }
}

/// Scroll offset that centers `cursor` in a viewport of `height` rows over
/// `total` items, clamped so the final page never scrolls past the end.
fn center_scroll(cursor: usize, height: usize, total: usize) -> usize {
    cursor.saturating_sub(height / 2).min(total.saturating_sub(height))
}

fn fmt_dur(secs: i64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Step duration: sub-second precision when short, else reuse `fmt_dur`.
fn fmt_secs(secs: f64) -> String {
    if secs < 10.0 {
        format!("{secs:.1}s")
    } else {
        fmt_dur(secs as i64)
    }
}

fn fmt_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    match n {
        0..=999 => format!("{n} B"),
        _ if n < MB => format!("{:.0} KB", n as f64 / KB as f64),
        _ if n < GB => format!("{:.1} MB", n as f64 / MB as f64),
        _ => format!("{:.1} GB", n as f64 / GB as f64),
    }
}

/// Total wall-clock of a run: live-ticking while active, final once done —
/// or a dash for a run so old GitHub no longer says when it finished.
fn run_dur(r: &Run) -> String {
    match r.state() {
        RunState::Running | RunState::Queued => {
            format!("{}…", fmt_dur((Utc::now() - r.started_at()).num_seconds().max(0)))
        }
        _ => r.duration_secs().map_or_else(|| "—".to_string(), fmt_dur),
    }
}

/// Compact label for a run's trigger event (fits the narrow Event column).
fn event_label(e: &str) -> String {
    match e {
        "push" => "push",
        "pull_request" | "pull_request_target" => "PR",
        "workflow_dispatch" => "manual",
        "schedule" => "cron",
        "release" => "release",
        "workflow_run" => "wf-run",
        "repository_dispatch" => "repo",
        "merge_group" => "merge",
        "deployment" | "deployment_status" => "deploy",
        other => return truncate(other, 8),
    }
    .to_string()
}

pub fn fmt_age(ts: DateTime<Utc>) -> String {
    let secs = (Utc::now() - ts).num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

fn fmt_dt(ts: DateTime<Utc>) -> String {
    ts.format("%Y-%m-%d %H:%M UTC").to_string()
}

/// What the runs table spends before its first resizable column: the selection
/// marker, the status glyph, and the gap after it.
const LEAD: u16 = 4;

/// One table cell, cut to the width its column is actually drawn at.
fn cell(r: &Run, tag: Option<&RunTag>, col: Column, w: usize, spin: &str) -> Cell<'static> {
    let dimmed = Style::default().fg(dim());
    match col {
        Column::Repo => Cell::from(truncate(&r.repository.full_name, w)),
        // The workflow's name, with its run number kept dim and always visible.
        // A run GitHub hasn't numbered yet spins there instead: #0 would be a
        // number that doesn't exist.
        Column::Workflow => {
            let number = if r.is_pending() {
                format!("  {spin}")
            } else {
                format!("  #{}", r.run_number)
            };
            Cell::from(Line::from(vec![
                Span::raw(truncate(r.workflow_name(), w.saturating_sub(number.chars().count()))),
                Span::styled(number, dimmed),
            ]))
        }
        // Blank where a run tagged nothing — the column is only on screen
        // because something on the list did. A released tag is the one worth
        // spotting from across the table, so only that one gets the accent.
        Column::Tag => match tag {
            Some(t) if t.released => Cell::from(truncate(&t.name, w))
                .style(Style::default().fg(accent()).add_modifier(Modifier::BOLD)),
            Some(t) => Cell::from(truncate(&t.name, w)).style(dimmed),
            None => Cell::from(""),
        },
        Column::Branch => Cell::from(truncate(r.head_branch.as_deref().unwrap_or("-"), w)),
        Column::Event => Cell::from(truncate(&event_label(&r.event), w)),
        Column::Actor => Cell::from(truncate(
            r.actor.as_ref().map(|a| a.login.as_str()).unwrap_or("-"),
            w,
        )),
        Column::Dur => Cell::from(run_dur(r)).style(dimmed),
        Column::Age => Cell::from(fmt_age(r.last_activity())).style(dimmed),
    }
}

fn short_repo(full: &str) -> String {
    // Keep owner/name but cap length nicely.
    truncate(full, 24)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let keep: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{keep}…")
    }
}

/// Colorize one raw log line: GitHub `##[…]` workflow markers, then any
/// embedded ANSI escape sequences the build tools emitted. `log_content`
/// strips the BOM, trailing CR/LF, and the ISO timestamp prefix.
fn highlight_log(raw: &str) -> Line<'static> {
    let content = log_content(raw);

    let marker = |prefix: &str, rest: &str, color: Color, bold: bool| {
        let mut style = Style::default().fg(color);
        if bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        Line::from(Span::styled(format!("{prefix}{rest}"), style))
    };

    if let Some(rest) = content.strip_prefix("##[error]") {
        return marker("✗ ", rest, Color::Red, true);
    }
    if let Some(rest) = content.strip_prefix("##[warning]") {
        return marker("▲ ", rest, Color::Yellow, true);
    }
    if let Some(rest) = content.strip_prefix("##[notice]") {
        return marker("● ", rest, Color::Cyan, false);
    }
    if let Some(rest) = content.strip_prefix("##[group]") {
        return marker("▸ ", rest, accent(), true);
    }
    if content.starts_with("##[endgroup]") {
        return Line::raw("");
    }
    if let Some(rest) = content.strip_prefix("##[command]") {
        return marker("$ ", rest, Color::Magenta, false);
    }
    if let Some(rest) = content.strip_prefix("##[debug]") {
        return marker("", rest, dim(), false);
    }
    if let Some(rest) = content.strip_prefix("##[section]") {
        return marker("", rest, accent(), true);
    }

    // Render embedded ANSI (e.g. colored cargo/clippy/test output) faithfully.
    if content.contains('\x1b') {
        if let Ok(text) = content.into_text() {
            if let Some(first) = text.lines.into_iter().next() {
                return first;
            }
        }
    }
    Line::raw(content.to_string())
}

fn centered(pct_x: u16, pct_y: u16, area: Rect) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(v[1])[1]
        .inner(Margin::new(0, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// Draw the whole screen into an off-screen terminal and read it back, so
    /// layout changes are checked against what actually lands on the grid.
    fn screen(app: &mut App, w: u16, h: u16) -> String {
        app.sync_layout(w);
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        let mut out = String::new();
        for y in 0..h {
            for x in 0..w {
                out.push_str(buf.cell((x, y)).map_or(" ", |c| c.symbol()));
            }
            out.push('\n');
        }
        out
    }

    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    fn demo_app() -> App {
        use crate::app::DataMsg;
        use crate::github::{Run, RunRepo};

        let run = |repo: &str, id: u64, status: &str, conclusion: Option<&str>| Run {
            id,
            name: Some("CI".into()),
            display_title: "fix the thing".into(),
            head_branch: Some("main".into()),
            head_sha: "a1b2c3d".into(),
            run_number: 296,
            workflow_id: 11,
            event: "push".into(),
            status: status.into(),
            conclusion: conclusion.map(str::to_string),
            html_url: String::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            run_started_at: Some(Utc::now()),
            actor: None,
            repository: RunRepo { full_name: repo.into() },
        };
        let cfg = crate::config::Config { pinned: vec!["org/api".into()], ..Default::default() };
        let mut app = App::new(&cfg);
        app.user = "you".into();
        app.apply(DataMsg::Repos(vec![
            "org/api".into(),
            "org/web".into(),
            "you/dotfiles".into(),
            "org/quiet-one".into(),
            "shamilatesoglu/actui-experiments".into(),
        ]));
        app.loading = false;
        // Ignore whatever this machine has saved, so renders are deterministic.
        app.state = crate::state::State::default();
        app.panes = crate::app::Panes::new(&app.state);
        app.runs = vec![
            run("org/api", 1, "in_progress", None),
            run("org/web", 2, "completed", Some("failure")),
            run("you/dotfiles", 3, "completed", Some("success")),
        ];
        app.recompute_view();
        app
    }

    #[test]
    fn sidebar_renders_beside_the_runs_table() {
        let mut app = demo_app();
        let out = screen(&mut app, 120, 14);
        println!("{out}");

        assert!(out.contains("All repos"), "the scope-clearing row is drawn");
        assert!(out.contains("★ org/api"), "a pinned repo is marked");
        assert!(out.contains("org/quiet-one"), "a repo with no runs is still listed");
        assert!(out.contains("Repos › Runs › Jobs"), "the breadcrumb gains a level");
        // Unscoped, the runs table keeps its repository column.
        assert!(out.contains("Repository"));
    }

    #[test]
    fn scoping_to_a_repo_drops_the_repository_column() {
        let mut app = demo_app();
        app.sync_layout(120);
        assert!(app.repos.select_repo("org/web"));
        app.recompute_view();
        let out = screen(&mut app, 120, 14);
        println!("{out}");

        assert!(out.contains("Runs · org/web 1/1"), "the pane title names the scope");
        assert!(!out.contains("Repository"), "the repo column is redundant when scoped");
        assert!(!out.contains("you/dotfiles   CI"), "other repos' runs are filtered out");
    }

    #[test]
    fn a_wide_table_caps_its_columns_instead_of_stretching() {
        let mut app = demo_app();
        let out = screen(&mut app, 220, 12);
        println!("{out}");
        let row = out.lines().find(|l| l.contains("org/api  ")).unwrap();
        // The sidebar names the repo too — the table's copy is the later one.
        let from = row.rfind("org/api").unwrap();
        let span = row[from..].find("CI  #296").unwrap();
        assert!(span <= 26, "the repository column should stay capped, spans {span}");
    }

    #[test]
    fn the_help_fits_a_standard_terminal() {
        let mut app = demo_app();
        app.mode = crate::app::Mode::Help;
        let out = screen(&mut app, 120, 40);
        println!("{out}");
        // The last row of each column has to survive the popup's height.
        assert!(out.contains("save the log to a file"), "left column is clipped");
        assert!(out.contains("show repos that failed to load"), "right column is clipped");
    }

    #[test]
    fn a_repo_with_no_runs_points_at_dispatch() {
        let mut app = demo_app();
        app.sync_layout(120);
        assert!(app.repos.select_repo("org/quiet-one"));
        app.recompute_view();
        let out = screen(&mut app, 120, 14);
        println!("{out}");
        assert!(out.contains("No recent runs in org/quiet-one"));
        assert!(out.contains("Press d to dispatch"));
    }

    #[test]
    fn a_name_too_long_for_the_sidebar_slides_past() {
        let mut app = demo_app();
        let first = screen(&mut app, 120, 14);
        assert!(app.repos.sliding, "a name that long has to slide");

        app.repos.step += 8;
        let later = screen(&mut app, 120, 14);
        println!("{first}\n{later}");
        assert_ne!(first, later, "the name should have moved on");
        // Every row still ends where it started: sliding never widens a row.
        let width = |out: &str| out.lines().map(|l| l.chars().count()).max().unwrap();
        assert_eq!(width(&first), width(&later));
    }

    #[test]
    fn resizing_the_sidebar_widens_the_pane_and_is_remembered() {
        use crossterm::event::{KeyCode, KeyEvent};

        let mut app = demo_app();
        let before = app.panes.sidebar;
        app.sync_layout(120);
        app.handle_key(KeyEvent::from(KeyCode::Char('>')));
        app.handle_key(KeyEvent::from(KeyCode::Char('>')));
        assert!(app.panes.sidebar > before);
        // What a save would write out.
        let mut saved = crate::state::State::default();
        app.panes.store(&mut saved);
        assert_eq!(saved.sidebar_width(), Some(app.panes.sidebar));

        let out = screen(&mut app, 120, 14);
        println!("{out}");
        let title: Vec<char> = out.lines().find(|l| l.contains("Repos 1/")).unwrap().chars().collect();
        let corner = |c: char| title.iter().position(|x| *x == c).unwrap();
        assert_eq!(corner('╮') - corner('╭') + 1, app.panes.sidebar as usize);

        // And it can't be dragged past the room the runs and detail panes need.
        for _ in 0..40 {
            app.handle_key(KeyEvent::from(KeyCode::Char('>')));
        }
        assert!(app.panes.sidebar <= 120 - crate::app::min_body(false));
        assert!(app.repos.shown, "widening must not push it off screen");
    }

    /// Screen column where `label` starts, in whole characters.
    fn column_of(line: &str, label: &str) -> usize {
        let chars: Vec<char> = line.chars().collect();
        let target: Vec<char> = label.chars().collect();
        chars.windows(target.len()).position(|w| w == target).unwrap()
    }

    /// The screen row `label` is drawn on.
    fn row_of(out: &str, label: &str) -> u16 {
        out.lines().position(|l| l.contains(label)).unwrap() as u16
    }

    fn mouse(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
        MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }
    }

    #[test]
    fn dragging_the_sidebar_border_resizes_it_and_is_remembered() {
        let mut app = demo_app();
        let out = screen(&mut app, 120, 14);
        let row = row_of(&out, "All repos");
        let border = app.panes.sidebar; // the body starts at column 0

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), border, row));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), border + 8, row));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), border + 8, row));
        assert_eq!(app.panes.sidebar, border + 9);
        assert_eq!(app.panes.dragging, None, "letting go ends the drag");

        let out = screen(&mut app, 120, 14);
        println!("{out}");
        let title: Vec<char> = out.lines().find(|l| l.contains("Repos 1/")).unwrap().chars().collect();
        let corner = |c: char| title.iter().position(|x| *x == c).unwrap();
        assert_eq!(corner('╮') - corner('╭') + 1, app.panes.sidebar as usize);

        let mut saved = crate::state::State::default();
        app.panes.store(&mut saved);
        assert_eq!(saved.sidebar_width(), Some(app.panes.sidebar));
    }

    #[test]
    fn dragging_the_detail_border_resizes_that_pane() {
        let mut app = demo_app();
        let out = screen(&mut app, 120, 14);
        let row = row_of(&out, "All repos");
        let was = app.panes.detail;
        let border = 120 - was; // the body spans the full width

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), border, row));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), border - 6, row));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), border - 6, row));
        assert_eq!(app.panes.detail, was + 6);

        let out = screen(&mut app, 120, 14);
        println!("{out}");
        let title = out.lines().find(|l| l.contains("Detail")).unwrap();
        assert_eq!(column_of(title, "╭ Detail") as u16, border - 6);
    }

    #[test]
    fn dragging_a_column_separator_resizes_that_column_only() {
        let mut app = demo_app();
        let out = screen(&mut app, 120, 14);
        let header_row = row_of(&out, "Repository");
        let header = out.lines().nth(header_row as usize).unwrap();
        // Columns are laid out with one blank between them, so the repository
        // column runs from where its title starts to one left of that blank.
        let repo_x = column_of(header, "Repository") as u16;
        let was = column_of(header, "Workflow") as u16 - repo_x - 1;
        let separator = repo_x + was - 1;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), separator, header_row));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), separator - 8, header_row));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), separator - 8, header_row));
        assert_eq!(app.panes.column_width(Column::Repo), was - 8);
        assert_eq!(app.panes.column_width(Column::Workflow), Column::Workflow.default_width());

        let out = screen(&mut app, 120, 14);
        println!("{out}");
        let header = out.lines().nth(header_row as usize).unwrap();
        assert_eq!(column_of(header, "Workflow") as u16, repo_x + was - 8 + 1);
        // The repo names are cut to the column's new width, not the old one.
        assert!(out.contains("you/d…"), "names follow the column they're in");

        let mut saved = crate::state::State::default();
        app.panes.store(&mut saved);
        assert_eq!(saved.columns().get("repo"), Some(&(was - 8)));
    }

    #[test]
    fn a_press_that_misses_a_divider_still_selects_a_row() {
        let mut app = demo_app();
        let out = screen(&mut app, 120, 14);
        let row = row_of(&out, "org/web");
        let x = column_of(out.lines().nth(row as usize).unwrap(), "org/web") as u16;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, row));
        assert_eq!(app.panes.dragging, None);
        assert_eq!(
            app.selected_run().map(|r| r.repository.full_name.as_str()),
            Some("org/web")
        );
    }

    #[test]
    fn the_live_steps_view_still_draws() {
        use crate::app::StepsView;

        let mut app = demo_app();
        app.jobs = vec![crate::github::Job {
            id: 9,
            name: "build".into(),
            status: "in_progress".into(),
            html_url: String::new(),
            check_run_url: String::new(),
            conclusion: None,
            started_at: None,
            completed_at: None,
            steps: Vec::new(),
        }];
        app.jobs_run_id = Some(1);
        app.steps_view = Some(StepsView {
            job_id: 9,
            job_name: "build".into(),
            repo: "org/api".into(),
            cursor: 0,
        });
        app.mode = crate::app::Mode::Logs;

        let out = screen(&mut app, 120, 14);
        println!("{out}");
        assert!(out.contains("Live steps · build"), "the steps pane is drawn");
        assert!(out.contains("live steps · updates automatically"), "and its footer hint");
        assert!(out.contains("Runs 1/3"), "beside the runs table it drills from");
        assert!(out.contains("All repos"), "and the sidebar stays alongside it");

        // Four panes don't fit on a narrow terminal; the sidebar is what goes.
        let out = screen(&mut app, 100, 14);
        println!("{out}");
        assert!(!out.contains("All repos"));
        assert!(out.contains("Live steps · build"), "the steps pane stays");
    }

    #[test]
    fn dragging_the_steps_border_resizes_that_pane() {
        use crate::app::StepsView;

        let mut app = demo_app();
        app.steps_view = Some(StepsView {
            job_id: 9,
            job_name: "build".into(),
            repo: "org/api".into(),
            cursor: 0,
        });
        app.mode = crate::app::Mode::Logs;

        let out = screen(&mut app, 160, 14);
        let row = row_of(&out, "All repos");
        let was = app.panes.steps;
        let border = 160 - was;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), border, row));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), border - 10, row));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), border - 10, row));
        assert_eq!(app.panes.steps, was + 10);

        let out = screen(&mut app, 160, 14);
        println!("{out}");
        let title = out.lines().find(|l| l.contains("Live steps")).unwrap();
        assert_eq!(column_of(title, "╭ Live steps") as u16, border - 10);

        let mut saved = crate::state::State::default();
        app.panes.store(&mut saved);
        assert_eq!(saved.steps_width(), Some(app.panes.steps));
    }

    #[test]
    fn the_tabs_row_switches_to_the_runners_view() {
        let mut app = demo_app();
        let out = screen(&mut app, 120, 14);
        println!("{out}");
        assert!(out.contains("⚙ Runners"), "the switcher sits beside the filters");
        assert!(out.contains("s runners"), "and the footer names its key again");
        assert!(!out.contains("│   Runners"), "it is not a row in the repos list");

        // Scope to a repo, then click the switcher.
        assert!(app.repos.select_repo("org/web"));
        app.recompute_view();
        let row = row_of(&out, "Running");
        let at = column_of(out.lines().nth(row as usize).unwrap(), "⚙ Runners") as u16;
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), at, row));
        assert!(matches!(app.mode, crate::app::Mode::Runners));

        let out = screen(&mut app, 120, 14);
        println!("{out}");
        assert!(out.contains("Org runners"), "the view opens");
        assert!(out.contains("All repos"), "beside the sidebar, which stays put");

        // Clicking it again comes back, with the repo scope untouched.
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), at, row));
        assert!(matches!(app.mode, crate::app::Mode::Normal));
        assert_eq!(app.repos.selected_repo(), Some("org/web"));

        // And `s` still opens it from the keyboard.
        use crossterm::event::{KeyCode, KeyEvent};
        app.handle_key(KeyEvent::from(KeyCode::Char('s')));
        assert!(matches!(app.mode, crate::app::Mode::Runners));
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(app.repos.selected_repo(), Some("org/web"));
    }


    /// A finished, successful run of `secs`, `mins` minutes back — the shape
    /// the workflow-runs endpoint hands over.
    fn past_run(id: u64, number: u64, secs: i64, mins: i64) -> crate::github::Run {
        use crate::github::{Run, RunRepo};
        let end = Utc::now() - chrono::Duration::minutes(mins);
        Run {
            id,
            name: Some("CI".into()),
            display_title: "fix the thing".into(),
            head_branch: Some("main".into()),
            head_sha: "a1b2c3d".into(),
            run_number: number,
            workflow_id: 11,
            event: "push".into(),
            status: "completed".into(),
            conclusion: Some("success".into()),
            html_url: format!("https://github.com/org/api/actions/runs/{id}"),
            created_at: end - chrono::Duration::seconds(secs),
            updated_at: end,
            run_started_at: Some(end - chrono::Duration::seconds(secs)),
            actor: None,
            repository: RunRepo { full_name: "org/api".into() },
        }
    }

    /// The demo app on the chart, with five minutes-long successful runs on it.
    /// The newest is run 3, which the runs list also holds — so `⏎` has
    /// somewhere to go.
    fn app_on_the_chart() -> App {
        use crossterm::event::{KeyCode, KeyEvent};

        let mut app = demo_app();
        app.handle_key(KeyEvent::from(KeyCode::Char('w')));
        // Newest first, as GitHub returns them.
        let runs = vec![
            past_run(3, 300, 300, 1),
            past_run(104, 299, 60, 2),
            past_run(103, 298, 240, 3),
            past_run(102, 297, 120, 4),
            past_run(101, 296, 180, 5),
        ];
        app.apply(crate::app::DataMsg::WorkflowRuns { workflow_id: 11, runs });
        app
    }

    #[test]
    fn the_tabs_row_switches_to_the_duration_chart() {
        let mut app = demo_app();
        let out = screen(&mut app, 120, 18);
        println!("{out}");
        assert!(out.contains("◷ Duration"), "the switcher sits beside the runners one");
        assert!(out.contains("⚙ Runners"), "which keeps its place");

        let row = row_of(&out, "Running");
        let at = column_of(out.lines().nth(row as usize).unwrap(), "◷ Duration") as u16;
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), at, row));
        assert!(matches!(app.mode, crate::app::Mode::Timing));

        let out = screen(&mut app, 120, 18);
        println!("{out}");
        assert!(out.contains("Duration · org/api › CI"), "titled for the run's workflow");
        assert!(out.contains("Reading"), "and waiting on its history");
        assert!(out.contains("All repos"), "beside the sidebar, which stays put");

        // Clicking it again comes back, with the repo scope untouched.
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), at, row));
        assert!(matches!(app.mode, crate::app::Mode::Normal));
    }

    #[test]
    fn the_chart_draws_the_runs_with_their_numbers_under_it() {
        use crossterm::event::{KeyCode, KeyEvent};

        let mut app = app_on_the_chart();
        let out = screen(&mut app, 120, 18);
        println!("{out}");

        assert!(out.contains("█"), "the longest run reaches the top of the plot");
        assert!(out.contains("┄"), "the median is drawn across the shorter runs");
        let braille = |c: char| ('\u{2801}'..='\u{28FF}').contains(&c);
        assert!(out.chars().any(braille), "and the smoothed curve runs over them, in braille");
        assert!(out.contains("███████████ ███████████"), "five runs fill the width as wide bars");
        assert!(out.contains("5m0s┤"), "the axis names its top");
        assert!(out.contains("1m0s┤"), "and its floor, since the bars don't start at zero");
        assert!(!out.contains("↑"), "with no outlier, nothing runs off the top");
        assert!(
            out.contains("5 runs over 5m · min 1m0s · median 3m0s · max 5m0s"),
            "the summary reads off the whole history"
        );
        // The cursor opens on the newest run, and the detail line names it.
        assert!(out.contains("#300 · 5m0s · main · push"), "the cursor's run is spelled out");

        // Moving left steps to the run before it.
        app.handle_key(KeyEvent::from(KeyCode::Char('h')));
        let out = screen(&mut app, 120, 18);
        println!("{out}");
        assert!(out.contains("#299 · 1m0s · main"));
    }

    #[test]
    fn a_history_longer_than_the_chart_folds_into_the_columns_there_are() {
        use crossterm::event::{KeyCode, KeyEvent};
        let mut app = demo_app();
        app.handle_key(KeyEvent::from(KeyCode::Char('w')));
        // 100 runs wobbling around two minutes, with one slow outlier.
        let runs: Vec<crate::github::Run> = (0..100)
            .map(|i| {
                let secs = 110 + (i * 37) % 40 + if i == 6 { 260 } else { 0 };
                past_run(1000 + i as u64, 400 - i as u64, secs, i)
            })
            .collect();
        app.apply(crate::app::DataMsg::WorkflowRuns { workflow_id: 11, runs });

        let out = screen(&mut app, 120, 18);
        println!("{out}");
        assert!(out.contains("100 runs over"), "the summary counts them and says over how long");
        // Every column now stands for more than one run, and says so.
        assert!(out.contains("longest of"), "a folded column names what it covers");
        // The outlier is marked rather than being allowed to flatten the rest.
        assert!(out.contains("↑"), "the slow run is shown running off the top");
        assert!(out.contains("max 6m32s"), "and the summary still gives its length");
    }

    #[test]
    fn a_run_github_has_housekept_shows_no_duration_and_keeps_its_place() {
        use crate::github::{Run, RunRepo};
        let mut app = demo_app();
        // Finished in six minutes 400 days ago; GitHub touched it yesterday when
        // its logs expired. Read naively it ran for 9600 hours and just finished.
        let start = Utc::now() - chrono::Duration::days(400);
        let ancient = Run {
            id: 9,
            name: Some("Release".into()),
            display_title: "ancient".into(),
            head_branch: Some("main".into()),
            head_sha: "a1b2c3d".into(),
            run_number: 12,
            workflow_id: 11,
            event: "push".into(),
            status: "completed".into(),
            conclusion: Some("success".into()),
            html_url: String::new(),
            created_at: start,
            updated_at: start + chrono::Duration::days(400) + chrono::Duration::minutes(6),
            run_started_at: Some(start),
            actor: None,
            repository: RunRepo { full_name: "org/quiet-one".into() },
        };
        app.apply(crate::app::DataMsg::RunsOnly { repo: "org/quiet-one".into(), runs: vec![ancient] });

        // Wide enough for the runs table to show its duration column.
        let out = screen(&mut app, 160, 14);
        println!("{out}");
        let row = out.lines().find(|l| l.contains("Release")).expect("the run is listed");
        assert!(row.contains("—"), "its duration is a dash, not 9600h: {row}");
        assert!(row.contains("400d"), "and its age is counted from when it ran: {row}");
        assert!(!row.contains("9600h"));
        // It sorts as the oldest run, not the newest, and the sidebar's rollup
        // says when it last ran rather than when GitHub last touched it.
        assert_eq!(app.runs.last().map(|r| r.id), Some(9));
        assert!(out.contains("org/quiet-one     400d"), "the sidebar dates it from its run");
    }

    #[test]
    fn the_chart_counts_runs_it_cannot_time_rather_than_plotting_them() {
        use crossterm::event::{KeyCode, KeyEvent};
        let mut app = demo_app();
        app.handle_key(KeyEvent::from(KeyCode::Char('w')));
        let mut old = past_run(101, 296, 360, 5);
        old.updated_at = old.started_at() + chrono::Duration::days(400) + chrono::Duration::minutes(6);
        let runs = vec![past_run(3, 300, 300, 1), past_run(104, 299, 60, 2), old];
        app.apply(crate::app::DataMsg::WorkflowRuns { workflow_id: 11, runs });

        let out = screen(&mut app, 120, 18);
        println!("{out}");
        assert!(out.contains("2 runs over"), "only the runs with a known length are charted");
        assert!(out.contains("1 with no known duration"), "the one left off is counted");
        assert!(!out.contains("9600h"));

        // A workflow with nothing left to chart says why, not "no runs".
        let mut old = past_run(101, 296, 360, 5);
        old.updated_at = old.started_at() + chrono::Duration::days(400);
        app.apply(crate::app::DataMsg::WorkflowRuns { workflow_id: 11, runs: vec![old] });
        let out = screen(&mut app, 120, 18);
        println!("{out}");
        assert!(out.contains("past the repo's retention"));
        assert!(!out.contains("No successful runs"));
    }

    #[test]
    fn the_chart_survives_a_pane_too_narrow_to_plot_in() {
        let mut app = app_on_the_chart();
        // Narrow enough that the sidebar goes and the plot has no room left.
        let out = screen(&mut app, 34, 10);
        println!("{out}");
        assert!(out.contains("5 runs"), "the numbers are what still fit");
    }

    #[test]
    fn the_curve_keeps_the_colour_of_the_bar_it_crosses_behind_it() {
        // The curve is braille dots over whatever cell it crosses. Over a solid
        // bar the bar's colour has to move to the background, or the line
        // punches a hole through it — the selected bar included.
        let mut app = app_on_the_chart();
        app.sync_layout(120);
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 18)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let braille = |c: char| ('\u{2801}'..='\u{28FF}').contains(&c);
        let (mut over_cursor, mut over_bar) = (0, 0);
        for cell in term.backend().buffer().content() {
            if !cell.symbol().chars().next().is_some_and(braille) {
                continue;
            }
            if cell.bg == text() {
                over_cursor += 1;
            } else if cell.bg == accent() {
                over_bar += 1;
            }
        }
        assert!(over_cursor > 0, "the selected bar keeps its colour behind the curve");
        assert!(over_bar > 0, "and so does an ordinary one");
    }

    #[test]
    fn enter_goes_back_to_the_run_under_the_cursor() {
        use crossterm::event::{KeyCode, KeyEvent};

        let mut app = app_on_the_chart();
        screen(&mut app, 120, 18);

        // The newest bar is run 3, which the runs list holds.
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(app.mode, crate::app::Mode::Normal));
        assert_eq!(app.selected_run().map(|r| r.id), Some(3));

        // A run older than the list holds has nowhere to go, so it says so and
        // stays on the chart.
        app.handle_key(KeyEvent::from(KeyCode::Char('w')));
        screen(&mut app, 120, 18);
        app.handle_key(KeyEvent::from(KeyCode::Char('h')));
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(app.mode, crate::app::Mode::Timing));
        let (msg, is_err) = app.status().unwrap();
        assert!(msg.contains("#299"), "names the run it cannot reach: {msg}");
        assert!(is_err);
    }

    #[test]
    fn the_workflow_list_is_only_fetched_when_you_ask_to_move_off_this_one() {
        use crossterm::event::{KeyCode, KeyEvent};

        let mut app = demo_app();
        app.handle_key(KeyEvent::from(KeyCode::Char('w')));
        assert!(
            !app.pending.iter().any(|c| matches!(c, crate::app::Command::FetchWorkflows { .. })),
            "opening the chart costs one conditional request, not two"
        );

        app.handle_key(KeyEvent::from(KeyCode::Char(']')));
        assert!(
            app.pending.iter().any(|c| matches!(c, crate::app::Command::FetchWorkflows { .. })),
            "stepping to another workflow asks for the list"
        );
    }

    #[test]
    fn a_workflow_with_no_successes_says_so() {
        use crossterm::event::{KeyCode, KeyEvent};

        let mut app = demo_app();
        app.handle_key(KeyEvent::from(KeyCode::Char('w')));
        app.apply(crate::app::DataMsg::WorkflowRuns { workflow_id: 11, runs: vec![] });
        let out = screen(&mut app, 120, 18);
        println!("{out}");
        assert!(out.contains("No successful runs of CI"));
    }

    /// A sidebar with far more repos than fit, and where its scrollbar landed.
    fn app_with_a_long_sidebar() -> (App, String, u16) {
        let mut app = demo_app();
        let many: Vec<String> = (0..40).map(|i| format!("org/repo-{i:02}")).collect();
        app.apply(crate::app::DataMsg::Repos(many));
        app.recompute_view();
        let out = screen(&mut app, 120, 14);
        let track_x = app.panes.sidebar - 1;
        (app, out, track_x)
    }

    /// The row a scrollbar glyph is drawn on: ▲ and ▼ are the arrows, █ the thumb.
    fn bar_row(out: &str, x: u16, glyph: char) -> u16 {
        out.lines()
            .position(|l| l.chars().nth(x as usize) == Some(glyph))
            .unwrap_or_else(|| panic!("no {glyph} in the bar")) as u16
    }

    #[test]
    fn a_scrollbars_arrows_step_and_its_track_pages() {
        let (mut app, out, x) = app_with_a_long_sidebar();
        println!("{out}");
        let (up, down) = (bar_row(&out, x, '▲'), bar_row(&out, x, '▼'));
        assert_eq!(app.repos.state.offset(), 0);

        // The bottom arrow steps one row; the top one steps back.
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, down));
        assert_eq!(app.repos.state.offset(), 1);
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), x, down));
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, up));
        assert_eq!(app.repos.state.offset(), 0);
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), x, up));

        // The track below the thumb pages down by a viewport.
        let out = screen(&mut app, 120, 14);
        let thumb = bar_row(&out, x, '█');
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, thumb + 1));
        let paged = app.repos.state.offset();
        assert!(paged > 1, "a press on the track pages rather than stepping, got {paged}");
        assert!(paged <= app.repos.len(), "and never past the end");
    }

    #[test]
    fn dragging_a_scrollbars_thumb_scrolls_the_list_under_it() {
        let (mut app, out, x) = app_with_a_long_sidebar();
        let (thumb, down) = (bar_row(&out, x, '█'), bar_row(&out, x, '▼'));

        // Grab the thumb and pull it to the bottom of the track.
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, thumb));
        assert_eq!(app.repos.state.offset(), 0, "grabbing the thumb doesn't move it");
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), x, down));
        let bottom = app.repos.state.offset();
        assert_eq!(bottom, app.repos.len() - app.hit.repos.height as usize);

        let out = screen(&mut app, 120, 14);
        println!("{out}");
        assert!(out.contains("org/repo-39"), "the end of the list is on screen");
        assert!(!out.contains("All repos"), "and the start has scrolled off");

        // Dragging back up follows the pointer, even once it leaves the track.
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 0, 0));
        assert_eq!(app.repos.state.offset(), 0);
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 0, 0));
        assert!(app.panes.scrolling.is_none(), "letting go lets go");

        // The wheel over the bar scrolls it too.
        app.handle_mouse(mouse(MouseEventKind::ScrollDown, x, thumb));
        assert!(app.repos.state.offset() > 0);
    }

    fn tag(name: &str, released: bool, title: Option<&str>) -> crate::github::RunTag {
        crate::github::RunTag {
            url: format!("http://x/releases/tag/{name}"),
            name: name.into(),
            released,
            title: title.map(str::to_string),
            prerelease: false,
        }
    }

    #[test]
    fn the_runs_table_shows_a_tag_column_only_when_it_has_something_to_say() {
        let mut app = demo_app();
        let (released, plain) = (app.runs[1].id, app.runs[2].id);

        // Nothing tagged: no column, no blank stripe down the table.
        let out = screen(&mut app, 130, 16);
        assert!(!out.contains("Tag"), "no column before any run has one");

        app.apply(crate::app::DataMsg::Tags {
            found: vec![
                (released, Some(tag("v0.4.0", true, None))),
                // A tag the workflow pushed without publishing a release.
                (plain, Some(tag("nightly-2026-09-04", false, None))),
            ],
        });

        let out = screen(&mut app, 130, 16);
        println!("{out}");
        assert!(out.contains("Tag"), "the column appears with the first tag");
        let rows: Vec<&str> = out.lines().filter(|l| l.contains("org/")).collect();
        // Each tag lands on the run that made it, and the untagged run stays blank.
        assert!(rows.iter().any(|l| l.contains("org/web") && l.contains("v0.4.0")));
        assert!(rows.iter().any(|l| l.contains("dotfiles") && l.contains("nightly-2…")));
        let api = rows.iter().find(|l| l.contains("org/api")).unwrap();
        assert!(!api.contains("v0.4.0") && !api.contains("nightly"), "untagged: {api}");

        // Too narrow to spare the width: the essentials win and the column goes.
        let narrow = screen(&mut app, 84, 16);
        println!("{narrow}");
        assert!(!narrow.contains("Tag"), "no room for it here");
        assert!(narrow.contains("Workflow"), "the essentials stay");
    }

    #[test]
    fn the_detail_pane_names_a_bare_tag_a_tag_and_a_released_one_a_release() {
        let mut app = demo_app();
        app.table_state.select(Some(0));
        let run_id = app.selected_run().unwrap().id;

        // Nothing tagged yet: the pane says nothing, and the footer doesn't
        // offer a key that would go nowhere.
        let out = screen(&mut app, 130, 24);
        assert!(!out.contains("t tag") && !out.contains("t release"));

        // A tag with no release behind it is still worth showing — as a tag.
        app.apply(crate::app::DataMsg::Tags {
            found: vec![(run_id, Some(tag("v0.4.0", false, None)))],
        });
        let out = screen(&mut app, 130, 24);
        println!("{out}");
        let row = out.lines().find(|l| l.contains("v0.4.0")).expect("a tag row");
        assert!(row.contains("tag  v0.4.0"), "called a tag: {row}");
        assert!(!row.contains("release"), "it isn't one yet: {row}");
        assert!(out.contains("t tag"), "and the key says what it opens");

        // The workflow publishes under it: same row, now a release.
        app.apply(crate::app::DataMsg::Tags {
            found: vec![(run_id, Some(tag("v0.4.0", true, Some("Scrollbars everywhere"))))],
        });
        let out = screen(&mut app, 130, 24);
        println!("{out}");
        let rows: Vec<&str> = out.lines().filter(|l| l.contains("v0.4.0")).collect();
        assert_eq!(rows.len(), 1, "one line, like every other row in the pane");
        assert!(rows[0].contains("release  v0.4.0"), "promoted: {}", rows[0]);
        assert!(rows[0].contains("Scrollbars"), "and what it's called: {}", rows[0]);
        assert!(out.contains("t release"), "the key follows suit");

        // Squeezed, the tag still gets through — it's the part that identifies
        // the release — and the row never wraps onto a second line.
        let narrow = screen(&mut app, 92, 24);
        println!("{narrow}");
        let rows: Vec<&str> = narrow.lines().filter(|l| l.contains("v0.4.0")).collect();
        assert_eq!(rows.len(), 1, "still one line: {rows:?}");
        assert!(narrow.contains("started"), "and the run's own details stay put");

        // `t` opens it; the run page stays on `o`.
        app.handle_key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('t')));
        assert!(app
            .pending
            .iter()
            .any(|c| matches!(c, crate::app::Command::OpenUrl(u) if u.ends_with("v0.4.0"))));
    }

    #[test]
    fn the_tag_row_marks_prereleases_and_drops_the_name_when_squeezed() {
        let pre = crate::github::RunTag { prerelease: true, ..tag("v0.5.0-rc1", true, Some("Release candidate")) };
        let text = |l: Line| l.spans.iter().map(|s| s.content.to_string()).collect::<String>();

        let wide = text(tag_line(&pre, 60));
        assert!(wide.contains("v0.5.0-rc1"));
        assert!(wide.contains("pre"), "a prerelease says so: {wide}");
        assert!(wide.contains("Release candidate"));

        // Too narrow for the name to say anything: the tag keeps the whole row.
        let tight = text(tag_line(&pre, 26));
        assert!(tight.contains("v0.5.0-rc1") && tight.contains("pre"));
        assert!(!tight.contains("candidate"), "a stub is worse than none: {tight}");
    }

    #[test]
    fn a_dispatch_waiting_on_github_spins_instead_of_showing_a_number() {
        let mut app = demo_app();
        let id = app.push_dispatch_placeholder("org/api", "Release", "main");
        app.recompute_view();
        // The placeholder is what the runs list is looking at.
        let at = app.view.iter().position(|&i| app.runs[i].id == id).unwrap();
        app.table_state.select(Some(at));

        let out = screen(&mut app, 120, 14);
        println!("{out}");
        assert!(!out.contains("#0"), "there is no run #0 to show");
        assert!(out.contains("dispatching"), "the detail pane says what's happening");
        // Both the row and the detail pane spin while we wait.
        let frames = SPINNER.iter().filter(|f| out.contains(**f)).count();
        assert!(frames > 0, "a spinner stands in for the number");

        // And the spinner keeps turning even though nothing is loading.
        assert!(!app.loading);
        let was = app.spinner;
        assert!(app.tick(), "a pending dispatch keeps the frames coming");
        assert_ne!(app.spinner, was);

        // Opening it in a browser would go nowhere, so it says so instead.
        app.handle_key(crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Char('o')));
        assert_eq!(
            app.status().map(|(m, _)| m),
            Some("That run hasn't appeared on GitHub yet")
        );
    }

    #[test]
    fn a_dragged_thumb_stays_under_the_pointer() {
        let mut app = demo_app();
        let many: Vec<String> = (0..60).map(|i| format!("org/repo-{i:02}")).collect();
        app.apply(crate::app::DataMsg::Repos(many));
        app.recompute_view();

        // A tall pane, so the track is long enough to drag down properly.
        let out = screen(&mut app, 120, 30);
        let x = app.panes.sidebar - 1;
        let bottom = bar_row(&out, x, '▼');
        let thumb = bar_row(&out, x, '█');

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), x, thumb));
        let mut offsets = vec![app.repos.state.offset()];
        let mut reached = None;
        for y in (thumb + 1)..bottom {
            app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), x, y));
            let out = screen(&mut app, 120, 30);
            let offset = app.repos.state.offset();
            let at = bar_row(&out, x, '█');
            offsets.push(offset);
            if offset == app.repos.len() - app.hit.repos.height as usize && reached.is_none() {
                reached = Some(y);
            }
            // Until the thumb runs out of track, its top follows the pointer.
            if reached.is_none() {
                assert_eq!(at, y, "the thumb should sit where the pointer is");
            }
        }

        assert!(reached.is_some(), "the end of the list is reachable by dragging");
        // Even, forward steps the whole way — no sticking, no jumping back.
        let steps: Vec<usize> = offsets.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(steps.iter().all(|s| *s <= 5), "no lurching: {steps:?}");
        assert!(offsets.windows(2).all(|w| w[1] >= w[0]), "never backwards: {offsets:?}");
    }

    #[test]
    fn a_narrow_terminal_drops_the_sidebar() {
        let mut app = demo_app();
        let out = screen(&mut app, 80, 14);
        println!("{out}");

        assert!(!out.contains("All repos"), "no room for a third pane");
        assert!(!app.repos.shown, "so it must not scope the runs list either");
        assert!(out.contains("Repository"));
    }

    #[test]
    fn strips_timestamp_and_bom() {
        let l = highlight_log("\u{feff}2026-06-03T09:13:47.2729296Z Hello world\r");
        assert_eq!(text_of(&l), "Hello world");
    }

    #[test]
    fn classifies_markers() {
        let e = highlight_log("2026-06-03T09:13:47.0000000Z ##[error]boom");
        assert_eq!(text_of(&e), "✗ boom");
        assert_eq!(e.spans[0].style.fg, Some(Color::Red));

        let g = highlight_log("2026-06-03T09:13:47.0000000Z ##[group]Run build");
        assert_eq!(text_of(&g), "▸ Run build");

        let end = highlight_log("2026-06-03T09:13:47.0000000Z ##[endgroup]");
        assert_eq!(text_of(&end), "");
    }

    #[test]
    fn parses_embedded_ansi() {
        // The exact pattern seen in real GitHub logs: ESC[36;1m … ESC[0m
        let l = highlight_log("2026-06-03T09:13:47.0000000Z \x1b[36;1mBUILD_NUMBER=732\x1b[0m");
        assert_eq!(text_of(&l), "BUILD_NUMBER=732");
        assert_eq!(l.spans[0].style.fg, Some(Color::Cyan));
    }
}
