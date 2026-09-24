//! The README's screenshot: a demo screen drawn by the real UI code, written
//! out as an SVG. Each character is placed on a fixed grid, and box-drawing
//! characters are drawn as lines, so the picture lines up whatever fonts the
//! reader has. Regenerate with:
//!
//! ```sh
//! cargo test write_readme_screenshot -- --ignored
//! ```

use super::*;
use crate::app::DataMsg;
use crate::github::{Actor, Job, RateLimit, Run, RunRepo, RunTag};
use chrono::Duration;
use ratatui::buffer::Buffer;
use std::fmt::Write as _;

const WIDTH: u16 = 120;
const HEIGHT: u16 = 24;
const OUT: &str = "docs/screenshot.svg";

/// One character cell, in pixels.
const CELL_W: f32 = 8.4;
const CELL_H: f32 = 18.0;
const FONT_SIZE: f32 = 14.0;
const PAD: f32 = 12.0;

#[test]
#[ignore = "writes docs/screenshot.svg; run it by name to refresh the README image"]
fn write_readme_screenshot() {
    set_theme(Theme::dark());
    let mut app = demo();
    app.sync_layout(WIDTH);
    let backend = ratatui::backend::TestBackend::new(WIDTH, HEIGHT);
    let mut term = ratatui::Terminal::new(backend).unwrap();
    term.draw(|f| draw(f, &mut app)).unwrap();
    let svg = to_svg(term.backend().buffer());
    std::fs::create_dir_all("docs").unwrap();
    std::fs::write(OUT, svg).unwrap();
}

/// A few repos mid-afternoon: one run going, one waiting, one failed, and a
/// release that just went out.
fn demo() -> App {
    let now = Utc::now();
    let run = |id: u64, repo: &str, flow: &str, number: u64, branch: &str, status: &str,
               conclusion: Option<&str>, ago: i64, took: i64| {
        let start = now - Duration::seconds(ago);
        Run {
            id,
            name: Some(flow.into()),
            display_title: "Update dependencies".into(),
            head_branch: Some(branch.into()),
            head_sha: String::new(),
            run_number: number,
            workflow_id: id,
            event: "push".into(),
            status: status.into(),
            conclusion: conclusion.map(str::to_string),
            html_url: String::new(),
            created_at: start,
            updated_at: start + Duration::seconds(took),
            run_started_at: Some(start),
            actor: Some(Actor { login: "you".into() }),
            repository: RunRepo { full_name: repo.into() },
        }
    };
    let job = |id: u64, name: &str, status: &str, conclusion: Option<&str>, ago: i64, took: i64| Job {
        id,
        name: name.into(),
        status: status.into(),
        html_url: String::new(),
        check_run_url: String::new(),
        conclusion: conclusion.map(str::to_string),
        started_at: Some(now - Duration::seconds(ago)),
        completed_at: (took > 0).then(|| now - Duration::seconds(ago - took)),
        steps: Vec::new(),
    };

    let cfg = crate::config::Config { pinned: vec!["org/api".into()], ..Default::default() };
    let mut app = App::new(&cfg);
    app.state = crate::state::State::default();
    app.panes = crate::app::Panes::new(&app.state);
    app.user = "you".into();
    app.apply(DataMsg::Repos(
        ["org/api", "org/infra", "org/mobile", "org/web", "you/dotfiles"]
            .map(String::from)
            .to_vec(),
    ));
    app.loading = false;
    app.fetching.clear(); // nothing was really sent for
    app.rate = Some(RateLimit { limit: 5000, remaining: 4950 });
    app.last_refresh = Some(now - Duration::seconds(12));
    app.spinner = 2;
    app.runs = vec![
        run(1, "org/api", "CI", 296, "main", "in_progress", None, 72, 69),
        run(2, "org/mobile", "Release", 3, "main", "queued", None, 20, 0),
        run(3, "org/api", "Release", 41, "main", "completed", Some("success"), 900, 184),
        run(4, "you/dotfiles", "lint", 5, "fix-zsh", "completed", Some("failure"), 1500, 31),
        run(5, "org/web", "Deploy", 88, "main", "completed", Some("success"), 3600, 263),
        run(6, "org/infra", "Plan", 120, "tf-vpc", "completed", Some("success"), 7200, 95),
    ];
    app.recompute_view();
    app.table_state.select(Some(0));
    app.apply(DataMsg::Jobs {
        run_id: 1,
        jobs: vec![
            job(11, "build", "completed", Some("success"), 70, 48),
            job(12, "test", "in_progress", None, 21, 0),
            job(13, "lint", "completed", Some("success"), 70, 17),
        ],
    });
    let release = RunTag {
        name: "v0.4.0".into(),
        url: String::new(),
        released: true,
        title: None,
        prerelease: false,
    };
    app.apply(DataMsg::Tags { found: vec![(3, Some(release))] });
    app
}

