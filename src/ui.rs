//! All rendering. `draw` is called every frame with the current `App`.

use crate::app::{App, DispatchStage, Filter, Focus, Mode};
use crate::github::{Job, RunState, Step};
use ansi_to_tui::IntoText;
use chrono::{DateTime, Utc};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, Tabs, Wrap,
};
use ratatui::Frame;

const ACCENT: Color = Color::Rgb(137, 180, 250);
const DIM: Color = Color::Rgb(127, 132, 156);
const BG_SEL: Color = Color::Rgb(49, 50, 68);
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

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
        _ => {}
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let (running, queued, failed, success) = app.counts();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let spin = if app.loading {
        format!(" {} ", SPINNER[app.spinner])
    } else {
        "   ".into()
    };

    let line1 = Line::from(vec![
        Span::styled("  actui ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::styled(format!("@{}  ", app.user), Style::default().fg(DIM)),
        Span::styled(spin, Style::default().fg(Color::Yellow)),
        chip("●", running, Color::Yellow),
        chip("○", queued, Color::Cyan),
        chip("●", failed, Color::Red),
        chip("●", success, Color::Green),
        Span::styled(format!("  {} runs", app.runs.len()), Style::default().fg(DIM)),
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
    if let Some(rl) = &app.rate {
        let c = if rl.remaining < 200 { Color::Red } else { DIM };
        right.push(Span::styled(
            format!(" api {}/{} ", rl.remaining, rl.limit),
            Style::default().fg(c),
        ));
    }
    if let Some(ts) = app.last_refresh {
        right.push(Span::styled(
            format!(" updated {} ", fmt_age(ts)),
            Style::default().fg(DIM),
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
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else if reached {
            Style::default().fg(Color::White)
        } else {
            Style::default().fg(DIM)
        };
        Span::styled(label, style)
    };
    let sep = || Span::styled(" › ", Style::default().fg(DIM));
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
        .style(Style::default().fg(DIM))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(ACCENT)
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
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if focused { ACCENT } else { DIM }))
        .title(Span::styled(" Runs ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)));

    if app.view.is_empty() {
        let msg = if app.loading {
            "Loading workflow runs…"
        } else if !app.search.is_empty() {
            "No runs match your search."
        } else {
            "No runs found for this filter."
        };
        let p = Paragraph::new(msg)
            .style(Style::default().fg(DIM))
            .alignment(Alignment::Center)
            .block(block);
        f.render_widget(p, area);
        return;
    }

    let header = Row::new(vec![
        Cell::from(""),
        Cell::from("Repository"),
        Cell::from("Workflow"),
        Cell::from("Branch"),
        Cell::from("Event"),
        Cell::from("Actor"),
        Cell::from("Age"),
    ])
    .style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD));

    let rows = app.view.iter().map(|&i| {
        let r = &app.runs[i];
        let (icon, color) = state_glyph(r.state());
        Row::new(vec![
            Cell::from(Span::styled(icon, Style::default().fg(color))),
            Cell::from(short_repo(&r.repository.full_name)),
            Cell::from(truncate(r.workflow_name(), 22)),
            Cell::from(truncate(r.head_branch.as_deref().unwrap_or("-"), 18)),
            Cell::from(truncate(&r.event, 11)),
            Cell::from(truncate(
                r.actor.as_ref().map(|a| a.login.as_str()).unwrap_or("-"),
                14,
            )),
            Cell::from(fmt_age(r.updated_at)).style(Style::default().fg(DIM)),
        ])
    });

    let widths = [
        Constraint::Length(2),
        Constraint::Min(16),
        Constraint::Length(22),
        Constraint::Length(18),
        Constraint::Length(11),
        Constraint::Length(14),
        Constraint::Length(7),
    ];

    // Only the focused pane highlights its selection; the other shows none
    // (dimming a single row reads as "disabled").
    let hl = if focused {
        Style::default().bg(BG_SEL).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .row_highlight_style(hl)
        .highlight_symbol(if focused { "▌" } else { " " });

    let mut state = app.table_state.clone();
    f.render_stateful_widget(table, area, &mut state);
}

fn draw_detail(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Jobs;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if focused { ACCENT } else { DIM }))
        .title(Span::styled(" Detail ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(run) = app.selected_run() else {
        f.render_widget(
            Paragraph::new("Select a run to see details.").style(Style::default().fg(DIM)),
            inner,
        );
        return;
    };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(9), Constraint::Min(3)])
        .split(inner);

    let (icon, color) = state_glyph(run.state());
    let mut info = vec![
        Line::from(vec![
            Span::styled(format!("{icon} "), Style::default().fg(color).add_modifier(Modifier::BOLD)),
            Span::styled(state_label(run.state()), Style::default().fg(color).add_modifier(Modifier::BOLD)),
            Span::styled(format!("  #{}", run.run_number), Style::default().fg(DIM)),
        ]),
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
    let title = if focused { " Jobs · ⏎ logs " } else { " Jobs " };
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(if focused { ACCENT } else { DIM }))
        .title(Span::styled(title, Style::default().fg(ACCENT)));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.jobs.is_empty() {
        f.render_widget(
            Paragraph::new("Loading jobs…").style(Style::default().fg(DIM)),
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
                Span::styled(format!("  {}", job_dur(j)), Style::default().fg(DIM)),
            ]))
        })
        .collect();

    let hl = if focused {
        Style::default().bg(BG_SEL).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let list = List::new(items)
        .highlight_style(hl)
        .highlight_symbol(if focused { "▌" } else { " " });
    let mut state = app.jobs_state.clone();
    f.render_stateful_widget(list, inner, &mut state);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    // The runs search prompt always wins while you're typing in it.
    if app.mode == Mode::Search {
        let line = Line::from(vec![
            Span::styled(" /", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::raw(app.search.clone()),
            Span::styled("▏", Style::default().fg(ACCENT)),
            Span::styled("  (Esc clear · Enter keep)", Style::default().fg(DIM)),
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
        f.render_widget(Paragraph::new(Span::styled(hint, Style::default().fg(DIM))), area);
        return;
    }
    let hint = match (app.mode == Mode::Logs, app.focus) {
        (true, _) => " j/k move · ⏎ fold · e/f expand/fold all · / search · n/N next/prev · Esc close",
        (false, Focus::Runs) => {
            " j/k move · ⏎/l jobs · 1-5 filter · / search · o open · d dispatch · c cancel · x/X rerun · r refresh · ? help · q quit"
        }
        (false, Focus::Jobs) => {
            " j/k job · ⏎/l logs · ←/Esc back · o open run · r refresh · ? help · q quit"
        }
    };
    f.render_widget(
        Paragraph::new(Span::styled(hint, Style::default().fg(DIM))),
        area,
    );
}

// -- overlays ---------------------------------------------------------------

fn draw_help(f: &mut Frame) {
    let area = centered(60, 70, f.area());
    f.render_widget(Clear, area);
    let block = popup_block(" Help ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let body = Text::from(vec![
        hl("Panes  (two-pane: Runs ⟷ Jobs)"),
        help_row("Tab", "switch focus between Runs and Jobs"),
        help_row("→ / l", "focus Jobs (drill into selected run)"),
        help_row("← / h / Esc", "focus Runs"),
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
        help_row("Enter / l", "view logs of selected job (l works anywhere)"),
        help_row("o", "open run in browser"),
        help_row("d", "dispatch a workflow (workflow_dispatch)"),
        help_row("c", "cancel the selected run"),
        help_row("x / X", "re-run failed jobs / re-run all"),
        help_row("r / F5", "refresh now (auto-refresh is on)"),
        Line::raw(""),
        hl("Logs view"),
        help_row("j / k", "move cursor"),
        help_row("Enter / Space", "fold / unfold step at cursor (shows duration)"),
        help_row("e / f", "expand all / fold all steps"),
        help_row("/", "search logs (auto-expands folded hits)"),
        help_row("n / N", "next / previous match"),
        Line::raw(""),
        help_row("?", "toggle this help"),
        help_row("q / Ctrl-C", "quit"),
    ]);
    f.render_widget(Paragraph::new(body), inner);
}

fn draw_logs_pane(f: &mut Frame, app: &App, area: Rect) {
    let Some(lv) = &app.logs else { return };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .title(Span::styled(
            format!(" Logs · {} ", truncate(&lv.title, area.width.saturating_sub(12) as usize)),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

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
                let acc = Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);
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
                    spans.push(Span::styled(meta, Style::default().fg(DIM)));
                }
                Line::from(spans)
            } else if !lv.search.is_empty() && lv.is_match(src) {
                highlight_match(&lv.lines[src], &lv.search)
            } else {
                highlight_log(&lv.lines[src])
            };
            // Cursor gutter + tree connector for lines inside a step.
            let gutter = if row == lv.cursor {
                Span::styled("▌", Style::default().fg(ACCENT))
            } else {
                Span::raw(" ")
            };
            let mut spans = vec![gutter, Span::raw(" ")];
            if in_group && !lv.is_header[src] {
                spans.push(Span::styled("│ ", Style::default().fg(DIM)));
            }
            spans.extend(line.spans);
            Line::from(spans)
        })
        .collect();
    f.render_widget(Paragraph::new(text), body);

    let by = Rect { x: inner.x, y: inner.y + inner.height - 1, width: inner.width, height: 1 };
    if lv.searching {
        // Live search prompt.
        let line = Line::from(vec![
            Span::styled(" /", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw(lv.search.clone()),
            Span::styled("▏", Style::default().fg(Color::Yellow)),
            Span::styled(
                format!("  {} matches  (Enter keep · Esc cancel)", lv.matches.len()),
                Style::default().fg(DIM),
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
        let bar = format!(" {pos}/{shown} · j/k{folds} · / search{search} · Esc close ");
        f.render_widget(
            Paragraph::new(Span::styled(bar, Style::default().fg(DIM))).alignment(Alignment::Right),
            by,
        );
    }
}

/// Live step view for a running job (text logs aren't available yet).
fn draw_steps_pane(f: &mut Frame, app: &App, area: Rect) {
    let Some(sv) = &app.steps_view else { return };
    let steps = app.steps_view_steps();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .title(Span::styled(
            format!(" Live steps · {} ", truncate(&sv.job_name, area.width.saturating_sub(16) as usize)),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let body = Rect { height: inner.height.saturating_sub(1), ..inner };
    if steps.is_empty() {
        f.render_widget(
            Paragraph::new("Waiting for the job to start…").style(Style::default().fg(DIM)),
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
                    Span::styled("▌", Style::default().fg(ACCENT))
                } else {
                    Span::raw(" ")
                };
                Line::from(vec![
                    gutter,
                    Span::styled(format!(" {icon} "), Style::default().fg(color)),
                    Span::styled(s.name.clone(), name_style),
                    Span::styled(format!("  {}", step_dur(s)), Style::default().fg(DIM)),
                ])
            })
            .collect();
        f.render_widget(Paragraph::new(lines), body);
    }

    let by = Rect { x: inner.x, y: inner.y + inner.height - 1, width: inner.width, height: 1 };
    let bar = " live · full logs load when the job finishes · j/k · Esc close ";
    f.render_widget(
        Paragraph::new(Span::styled(bar, Style::default().fg(DIM))).alignment(Alignment::Right),
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
            Some("cancelled") => ("◌", DIM),
            Some("skipped") => ("○", DIM),
            _ => ("·", DIM),
        },
        _ => ("·", DIM),
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
    f.render_widget(Clear, area);
    let block = popup_block(&format!(" Dispatch · {} ", d.repo));
    let inner = block.inner(area);
    f.render_widget(block, area);

    match d.stage {
        DispatchStage::SelectWorkflow => {
            if d.workflows.is_empty() {
                f.render_widget(
                    Paragraph::new("Loading workflows…").style(Style::default().fg(DIM)),
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
                    let c = if active { Color::Green } else { DIM };
                    ListItem::new(Line::from(vec![
                        Span::styled(format!("{dot} "), Style::default().fg(c)),
                        Span::raw(w.name.clone()),
                        Span::styled(format!("  {}", w.path), Style::default().fg(DIM)),
                    ]))
                })
                .collect();
            let list = List::new(items)
                .highlight_style(Style::default().bg(BG_SEL).add_modifier(Modifier::BOLD))
                .highlight_symbol("▌")
                .block(Block::default().title(Span::styled(
                    " Pick a workflow · ⏎ next · Esc cancel ",
                    Style::default().fg(DIM),
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
            Paragraph::new("Loading inputs…").style(Style::default().fg(DIM)),
            area,
        );
        return;
    }

    // Highlight for the focused field's value.
    let val_style = |on: bool| {
        if on {
            Style::default().fg(Color::Black).bg(ACCENT)
        } else {
            Style::default().fg(Color::White).bg(BG_SEL)
        }
    };

    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled("workflow  ", Style::default().fg(DIM)),
            Span::styled(name.to_string(), Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::raw(""),
    ];

    // Field 0: ref.
    lines.push(Line::from(Span::styled("ref (branch / tag / sha)", Style::default().fg(DIM))));
    lines.push(Line::from(Span::styled(format!(" {} ", d.git_ref), val_style(d.field_idx == 0))));
    lines.push(Line::raw(""));

    if !d.dispatchable {
        lines.push(Line::from(Span::styled(
            "⚠ This workflow has no workflow_dispatch trigger and can't be run manually.",
            Style::default().fg(Color::Red),
        )));
    } else if d.fields.is_empty() {
        lines.push(Line::from(Span::styled("(no inputs)", Style::default().fg(DIM))));
    } else {
        for (i, field) in d.fields.iter().enumerate() {
            let focused = d.field_idx == i + 1;
            let mut label = vec![Span::styled(field.name.clone(), Style::default().fg(DIM))];
            if field.required {
                label.push(Span::styled(" *", Style::default().fg(Color::Red)));
            }
            if !field.description.is_empty() {
                label.push(Span::styled(
                    format!("  — {}", field.description),
                    Style::default().fg(DIM),
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
                        Span::styled("  (Space toggles)", Style::default().fg(DIM)),
                    ])
                }
                FieldKind::Choice { options, idx } => {
                    let cur = options.get(*idx).map(|s| s.as_str()).unwrap_or("");
                    Line::from(vec![
                        Span::styled(format!(" ‹ {cur} › "), val_style(focused)),
                        Span::styled(
                            format!("  ({}/{}, ←/→)", idx + 1, options.len()),
                            Style::default().fg(DIM),
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
        Style::default().fg(DIM),
    )));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn draw_confirm(f: &mut Frame, app: &App) {
    let Some(a) = &app.pending_action else { return };
    let area = centered(50, 20, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow))
        .title(Span::styled(" Confirm ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)));
    let inner = block.inner(area);
    f.render_widget(block, area);
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

fn popup_block(title: &str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .title(Span::styled(
            title.to_string(),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
}

fn kv(k: &str, v: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{k:>8}  "), Style::default().fg(DIM)),
        Span::raw(v.to_string()),
    ])
}

fn hl(s: &str) -> Line<'static> {
    Line::from(Span::styled(
        s.to_string(),
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
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
        RunState::Cancelled => ("◌", DIM),
        RunState::Skipped => ("○", DIM),
        RunState::Other => ("·", DIM),
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
            Some("cancelled") => ("◌", DIM),
            Some("skipped") => ("○", DIM),
            _ => ("·", DIM),
        },
        _ => ("·", DIM),
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
        return marker("▸ ", rest, ACCENT, true);
    }
    if content.starts_with("##[endgroup]") {
        return Line::raw("");
    }
    if let Some(rest) = content.strip_prefix("##[command]") {
        return marker("$ ", rest, Color::Magenta, false);
    }
    if let Some(rest) = content.strip_prefix("##[debug]") {
        return marker("", rest, DIM, false);
    }
    if let Some(rest) = content.strip_prefix("##[section]") {
        return marker("", rest, ACCENT, true);
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
