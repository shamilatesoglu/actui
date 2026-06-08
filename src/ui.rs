//! All rendering. `draw` is called every frame with the current `App`.

use crate::app::{App, DispatchStage, Filter, Focus, Mode, RefKind};
use crate::github::{Job, Run, RunState, Step};
use ansi_to_tui::IntoText;
use chrono::{DateTime, Utc};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, Tabs, Wrap,
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

pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Length(1), // tabs
            Constraint::Min(3),    // body
            Constraint::Length(1), // footer
        ])
        .split(f.area());

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
        Mode::RefPicker => draw_ref_picker(f, app),
        _ => {}
    }
}

fn draw_artifacts(f: &mut Frame, app: &App) {
    let Some(av) = &app.artifacts else { return };
    let area = centered(60, 60, f.area());
    let inner = popup(f, area, &format!("Artifacts · {}", av.repo), accent());

    if !av.loaded {
        f.render_widget(
            Paragraph::new("Loading artifacts…").style(Style::default().fg(dim())),
            inner,
        );
        return;
    }
    if av.items.is_empty() {
        f.render_widget(
            Paragraph::new("No artifacts for this run.").style(Style::default().fg(dim())),
            inner,
        );
        return;
    }

    let items: Vec<ListItem> = av
        .items
        .iter()
        .map(|a| {
            let (note, c) = if a.expired {
                (" (expired)".to_string(), Color::Red)
            } else {
                (format!("  {}", fmt_bytes(a.size_in_bytes)), dim())
            };
            ListItem::new(Line::from(vec![
                Span::raw(a.name.clone()),
                Span::styled(note, Style::default().fg(c)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .highlight_style(Style::default().bg(bg_sel()).add_modifier(Modifier::BOLD))
        .highlight_symbol("▌")
        .block(Block::default().title(Span::styled(
            " ⏎ download (.zip) · j/k move · Esc close ",
            Style::default().fg(dim()),
        )));
    let mut state = av.state.clone();
    f.render_stateful_widget(list, inner, &mut state);
}

fn draw_approval(f: &mut Frame, app: &App) {
    let Some(av) = &app.approval else { return };
    let area = centered(64, 60, f.area());
    let inner = popup(f, area, &format!("Review deployment · {}", av.repo), Color::Yellow);

    if !av.loaded {
        f.render_widget(
            Paragraph::new("Loading pending deployments…").style(Style::default().fg(dim())),
            inner,
        );
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1), Constraint::Length(1)])
        .split(inner);

    let items: Vec<ListItem> = av
        .items
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let approvable = p.current_user_can_approve;
            let checked = av.selected.contains(&i);
            let (mark, mark_c) = if !approvable {
                ("[-]", dim())
            } else if checked {
                ("[x]", Color::Green)
            } else {
                ("[ ]", dim())
            };
            let mut spans = vec![
                Span::styled(format!("{mark} "), Style::default().fg(mark_c)),
                Span::raw(p.environment.name.clone()),
            ];
            if !approvable {
                spans.push(Span::styled("  — no review access", Style::default().fg(Color::Red)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .highlight_style(select_style(true))
        .highlight_symbol("▌");
    let mut state = av.state.clone();
    f.render_stateful_widget(list, rows[0], &mut state);

    // Comment line (editable with `c`).
    let comment = if av.comment.is_empty() && !av.editing_comment {
        Line::from(Span::styled(" comment: (press c to add)", Style::default().fg(dim())))
    } else {
        let mut s = vec![
            Span::styled(" comment: ", Style::default().fg(dim())),
            Span::raw(av.comment.clone()),
        ];
        if av.editing_comment {
            s.push(Span::styled("▏", Style::default().fg(Color::Yellow)));
        }
        Line::from(s)
    };
    f.render_widget(Paragraph::new(comment), rows[1]);

    let hint = if av.editing_comment {
        " typing comment… · Enter/Esc done "
    } else {
        " Space toggle · ⏎/y approve · x reject · c comment · Esc cancel "
    };
    f.render_widget(
        Paragraph::new(Span::styled(hint, Style::default().fg(dim()))).alignment(Alignment::Right),
        rows[2],
    );
}

fn draw_ref_picker(f: &mut Frame, app: &App) {
    let Some(rp) = &app.ref_picker else { return };
    let area = centered(50, 70, f.area());
    let inner = popup(f, area, &format!("Pick ref · {}", rp.repo), accent());

    if !rp.loaded {
        f.render_widget(
            Paragraph::new("Loading branches & tags…").style(Style::default().fg(dim())),
            inner,
        );
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(3), Constraint::Length(1)])
        .split(inner);

    // Filter prompt.
    let filt = Line::from(vec![
        Span::styled(" /", Style::default().fg(accent()).add_modifier(Modifier::BOLD)),
        Span::raw(rp.filter.clone()),
        Span::styled("▏", Style::default().fg(accent())),
        Span::styled(
            format!("  {} match{}", rp.view.len(), if rp.view.len() == 1 { "" } else { "es" }),
            Style::default().fg(dim()),
        ),
    ]);
    f.render_widget(Paragraph::new(filt), rows[0]);

    let items: Vec<ListItem> = rp
        .view
        .iter()
        .filter_map(|&i| rp.items.get(i))
        .map(|r| {
            let (tag, c) = match r.kind {
                RefKind::Branch => ("br ", accent()),
                RefKind::Tag => ("tag", Color::Magenta),
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{tag} "), Style::default().fg(c)),
                Span::raw(r.name.clone()),
            ]))
        })
        .collect();
    let list = List::new(items)
        .highlight_style(select_style(true))
        .highlight_symbol("▌");
    let mut state = rp.state.clone();
    f.render_stateful_widget(list, rows[1], &mut state);

    f.render_widget(
        Paragraph::new(Span::styled(
            " type to filter · ↑/↓ move · ⏎ select · Esc cancel ",
            Style::default().fg(dim()),
        ))
        .alignment(Alignment::Right),
        rows[2],
    );
}

fn draw_errors(f: &mut Frame, app: &App) {
    let area = centered(70, 60, f.area());
    let inner = popup(f, area, &format!("Load errors ({})", app.errors.len()), Color::Red);
    let lines: Vec<Line> = app
        .errors
        .iter()
        .map(|e| Line::from(Span::raw(format!(" • {e}"))))
        .collect();
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
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
        right.push(Span::styled(
            format!(" api {}/{} ", rl.remaining, rl.limit),
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

fn draw_body(f: &mut Frame, app: &App, area: Rect) {
    // Give the detail pane more room while it's showing logs.
    let detail = if app.mode == Mode::Logs { 55 } else { 36 };
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(100 - detail), Constraint::Percentage(detail)])
        .split(area);
    draw_table(f, app, cols[0]);
    if app.mode == Mode::Logs && app.steps_view.is_some() {
        draw_steps_pane(f, app, cols[1]);
    } else if app.mode == Mode::Logs && app.logs.is_some() {
        draw_logs_pane(f, app, cols[1]);
    } else {
        draw_detail(f, app, cols[1]);
    }
}

fn draw_table(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Runs;
    let content = pane(f, area, "Runs", focused);

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

    let header = Row::new(vec![
        Cell::from(""),
        Cell::from("Repository"),
        Cell::from("Workflow"),
        Cell::from("Branch"),
        Cell::from("Event"),
        Cell::from("Actor"),
        Cell::from("Dur"),
        Cell::from("Age"),
    ])
    .style(Style::default().fg(accent()).add_modifier(Modifier::BOLD));

    let rows = app.view.iter().map(|&i| {
        let r = &app.runs[i];
        let (icon, color) = state_glyph(r.state());
        Row::new(vec![
            Cell::from(Span::styled(icon, Style::default().fg(color))),
            Cell::from(short_repo(&r.repository.full_name)),
            // Workflow name with a dim run number, so #NNN is scannable inline.
            Cell::from(Line::from(vec![
                Span::raw(truncate(r.workflow_name(), 15)),
                Span::styled(format!("  #{}", r.run_number), Style::default().fg(dim())),
            ])),
            Cell::from(truncate(r.head_branch.as_deref().unwrap_or("-"), 16)),
            Cell::from(event_label(&r.event)),
            Cell::from(truncate(
                r.actor.as_ref().map(|a| a.login.as_str()).unwrap_or("-"),
                12,
            )),
            Cell::from(run_dur(r)).style(Style::default().fg(dim())),
            Cell::from(fmt_age(r.updated_at)).style(Style::default().fg(dim())),
        ])
    });

    let widths = [
        Constraint::Length(2),
        Constraint::Min(14),
        Constraint::Length(23),
        Constraint::Length(16),
        Constraint::Length(8),
        Constraint::Length(12),
        Constraint::Length(8),
        Constraint::Length(6),
    ];

    // Both panes show their selection; the unfocused one dims it (lazyactions).
    let hl = select_style(focused);
    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(hl)
        .highlight_symbol(if focused { "▌" } else { " " });

    let mut state = app.table_state.clone();
    f.render_stateful_widget(table, content, &mut state);
}

fn draw_detail(f: &mut Frame, app: &App, area: Rect) {
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

fn draw_jobs(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Jobs;
    // Divider rule between the run info and the jobs list within the Detail pane.
    let title = if focused { " Jobs · ⏎ logs " } else { " Jobs " };
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(if focused { accent() } else { dim() }))
        .title(pane_title(title.trim(), focused));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.jobs.is_empty() {
        f.render_widget(
            Paragraph::new("Loading jobs…").style(Style::default().fg(dim())),
            inner,
        );
        return;
    }

    let items: Vec<ListItem> = app
        .jobs
        .iter()
        .map(|j| {
            let (icon, color) = job_glyph(j);
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
    let mut state = app.jobs_state.clone();
    f.render_stateful_widget(list, inner, &mut state);
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
    if app.mode == Mode::Logs && app.steps_view.is_some() {
        let hint = " live steps · updates automatically · ⏎ try logs · j/k move · Esc close";
        f.render_widget(Paragraph::new(Span::styled(hint, Style::default().fg(dim()))), area);
        return;
    }
    let hint: String = match (app.mode == Mode::Logs, app.focus) {
        (true, _) => " j/k move · ←/→ scroll · ⏎ fold · e/f all · / search · n/N · s save · Esc close".into(),
        (false, Focus::Runs) => {
            // Only advertise `a approve` when the selected run is actually held.
            let approve = if app.selected_run().is_some_and(|r| r.needs_approval()) {
                " · a approve"
            } else {
                ""
            };
            format!(" j/k move · ⏎/l jobs · / search · o open · d dispatch · c cancel · x/X rerun{approve} · A artifacts · ? help · q quit")
        }
        (false, Focus::Jobs) => {
            " j/k job · ⏎/l logs · R rerun job · A artifacts · ←/Esc back · o open · ? help · q quit".into()
        }
    };
    f.render_widget(
        Paragraph::new(Span::styled(hint, Style::default().fg(dim()))),
        area,
    );
}

// -- overlays ---------------------------------------------------------------

fn draw_help(f: &mut Frame) {
    let area = centered(60, 70, f.area());
    let inner = popup(f, area, "Help", accent());
    let body = Text::from(vec![
        hl("Panes  (two-pane: Runs ⟷ Jobs)"),
        help_row("Tab", "switch focus between Runs and Jobs"),
        help_row("→ / l", "focus Jobs (drill into selected run)"),
        help_row("← / h / Bksp / Esc", "focus Runs (Bksp/Esc go back anywhere)"),
        help_row("Enter", "Runs: drill to jobs · Jobs: view logs"),
        Line::raw(""),
        hl("Navigation  (acts on the focused pane)"),
        help_row("j / k, ↑ / ↓", "move selection"),
        help_row("g / G", "jump to top / bottom"),
        help_row("PgUp / PgDn", "page up / down"),
        Line::raw(""),
        hl("Filter & search"),
        help_row("1 - 5", "All / Running / Queued / Failed / Success"),
        help_row("[ / ]", "cycle status filter"),
        help_row("/", "fuzzy search (repo, workflow, branch)"),
        Line::raw(""),
        hl("Actions"),
        help_row("Enter / l / L", "view logs of selected job (L works anywhere)"),
        help_row("o", "open in browser (focused job's page, else the run)"),
        help_row("d", "dispatch a workflow (workflow_dispatch)"),
        help_row("c", "cancel the selected run"),
        help_row("x / X", "re-run failed jobs / re-run all"),
        help_row("R", "re-run the selected job"),
        help_row("a", "approve a held run (fork-PR or environment deployment)"),
        help_row("  ↳ env review", "Space pick env · c comment · ⏎ approve · x reject"),
        help_row("  ↳ dispatch ref", "Space / → on the ref field to pick a branch or tag"),
        help_row("A", "browse / download run artifacts"),
        help_row("r / F5", "refresh now (auto-refresh is on)"),
        help_row("E", "show repos that failed to load"),
        Line::raw(""),
        hl("Notifications"),
        help_row("(auto)", "bell + desktop toast when a watched run finishes"),
        Line::raw(""),
        hl("Logs view"),
        help_row("j / k", "move cursor"),
        help_row("← / →", "scroll horizontally"),
        help_row("Enter / Space", "fold / unfold step at cursor (shows duration)"),
        help_row("e / f", "expand all / fold all steps"),
        help_row("/", "search logs (auto-expands folded hits)"),
        help_row("n / N", "next / previous match"),
        help_row("s", "save the log to a file"),
        Line::raw(""),
        help_row("?", "toggle this help"),
        help_row("q / Ctrl-C", "quit"),
    ]);
    f.render_widget(Paragraph::new(body), inner);
}

fn draw_logs_pane(f: &mut Frame, app: &App, area: Rect) {
    let Some(lv) = &app.logs else { return };
    let title = format!("Logs · {}", truncate(&lv.title, area.width.saturating_sub(10) as usize));
    let inner = pane(f, area, &title, true);

    // Reserve the last row for a status indicator.
    let body = Rect { height: inner.height.saturating_sub(1), ..inner };
    let height = body.height as usize;
    let shown = lv.visible.len();

    // Center the cursor in the viewport (pure function of cursor + sizes).
    let max_scroll = shown.saturating_sub(height);
    let scroll = lv.cursor.saturating_sub(height / 2).min(max_scroll);

    let text: Vec<Line> = lv
        .visible
        .iter()
        .enumerate()
        .skip(scroll)
        .take(height)
        .map(|(row, &src)| {
            let in_group = lv.line_group[src].is_some();
            let line = if lv.is_header[src] {
                // Step node: ▸/▾ arrow, name, line count + duration.
                let g = lv.line_group[src].unwrap();
                let collapsed = lv.groups[g].collapsed;
                let arrow = if collapsed { "▸" } else { "▾" };
                let name = strip_ts(
                    lv.lines[src]
                        .trim_start_matches('\u{feff}')
                        .trim_end_matches(['\r', '\n']),
                )
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
        let folds = if lv.has_groups() { " · ⏎ fold · e/f all" } else { "" };
        let hs = if lv.hscroll > 0 { format!(" · →{}", lv.hscroll) } else { String::new() };
        let bar = format!(" {pos}/{shown} · j/k{folds} · / search{search} · s save{hs} · Esc close ");
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
        let max_scroll = steps.len().saturating_sub(height);
        let scroll = sv.cursor.saturating_sub(height / 2).min(max_scroll);
        let lines: Vec<Line> = steps
            .iter()
            .enumerate()
            .skip(scroll)
            .take(height)
            .map(|(i, s)| {
                let (icon, color) = step_glyph(s);
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
    let bar = " live · full logs load when the job finishes · j/k · Esc close ";
    f.render_widget(
        Paragraph::new(Span::styled(bar, Style::default().fg(dim()))).alignment(Alignment::Right),
        by,
    );
}

fn step_glyph(s: &Step) -> (&'static str, Color) {
    match s.status.as_str() {
        "in_progress" => ("●", Color::Yellow),
        "queued" | "waiting" | "pending" => ("○", Color::Cyan),
        "completed" => match s.conclusion.as_deref() {
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
    let content = strip_ts(raw.trim_start_matches('\u{feff}').trim_end_matches(['\r', '\n']));
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

fn draw_dispatch(f: &mut Frame, app: &App) {
    let Some(d) = &app.dispatch else { return };
    let area = centered(70, 70, f.area());
    let inner = popup(f, area, &format!("Dispatch · {}", d.repo), accent());

    match d.stage {
        DispatchStage::SelectWorkflow => {
            if d.workflows.is_empty() {
                f.render_widget(
                    Paragraph::new("Loading workflows…").style(Style::default().fg(dim())),
                    inner,
                );
                return;
            }
            let items: Vec<ListItem> = d
                .workflows
                .iter()
                .map(|w| {
                    let active = w.state == "active";
                    let dot = if active { "●" } else { "○" };
                    let c = if active { Color::Green } else { dim() };
                    ListItem::new(Line::from(vec![
                        Span::styled(format!("{dot} "), Style::default().fg(c)),
                        Span::raw(w.name.clone()),
                        Span::styled(format!("  {}", w.path), Style::default().fg(dim())),
                    ]))
                })
                .collect();
            let list = List::new(items)
                .highlight_style(Style::default().bg(bg_sel()).add_modifier(Modifier::BOLD))
                .highlight_symbol("▌")
                .block(Block::default().title(Span::styled(
                    " Pick a workflow · ⏎ next · Esc cancel ",
                    Style::default().fg(dim()),
                )));
            let mut state = d.wf_state.clone();
            f.render_stateful_widget(list, inner, &mut state);
        }
        DispatchStage::EditParams => draw_dispatch_form(f, d, inner),
    }
}

fn draw_dispatch_form(f: &mut Frame, d: &crate::app::DispatchState, area: Rect) {
    use crate::app::FieldKind;
    let wf = d.wf_state.selected().and_then(|i| d.workflows.get(i));
    let name = wf.map(|w| w.name.as_str()).unwrap_or("?");

    if !d.loaded {
        f.render_widget(
            Paragraph::new("Loading inputs…").style(Style::default().fg(dim())),
            area,
        );
        return;
    }

    // Highlight for the focused field's value.
    let val_style = |on: bool| {
        if on {
            Style::default().fg(Color::Black).bg(accent())
        } else {
            Style::default().fg(Color::White).bg(bg_sel())
        }
    };

    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled("workflow  ", Style::default().fg(dim())),
            Span::styled(name.to_string(), Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::raw(""),
    ];

    // Field 0: ref.
    let ref_focused = d.field_idx == 0;
    let mut ref_label = vec![Span::styled("ref (branch / tag / sha)", Style::default().fg(dim()))];
    if ref_focused {
        ref_label.push(Span::styled("  — Space/→ to pick", Style::default().fg(accent())));
    }
    lines.push(Line::from(ref_label));
    lines.push(Line::from(vec![
        Span::styled(format!(" {} ", d.git_ref), val_style(ref_focused)),
        Span::styled(" ▾", Style::default().fg(if ref_focused { accent() } else { dim() })),
    ]));
    lines.push(Line::raw(""));

    if !d.dispatchable {
        lines.push(Line::from(Span::styled(
            "⚠ This workflow has no workflow_dispatch trigger and can't be run manually.",
            Style::default().fg(Color::Red),
        )));
    } else if d.fields.is_empty() {
        lines.push(Line::from(Span::styled("(no inputs)", Style::default().fg(dim()))));
    } else {
        for (i, field) in d.fields.iter().enumerate() {
            let focused = d.field_idx == i + 1;
            let mut label = vec![Span::styled(field.name.clone(), Style::default().fg(dim()))];
            if field.required {
                label.push(Span::styled(" *", Style::default().fg(Color::Red)));
            }
            if !field.description.is_empty() {
                label.push(Span::styled(
                    format!("  — {}", field.description),
                    Style::default().fg(dim()),
                ));
            }
            lines.push(Line::from(label));
            let value_line = match &field.kind {
                FieldKind::Text { value, .. } => {
                    Line::from(Span::styled(format!(" {value} "), val_style(focused)))
                }
                FieldKind::Bool(b) => {
                    let mark = if *b { "[x] true" } else { "[ ] false" };
                    Line::from(vec![
                        Span::styled(format!(" {mark} "), val_style(focused)),
                        Span::styled("  (Space toggles)", Style::default().fg(dim())),
                    ])
                }
                FieldKind::Choice { options, idx } => {
                    let cur = options.get(*idx).map(|s| s.as_str()).unwrap_or("");
                    Line::from(vec![
                        Span::styled(format!(" ‹ {cur} › "), val_style(focused)),
                        Span::styled(
                            format!("  ({}/{}, ←/→)", idx + 1, options.len()),
                            Style::default().fg(dim()),
                        ),
                    ])
                }
            };
            lines.push(value_line);
            lines.push(Line::raw(""));
        }
    }

    lines.push(Line::from(Span::styled(
        "↑/↓ field · type/Space/←→ edit · ⏎ dispatch · Esc back",
        Style::default().fg(dim()),
    )));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn draw_confirm(f: &mut Frame, app: &App) {
    let Some(a) = &app.pending_action else { return };
    let area = centered(50, 20, f.area());
    let inner = popup(f, area, "Confirm", Color::Yellow);
    let body = Text::from(vec![
        Line::raw(""),
        Line::from(a.prompt()).alignment(Alignment::Center),
        Line::raw(""),
        Line::from(vec![
            Span::styled("  [y] yes  ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::styled("  [n] no  ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        ])
        .alignment(Alignment::Center),
    ]);
    f.render_widget(Paragraph::new(body), inner);
}

// -- helpers ----------------------------------------------------------------

fn kv(k: &str, v: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{k:>8}  "), Style::default().fg(dim())),
        Span::raw(v.to_string()),
    ])
}

fn hl(s: &str) -> Line<'static> {
    Line::from(Span::styled(
        s.to_string(),
        Style::default().fg(accent()).add_modifier(Modifier::BOLD),
    ))
}

fn help_row(keys: &str, desc: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {keys:<14}"), Style::default().fg(Color::Yellow)),
        Span::raw(desc.to_string()),
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

fn job_glyph(j: &Job) -> (&'static str, Color) {
    match j.status.as_str() {
        "in_progress" => ("●", Color::Yellow),
        "queued" | "waiting" | "pending" => ("○", Color::Cyan),
        "completed" => match j.conclusion.as_deref() {
            Some("success") => ("●", Color::Green),
            Some("failure") | Some("timed_out") => ("●", Color::Red),
            Some("cancelled") => ("◌", dim()),
            Some("skipped") => ("○", dim()),
            _ => ("·", dim()),
        },
        _ => ("·", dim()),
    }
}

fn job_dur(j: &Job) -> String {
    match (j.started_at, j.completed_at) {
        (Some(s), Some(e)) => fmt_dur((e - s).num_seconds().max(0)),
        (Some(s), None) => format!("{}…", fmt_dur((Utc::now() - s).num_seconds().max(0))),
        _ => String::new(),
    }
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

/// GitHub job logs prefix every line with an ISO timestamp; drop it for display.
fn strip_ts(line: &str) -> &str {
    if let Some((first, rest)) = line.split_once(' ') {
        if first.len() >= 20 && first.contains('T') && first.contains(':') {
            return rest;
        }
    }
    line
}

/// Colorize one raw log line: GitHub `##[…]` workflow markers, then any
/// embedded ANSI escape sequences the build tools emitted.
fn highlight_log(raw: &str) -> Line<'static> {
    let line = raw
        .trim_end_matches(['\r', '\n'])
        .trim_start_matches('\u{feff}'); // strip CR/LF and a leading UTF-8 BOM
    let content = strip_ts(line);

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
