//! Rendering for the modal overlays that float above the persistent two-pane
//! layout: the dispatch form, confirm prompt, artifacts browser, deployment
//! review picker, branch/tag picker, load-errors list, and help. They borrow
//! the palette accessors and `popup`/`centered` primitives from the parent
//! `ui` module.

use super::*;
use crate::app::{App, DispatchStage, RefKind};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

pub(super) fn draw_artifacts(f: &mut Frame, app: &App) {
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

pub(super) fn draw_approval(f: &mut Frame, app: &App) {
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

pub(super) fn draw_ref_picker(f: &mut Frame, app: &App) {
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

pub(super) fn draw_errors(f: &mut Frame, app: &App) {
    let area = centered(70, 60, f.area());
    let inner = popup(f, area, &format!("Load errors ({})", app.errors.len()), Color::Red);
    let lines: Vec<Line> = app
        .errors
        .iter()
        .map(|e| Line::from(Span::raw(format!(" • {e}"))))
        .collect();
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

pub(super) fn draw_help(f: &mut Frame) {
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

pub(super) fn draw_dispatch(f: &mut Frame, app: &App) {
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

pub(super) fn draw_confirm(f: &mut Frame, app: &App) {
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
