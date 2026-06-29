//! All rendering. `draw` is called every frame with the current `App`. The
//! persistent layout (header, tabs, the runs/detail/jobs panes, logs, footer)
//! lives here; the floating modal overlays are in the `overlays` submodule.

mod overlays;

use crate::app::{is_error_line, log_content, App, Filter, Focus, Mode, RunnerRow, RunnerStatus};
use crate::github::{Job, Run, RunState, Step};
use ansi_to_tui::IntoText;
use chrono::{DateTime, Utc};
use overlays::*;
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
/// they already adapt; only these custom tints need a per-mode value.
#[derive(Clone, Copy)]
pub struct Theme {
    accent: Color,
    dim: Color,
    bg_sel: Color,     // selection background (focused pane)
    bg_sel_dim: Color, // selection background (unfocused pane)
    pale: Color,       // unfocused selected text
    popup_bg: Color,   // popup window fill
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
    vec![
        Span::raw("  "),
        crumb("Runs", at_runs, true),
        sep(),
        crumb("Jobs", at_jobs, at_jobs || logs),
        sep(),
        crumb("Logs", logs, logs),
    ]
}

fn draw_tabs(f: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = Filter::ALL
        .iter()
        .map(|filt| Line::from(format!(" {} ", filt.label())))
        .collect();
    let sel = Filter::ALL.iter().position(|x| *x == app.filter).unwrap_or(0);
    let tabs = Tabs::new(titles)
        .select(sel)
        .style(Style::default().fg(dim()))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(accent())
                .add_modifier(Modifier::BOLD),
        )
        .divider("");
    f.render_widget(tabs, area);
}

fn draw_body(f: &mut Frame, app: &mut App, area: Rect) {
    // The org-runners view takes over the body as its own dedicated pane.
    if app.mode == Mode::Runners {
        draw_runners_pane(f, app, area);
        return;
    }
    // Live steps open as a third pane so the run detail + jobs list stay
    // visible — you keep your place in the jobs list while watching steps.
    if app.mode == Mode::Logs && app.steps_view.is_some() {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(36),
                Constraint::Percentage(28),
                Constraint::Percentage(36),
            ])
            .split(area);
        draw_table(f, app, cols[0]);
        draw_detail(f, app, cols[1]);
        draw_steps_pane(f, app, cols[2]);
        return;
    }
    // Text logs take the full width: CI log lines are long, and the runs list
    // adds nothing while reading one job's output. Esc returns to the panes.
    if app.mode == Mode::Logs && app.logs.is_some() {
        draw_logs_pane(f, app, area);
        return;
    }
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(64), Constraint::Percentage(36)])
        .split(area);
    draw_table(f, app, cols[0]);
    draw_detail(f, app, cols[1]);
}