/// Draw the buffer as an SVG: a background, then each row's backgrounds,
/// box-drawing lines and text.
fn to_svg(buf: &Buffer) -> String {
    let (cols, rows) = (buf.area.width, buf.area.height);
    let w = cols as f32 * CELL_W + 2.0 * PAD;
    let h = rows as f32 * CELL_H + 2.0 * PAD;
    let mut out = String::new();
    let _ = writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">"#
    );
    let _ = writeln!(out, r#"<rect width="{w}" height="{h}" rx="8" fill="{}"/>"#, hex(BASE));
    let _ = writeln!(
        out,
        r#"<g font-family="ui-monospace, SFMono-Regular, Menlo, Consolas, 'DejaVu Sans Mono', monospace" font-size="{FONT_SIZE}">"#
    );
    for y in 0..rows {
        let top = PAD + y as f32 * CELL_H;
        let mut lines = String::new();
        // Text runs of one style: the characters and where each one goes.
        let mut run: Option<(String, bool, String, Vec<f32>)> = None;
        let flush = |run: &mut Option<(String, bool, String, Vec<f32>)>, out: &mut String| {
            if let Some((fill, bold, text, xs)) = run.take() {
                let xs: Vec<String> = xs.iter().map(|x| format!("{x:.1}")).collect();
                let weight = if bold { r#" font-weight="bold""# } else { "" };
                let _ = writeln!(
                    out,
                    r#"<text x="{}" y="{:.1}" fill="{fill}"{weight}>{}</text>"#,
                    xs.join(" "),
                    top + CELL_H * 0.75,
                    escape(&text)
                );
            }
        };
        for x in 0..cols {
            let cell = &buf[(x, y)];
            let left = PAD + x as f32 * CELL_W;
            if cell.bg != Color::Reset {
                let _ = writeln!(
                    out,
                    r#"<rect x="{left:.1}" y="{top:.1}" width="{:.1}" height="{CELL_H}" fill="{}"/>"#,
                    CELL_W + 0.2,
                    hex(color(cell.bg, BASE))
                );
            }
            let fill = hex(color(cell.fg, TEXT));
            let sym = cell.symbol();
            if let Some(path) = box_path(sym, left, top) {
                let _ = writeln!(lines, r#"<path d="{path}" stroke="{fill}" fill="none" stroke-width="1.2"/>"#);
                continue;
            }
            if let Some(rect) = block_rect(sym, left, top) {
                let _ = writeln!(lines, r#"<rect {rect} fill="{fill}"/>"#);
                continue;
            }
            if sym.trim().is_empty() {
                continue;
            }
            let bold = cell.modifier.contains(Modifier::BOLD);
            match &mut run {
                Some((f, b, text, xs)) if *f == fill && *b == bold => {
                    text.push_str(sym);
                    xs.push(left);
                }
                _ => {
                    flush(&mut run, &mut out);
                    run = Some((fill, bold, sym.to_string(), vec![left]));
                }
            }
        }
        flush(&mut run, &mut out);
        out.push_str(&lines);
    }
    out.push_str("</g>\n</svg>\n");
    out
}

/// A box-drawing character as a path through its cell: a stroke from the
/// centre to each side it joins, with rounded corners drawn as curves.
fn box_path(sym: &str, left: f32, top: f32) -> Option<String> {
    // Which sides the character reaches: up, down, left, right.
    let (u, d, l, r, round) = match sym {
        "─" => (false, false, true, true, false),
        "│" => (true, true, false, false, false),
        "╭" => (false, true, false, true, true),
        "╮" => (false, true, true, false, true),
        "╰" => (true, false, false, true, true),
        "╯" => (true, false, true, false, true),
        "├" => (true, true, false, true, false),
        "┤" => (true, true, true, false, false),
        "┬" => (false, true, true, true, false),
        "┴" => (true, false, true, true, false),
        "┼" => (true, true, true, true, false),
        _ => return None,
    };
    let (cx, cy) = (left + CELL_W / 2.0, top + CELL_H / 2.0);
    let (right, bottom) = (left + CELL_W, top + CELL_H);
    if round {
        // From one side, curving through the centre, to the other.
        let (ax, ay) = if u { (cx, top) } else { (cx, bottom) };
        let (bx, by) = if l { (left, cy) } else { (right, cy) };
        return Some(format!("M{ax:.1} {ay:.1} Q{cx:.1} {cy:.1} {bx:.1} {by:.1}"));
    }
    let mut p = String::new();
    if l || r {
        let (a, b) = (if l { left } else { cx }, if r { right } else { cx });
        let _ = write!(p, "M{a:.1} {cy:.1} H{b:.1} ");
    }
    if u || d {
        let (a, b) = (if u { top } else { cy }, if d { bottom } else { cy });
        let _ = write!(p, "M{cx:.1} {a:.1} V{b:.1}");
    }
    Some(p.trim_end().to_string())
}

/// Block characters as filled rectangles, so they meet their neighbours.
fn block_rect(sym: &str, left: f32, top: f32) -> Option<String> {
    let (x, w) = match sym {
        "█" => (left, CELL_W),
        "▌" => (left, CELL_W / 2.0),
        "▐" => (left + CELL_W / 2.0, CELL_W / 2.0),
        _ => return None,
    };
    Some(format!(r#"x="{x:.1}" y="{top:.1}" width="{w:.1}" height="{CELL_H}""#))
}

/// The page behind everything, and the text colour the terminal would use.
const BASE: (u8, u8, u8) = (24, 24, 37);
const TEXT: (u8, u8, u8) = (205, 214, 244);

/// A cell colour as RGB. Named colours take the dark theme's matching shades;
/// `Reset` is whatever the terminal would show, given as `default`.
fn color(c: Color, default: (u8, u8, u8)) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (69, 71, 90),
        Color::Red | Color::LightRed => (243, 139, 168),
        Color::Green | Color::LightGreen => (166, 227, 161),
        Color::Yellow | Color::LightYellow => (249, 226, 175),
        Color::Blue | Color::LightBlue => (137, 180, 250),
        Color::Magenta | Color::LightMagenta => (203, 166, 247),
        Color::Cyan | Color::LightCyan => (137, 220, 235),
        Color::Gray | Color::White => (205, 214, 244),
        Color::DarkGray => (88, 91, 112),
        _ => default,
    }
}

fn hex((r, g, b): (u8, u8, u8)) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}