fn draw_table(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Runs;
    // Position indicator in the title, so long lists stay orientable.
    let title = if app.view.is_empty() {
        "Runs".to_string()
    } else {
        let at = app.table_state.selected().map_or(0, |i| i + 1);
        format!("Runs {at}/{}", app.view.len())
    };
    let content = pane(f, area, &title, focused);
    app.hit.runs = content;

    if app.view.is_empty() {
        let msg = if app.loading {
            "Loading workflow runs…"
        } else if !app.search.is_empty() {
            "No runs match your search."
        } else {
            "No runs found for this filter."
        };
        let p = Paragraph::new(msg)
            .style(Style::default().fg(dim()))
            .alignment(Alignment::Center);
        f.render_widget(p, content);
        return;
    }

    // Adaptive columns: drop the lower-value ones as the pane narrows so the
    // essentials (status, repo, workflow, age) are never pushed off-screen.
    let wide = content.width >= 100; // event + actor
    let medium = content.width >= 72; // branch + duration

    let mut head = vec![Cell::from(""), Cell::from("Repository"), Cell::from("Workflow")];
    let mut widths = vec![Constraint::Length(2), Constraint::Min(14), Constraint::Length(23)];
    if medium {
        head.push(Cell::from("Branch"));
        widths.push(Constraint::Length(16));
    }
    if wide {
        head.push(Cell::from("Event"));
        head.push(Cell::from("Actor"));
        widths.push(Constraint::Length(8));
        widths.push(Constraint::Length(12));
    }
    if medium {
        head.push(Cell::from("Dur"));
        widths.push(Constraint::Length(8));
    }
    head.push(Cell::from("Age"));
    widths.push(Constraint::Length(6));
    let header = Row::new(head).style(Style::default().fg(accent()).add_modifier(Modifier::BOLD));

    // Collected (owned), so the table can render against the real
    // `table_state` — keeping its scroll offset is what lets mouse clicks
    // map back to rows.
    let rows: Vec<Row> = app.view.iter().map(|&i| {
        let r = &app.runs[i];
        let (icon, color) = state_glyph(r.state());
        let mut cells = vec![
            Cell::from(Span::styled(icon, Style::default().fg(color))),
            Cell::from(short_repo(&r.repository.full_name)),
            // Workflow name with a dim run number, so #NNN is scannable inline.
            Cell::from(Line::from(vec![
                Span::raw(truncate(r.workflow_name(), 15)),
                Span::styled(format!("  #{}", r.run_number), Style::default().fg(dim())),
            ])),
        ];
        if medium {
            cells.push(Cell::from(truncate(r.head_branch.as_deref().unwrap_or("-"), 16)));
        }
        if wide {
            cells.push(Cell::from(event_label(&r.event)));
            cells.push(Cell::from(truncate(
                r.actor.as_ref().map(|a| a.login.as_str()).unwrap_or("-"),
                12,
            )));
        }
        if medium {
            cells.push(Cell::from(run_dur(r)).style(Style::default().fg(dim())));
        }
        cells.push(Cell::from(fmt_age(r.updated_at)).style(Style::default().fg(dim())));
        Row::new(cells)
    }).collect();

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
    if app.view.len() > viewport {
        let mut sb = ScrollbarState::new(app.view.len()).position(app.table_state.offset());
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            area.inner(Margin { vertical: 1, horizontal: 0 }),
            &mut sb,
        );
    }
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

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(9), Constraint::Min(3)])
        .split(inner);

    let (icon, color) = state_glyph(run.state());
    // Held-for-approval runs read as "queued"; call it out so `a` makes sense.
    let (label, label_color) = if run.needs_approval() {
        ("awaiting approval", Color::Yellow)
    } else {
        (state_label(run.state()), color)
    };
    let mut first = vec![
        Span::styled(format!("{icon} "), Style::default().fg(color).add_modifier(Modifier::BOLD)),
        Span::styled(label, Style::default().fg(label_color).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  #{}", run.run_number), Style::default().fg(dim())),
    ];
    if run.needs_approval() {
        first.push(Span::styled("  · press a", Style::default().fg(dim())));
    }
    let mut info = vec![
        Line::from(first),
        kv("repo", &run.repository.full_name),
        kv("flow", run.workflow_name()),
        kv("title", run.title()),
        kv("branch", run.head_branch.as_deref().unwrap_or("-")),
        kv("event", &run.event),
        kv("actor", run.actor.as_ref().map(|a| a.login.as_str()).unwrap_or("-")),
        kv("started", &fmt_dt(run.run_started_at.unwrap_or(run.created_at))),
    ];
    info.truncate(rows[0].height as usize);
    f.render_widget(Paragraph::new(info).wrap(Wrap { trim: true }), rows[0]);

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
            Constraint::Length(jobs_len.min(6).max(3) as u16),
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

    // Title + body track the selected job: a failing log is titled "Error
    // Previews · N" and lists every spot an error/fail line appears, so the pane
    // never claims "Log Preview" while showing errors.
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
                title = format!(" Error Previews · {errors} ");
                title_style = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
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

/// Build the job-log preview lines, returning them alongside the count of
/// error/fail lines found (0 → a clean log, so the caller keeps the plain
/// "Log Preview" title).
fn get_error_preview_lines(log_text: &str, conclusion: Option<&str>) -> (Vec<Line<'static>>, usize) {
    let lines: Vec<&str> = log_text.lines().collect();
    let n = lines.len();
    let mut should_include = vec![false; n];
    let mut error_count = 0usize;

    for (idx, line) in lines.iter().enumerate() {
        let content = log_content(line);
        if is_error_line(content) {
            error_count += 1;
            let start = idx.saturating_sub(3);
            let end = (idx + 3).min(n.saturating_sub(1));
            for j in start..=end {
                should_include[j] = true;
            }
        }
    }

    if error_count == 0 {
        if matches!(conclusion, Some("failure") | Some("timed_out")) {
            let start = n.saturating_sub(15);
            let mut preview = vec![Line::from(vec![
                Span::styled("No error/fail lines found. Showing end of log:", Style::default().fg(dim()))
            ])];
            for i in start..n {
                let line_num_str = format!("{:>4} │ ", i + 1);
                let highlighted = highlight_log(lines[i]);
                let mut spans = vec![Span::styled(line_num_str, Style::default().fg(dim()))];
                spans.extend(highlighted.spans);
                preview.push(Line::from(spans));
            }
            return (preview, 0);
        } else {
            return (
                vec![Line::from(vec![Span::styled(
                    "Job completed successfully (no error/fail lines found).",
                    Style::default().fg(dim()),
                )])],
                0,
            );
        }
    }

    let mut preview = Vec::new();
    let mut in_gap = false;

    for i in 0..n {
        if should_include[i] {
            if in_gap {
                preview.push(Line::from(vec![
                    Span::styled("  ...", Style::default().fg(dim()))
                ]));
                in_gap = false;
            }
            let line_num_str = format!("{:>4} │ ", i + 1);
            let highlighted = highlight_log(lines[i]);
            let mut spans = vec![Span::styled(line_num_str, Style::default().fg(dim()))];
            spans.extend(highlighted.spans);
            preview.push(Line::from(spans));
        } else {
            if !preview.is_empty() {
                in_gap = true;
            }
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

    if rv.rows.len() > list_area.height as usize {
        let mut sb = ScrollbarState::new(rv.rows.len()).position(state.offset());
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            list_area,
            &mut sb,
        );
    }

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
            Span::styled(format!("{:>8}  ", "status"), Style::default().fg(dim())),
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
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
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
    if app.mode == Mode::Runners {
        let hint = " org self-hosted runners · j/k move · ⏎ details · o open on GitHub · r refresh · Esc back";
        f.render_widget(Paragraph::new(Span::styled(hint, Style::default().fg(dim()))), area);
        return;
    }
    if app.mode == Mode::Logs && app.steps_view.is_some() {
        let hint = " live steps · updates automatically · ⏎ try logs · j/k move · Esc close";
        f.render_widget(Paragraph::new(Span::styled(hint, Style::default().fg(dim()))), area);
        return;
    }
    let hint: String = match (app.mode == Mode::Logs, app.focus) {
        (true, _) => {
            let preview_only = app.logs.as_ref().map_or(false, |lv| lv.preview_only);
            let mode_str = if preview_only { " [errors]" } else { "" };
            format!(" j/k move · ←/→ scroll · ⏎ fold · e/f all · p preview{mode_str} · / search · n/N · s save · Esc close")
        }
        (false, Focus::Runs) => {
            // Only advertise `a approve` when the selected run is actually held.
            let approve = if app.selected_run().is_some_and(|r| r.needs_approval()) {
                " · a approve"
            } else {
                ""
            };
            // Offer `v failures` once the selected run has failed.
            let fails = if app.selected_run().is_some_and(|r| r.state() == RunState::Failure) {
                " · v failures"
            } else {
                ""
            };
            format!(" j/k move · ⏎/l jobs · / search · o open · d dispatch · c cancel · x/X rerun{approve}{fails} · A artifacts · s runners · ? help · q quit")
        }
        (false, Focus::Jobs) => {
            " j/k job · ⏎/l logs · R rerun job · v failures · A artifacts · s runners · ←/Esc back · o open · ? help · q quit".into()
        }
    };
    // A kept search filter stays visible (and dismissable) while it's active.
    let hint = if app.mode != Mode::Logs && !app.search.is_empty() {
        format!(" /{} · Esc clear ·{hint}", app.search)
    } else {
        hint
    };
    f.render_widget(
        Paragraph::new(Span::styled(hint, Style::default().fg(dim()))),
        area,
    );
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

    if shown > height {
        let mut sb = ScrollbarState::new(shown).position(scroll);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            area.inner(Margin { vertical: 1, horizontal: 0 }),
            &mut sb,
        );
    }

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

fn kv(k: &str, v: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{k:>8}  "), Style::default().fg(dim())),
        Span::raw(v.to_string()),
    ])
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

/// Total wall-clock of a run: live-ticking while active, final once done.
fn run_dur(r: &Run) -> String {
    let start = r.run_started_at.unwrap_or(r.created_at);
    match r.state() {
        RunState::Running | RunState::Queued => {
            format!("{}…", fmt_dur((Utc::now() - start).num_seconds().max(0)))
        }
        _ => fmt_dur((r.updated_at - start).num_seconds().max(0)),
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
