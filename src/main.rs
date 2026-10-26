//! actui — a TUI to view and manage GitHub Actions across your repos and orgs.

mod app;
mod config;
mod github;
mod pulls;
mod state;
mod ui;

use anyhow::Result;
use app::{App, Command, DataMsg, Fetch, OrgRunners, RunnerGroup};
use config::Config;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::{FutureExt, StreamExt};
use github::{Cond, Github, Repo, WfDispatch};
use std::io::IsTerminal;
use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedSender};

/// What the command line asked for.
#[derive(Debug, PartialEq)]
enum Cli {
    Run,
    Help,
    Version,
    /// An argument actui doesn't take.
    Unknown(String),
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Cli {
    match args.next().as_deref() {
        None => Cli::Run,
        Some("-h" | "--help") => Cli::Help,
        Some("-V" | "--version") => Cli::Version,
        Some(other) => Cli::Unknown(other.to_string()),
    }
}

fn help() -> String {
    let config = config::dir()
        .map(|d| d.join("config.toml").display().to_string())
        .unwrap_or_else(|| "(no config directory on this system)".to_string());
    format!(
        "actui {version} — GitHub Actions across your repos and orgs, in the terminal

Usage: actui [options]

Options:
  -h, --help     Print this help
  -V, --version  Print the version

Auth:    $GITHUB_TOKEN or $GH_TOKEN, else `gh auth token`
Config:  {config}
Keys:    press ? inside actui

{repo}",
        version = env!("CARGO_PKG_VERSION"),
        repo = env!("CARGO_PKG_REPOSITORY"),
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    match parse_args(std::env::args().skip(1)) {
        Cli::Run => {}
        Cli::Help => {
            println!("{}", help());
            return Ok(());
        }
        Cli::Version => {
            println!("actui {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Cli::Unknown(arg) => {
            eprintln!("actui: unexpected argument '{arg}'\nTry 'actui --help'.");
            std::process::exit(2);
        }
    }
    // Without a terminal on both ends, ratatui can't draw and would panic.
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        eprintln!("actui: needs an interactive terminal");
        std::process::exit(1);
    }

    let cfg = Config::load();

    let token = match github::resolve_token() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("actui: {e}");
            std::process::exit(1);
        }
    };
    let gh = Github::new(&token)?;

    // The terminal has to be asked before raw mode starts, because its answer
    // arrives as terminal input and the event loop would take it first. Skipped
    // when the config already picked a palette.
    let colors = match cfg.theme.as_str() {
        "dark" | "light" => None,
        _ => terminal_colors(Duration::from_secs(1)),
    };

    let mut terminal = ratatui::init();
    // Mouse: wheel scrolls, click selects rows / panes / filter tabs.
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture);
    let res = run(&mut terminal, gh, cfg, colors).await;
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    ratatui::restore();
    res
}

/// The terminal's foreground and background colors.
#[derive(Clone, Copy, PartialEq, Debug)]
struct TermColors {
    fg: (u8, u8, u8),
    bg: (u8, u8, u8),
}

/// Where the palette comes from.
#[derive(Clone, Copy, PartialEq, Debug)]
enum ThemeSource {
    /// The config picked "dark" or "light".
    Configured { dark: bool },
    /// Built from the colors the terminal reported. Asked again while the app
    /// runs, so changing the terminal's theme changes ours.
    Terminal(TermColors),
    /// The terminal won't report its colors: named ANSI colors, which it draws
    /// in its own scheme, so they follow it without being asked.
    Ansi,
}

/// Pick the palette. A configured "dark"/"light" wins outright. Otherwise the
/// terminal's own colors decide, since that is the screen actually being drawn
/// on.
fn resolve_theme(configured: &str, terminal: Option<TermColors>) -> ThemeSource {
    match configured {
        "dark" => ThemeSource::Configured { dark: true },
        "light" => ThemeSource::Configured { dark: false },
        _ => terminal.map_or(ThemeSource::Ansi, ThemeSource::Terminal),
    }
}

/// Ask the terminal for its foreground and background colors. None when it
/// doesn't answer within `wait`: an older terminal, output that is not a
/// terminal at all, or a link too slow to wait on.
fn terminal_colors(wait: Duration) -> Option<TermColors> {
    let mut options = terminal_colorsaurus::QueryOptions::default();
    options.timeout = wait;
    let colors = terminal_colorsaurus::color_palette(options).ok()?;
    Some(TermColors {
        fg: colors.foreground.scale_to_8bit(),
        bg: colors.background.scale_to_8bit(),
    })
}

fn palette(source: ThemeSource) -> ui::Theme {
    match source {
        ThemeSource::Configured { dark: true } => ui::Theme::dark(),
        ThemeSource::Configured { dark: false } => ui::Theme::light(),
        ThemeSource::Terminal(c) => ui::Theme::from_terminal(c.fg, c.bg),
        ThemeSource::Ansi => ui::Theme::ansi(),
    }
}

async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    gh: Github,
    cfg: Config,
    colors: Option<TermColors>,
) -> Result<()> {
    let mut app = App::new(&cfg);
    let mut theme = resolve_theme(&cfg.theme, colors);
    ui::set_theme(palette(theme));
    let (tx, mut rx) = mpsc::unbounded_channel::<DataMsg>();

    // Initial load.
    {
        let gh = gh.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            match gh.whoami().await {
                Ok(u) => {
                    let _ = tx.send(DataMsg::User(u));
                }
                Err(e) => {
                    let _ = tx.send(DataMsg::Error(format!("auth check failed: {e}")));
                }
            }
        });
    }
    // One free `/rate_limit` read for an immediate header number.
    {
        let gh = gh.clone();
        tokio::spawn(async move {
            let _ = gh.rate_limit().await;
        });
    }
    spawn_refresh(&gh, &cfg, &tx, None);

    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(120));
    let mut sched = tokio::time::interval(Duration::from_secs(1));
    // Two-tier cadence: a slow broad sweep of all repos, and a faster focused
    // poll of just the selected run's jobs while it's active.
    let broad_iv = Duration::from_secs(cfg.refresh_secs.max(15));
    let focused_iv = Duration::from_secs(cfg.active_refresh_secs.clamp(5, cfg.refresh_secs.max(15)));
    // While the live step view is open on a running job, poll just that one run
    // on a tight cadence so steps advance near real-time, like the web UI.
    let live_iv = Duration::from_secs(cfg.live_refresh_secs.clamp(1, 10));
    let mut last_broad = std::time::Instant::now();
    let mut last_focused = std::time::Instant::now();
    let mut last_live = std::time::Instant::now();
    // After a dispatch, how often to look for the run GitHub makes of it.
    let pickup_iv = Duration::from_secs(3);
    let mut last_pickup = std::time::Instant::now();
    // The open PR is read again on its own cadence.
    let mut last_pr = std::time::Instant::now();
    // Re-ask the terminal for its colors periodically so the theme switches
    // live.
    let mut ask_terminal = false;
    let mut last_theme = std::time::Instant::now();

    let mut redraw = true;
    loop {
        if redraw {
            let width = terminal.size().map(|s| s.width).unwrap_or(u16::MAX);
            app.sync_layout(width);
            terminal.draw(|f| ui::draw(f, &mut app))?;
            redraw = false;
        }
        if app.should_quit {
            break;
        }

        tokio::select! {
            maybe_ev = events.next() => {
                match maybe_ev {
                    Some(Ok(ev)) => redraw |= handle_event(&mut app, ev),
                    Some(Err(_)) | None => break,
                }
            }
            Some(msg) = rx.recv() => {
                app.apply(msg);
                redraw = true;
            }
            _ = tick.tick() => {
                // Animates the spinner while loading, expires stale status;
                // idle ticks don't redraw.
                redraw |= app.tick();
            }
            _ = sched.tick() => {
                // The once-a-second redraw also keeps ages/durations current.
                redraw = true;
                app.save_state();
                if matches!(theme, ThemeSource::Terminal(_))
                    && last_theme.elapsed() >= Duration::from_secs(3)
                {
                    last_theme = std::time::Instant::now();
                    // Asked below, once the event stream is free of this select.
                    ask_terminal = true;
                }
                // Surface live rate-limit / back-off state from response headers.
                app.rate = gh.rate();
                let paused = gh.pause_remaining();
                app.paused_secs = paused.map(|d| d.as_secs());

                // Pause all polling on a rate-limit back-off or when quota is low.
                let low = app.rate.as_ref().is_some_and(|r| r.remaining < 50);
                let throttled = paused.is_some() || low;
                // The PR screens spend GraphQL points, which run out apart from REST's.
                app.prs.paused = throttled || gh.prs_paused();

                // Honor X-Poll-Interval as a floor (absent on Actions endpoints today).
                let floor = Duration::from_secs(gh.poll_interval_secs());

                // A broad sweep is gated on `loading` so we never stack two sweeps;
                // the live-steps and focused tiers deliberately are NOT, so the open
                // live step view keeps advancing on its tight cadence even while a
                // slow broad sweep is in flight (their FetchJobs are cheap, bounded,
                // and ETag-conditional). All tiers still pause when throttled.
                if !app.loading && !throttled && last_broad.elapsed() >= broad_iv.max(floor) {
                    app.queue_broad_refresh();
                    last_broad = std::time::Instant::now();
                    last_focused = std::time::Instant::now(); // broad already covers jobs
                    last_live = std::time::Instant::now();
                } else if !throttled
                    && app.live_steps_run().is_some()
                    && last_live.elapsed() >= live_iv.max(floor)
                {
                    // Tight poll of the one run feeding the open live step view.
                    app.queue_live_steps_refresh();
                    last_live = std::time::Instant::now();
                } else if !throttled
                    && app.any_run_active()
                    && last_focused.elapsed() >= focused_iv.max(floor)
                {
                    app.queue_focused_refresh();
                    last_focused = std::time::Instant::now();
                }
                // Apart from the tiers above: it reads only the dispatched
                // repos, and stops once their runs show up.
                if !throttled && app.has_pending_dispatch() && last_pickup.elapsed() >= pickup_iv {
                    app.queue_dispatch_pickup();
                    last_pickup = std::time::Instant::now();
                }
                // An open PR with checks still running is watched closely.
                if app.prs.open.is_none() {
                    // Counts from the moment a PR opens, which fetches it itself.
                    last_pr = std::time::Instant::now();
                }
                let pr_iv = if app.prs.open_checks_pending() { focused_iv } else { broad_iv };
                if !app.prs.paused && app.prs.open.is_some() && last_pr.elapsed() >= pr_iv.max(floor) {
                    app.queue_pr_refresh();
                    last_pr = std::time::Instant::now();
                }
            }
        }

        // Coalesce whatever else is already ready into this same frame: a
        // burst of per-repo results or auto-repeated keys becomes one redraw
        // instead of one per item. Bounded so a flood can't starve rendering.
        for _ in 0..256 {
            match rx.try_recv() {
                Ok(msg) => {
                    app.apply(msg);
                    redraw = true;
                }
                Err(_) => break,
            }
        }
        for _ in 0..64 {
            match events.next().now_or_never() {
                Some(Some(Ok(ev))) => redraw |= handle_event(&mut app, ev),
                Some(Some(Err(_)) | None) => {
                    app.should_quit = true;
                    break;
                }
                None => break,
            }
        }

        // The terminal's answer arrives as input, so our reader has to stop
        // first, or it would take the answer as keypresses. Keys it had
        // already read stay queued for the new stream.
        if std::mem::take(&mut ask_terminal) {
            drop(events);
            // Waits for the dropped stream's reader thread to let go of input.
            let _ = crossterm::event::poll(Duration::from_millis(50));
            // A late answer would land in the new stream as keypresses, and
            // each wait stalls the app, so a terminal that missed once is not
            // asked again. ANSI colors take over, and they still follow its
            // scheme.
            let now = terminal_colors(Duration::from_millis(500))
                .map_or(ThemeSource::Ansi, ThemeSource::Terminal);
            if now != theme {
                theme = now;
                ui::set_theme(palette(theme));
                redraw = true;
            }
            events = EventStream::new();
        }

        // Manual refresh (r / F5): immediate broad sweep, unless backing off.
        if std::mem::take(&mut app.force_refresh) {
            if let Some(d) = gh.pause_remaining() {
                app.notify(format!("Rate-limited — try again in {}s", d.as_secs()), true);
            } else if !app.loading {
                app.queue_broad_refresh();
                last_broad = std::time::Instant::now();
                last_focused = std::time::Instant::now();
            }
        }

        // Requests going out dim the rows they read, so that's a new frame.
        redraw |= !app.pending.is_empty();
        dispatch_commands(&mut app, &gh, &cfg, &tx);
    }
    // Keep whatever changed since the last tick.
    app.save_state();
    Ok(())
}

/// Apply one terminal event. Returns true when it changed visible state and
/// the frame needs a redraw (pointer motion, key releases etc. don't).
fn handle_event(app: &mut App, ev: Event) -> bool {
    match ev {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            app.handle_key(key);
            true
        }
        Event::Mouse(m) => app.handle_mouse(m),
        Event::Resize(_, _) => true,
        _ => false,
    }
}

/// Spawn a fire-and-forget mutation and report its outcome to the UI: a fixed
/// success notice, or the error under `err_prefix`. Captures the shared
/// `Ok(()) -> Action / Err -> Error` shape of the action commands so each arm
/// can't drift in how it reports success or failure.
fn spawn_action<F, Fut>(
    tx: &UnboundedSender<DataMsg>,
    ok: impl Into<String>,
    err_prefix: &'static str,
    fut: F,
) where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let tx = tx.clone();
    let ok = ok.into();
    tokio::spawn(async move {
        let msg = match fut().await {
            Ok(()) => DataMsg::Action(ok),
            Err(e) => DataMsg::Error(format!("{err_prefix}: {e}")),
        };
        let _ = tx.send(msg);
    });
}

/// Execute everything the UI queued since the last iteration.
fn dispatch_commands(app: &mut App, gh: &Github, cfg: &Config, tx: &UnboundedSender<DataMsg>) {
    for cmd in std::mem::take(&mut app.pending) {
        match cmd {
            Command::FetchRuns { repo } => {
                let (gh, tx) = (gh.clone(), tx.clone());
                let per_page = cfg.runs_for(&repo, app.repos.scope());
                app.start_fetch(Fetch::Runs(repo.clone()));
                tokio::spawn(async move {
                    match gh.list_runs(&repo, per_page).await {
                        // NotModified → we already have this repo's deep list.
                        Ok(Cond::Modified(runs)) => {
                            let _ = tx.send(DataMsg::RunsOnly { repo: repo.clone(), runs });
                        }
                        Ok(Cond::NotModified) => {}
                        Err(e) => {
                            let _ = tx.send(DataMsg::Error(format!("{repo}: {e}")));
                        }
                    }
                    let _ = tx.send(DataMsg::Fetched(Fetch::Runs(repo)));
                });
            }
            Command::Refresh { deep } => {
                app.loading = true;
                app.listing_repos = true;
                spawn_refresh(gh, cfg, tx, deep);
            }
            Command::FetchJobs { repo, run_id } => {
                let (gh, tx) = (gh.clone(), tx.clone());
                app.start_fetch(Fetch::Jobs(run_id));
                tokio::spawn(async move {
                    // NotModified → keep the jobs we already have.
                    if let Ok(Cond::Modified(jobs)) = gh.list_jobs(&repo, run_id).await {
                        let _ = tx.send(DataMsg::Jobs { run_id, jobs });
                    }
                    let _ = tx.send(DataMsg::Fetched(Fetch::Jobs(run_id)));
                });
            }
            Command::FetchLogs { repo, job_id, title } => {
                let (gh, tx) = (gh.clone(), tx.clone());
                app.start_fetch(Fetch::Logs(job_id));
                tokio::spawn(async move {
                    match gh.job_logs(&repo, job_id).await {
                        Ok(text) => {
                            let text = if text.trim().is_empty() {
                                "(no logs yet)".to_string()
                            } else {
                                text
                            };
                            let _ = tx.send(DataMsg::Logs { job_id, title, text });
                        }
                        Err(e) => {
                            let _ = tx.send(DataMsg::Error(format!("logs: {e}")));
                        }
                    }
                    let _ = tx.send(DataMsg::Fetched(Fetch::Logs(job_id)));
                });
            }
            Command::FetchAnnotations { run_id, jobs } => {
                let (gh, tx) = (gh.clone(), tx.clone());
                tokio::spawn(async move {
                    // Fetch each job's annotations concurrently; a job that errors
                    // (or has none) just contributes nothing rather than failing all.
                    let fetches = jobs.into_iter().map(|j| {
                        let gh = gh.clone();
                        async move {
                            gh.annotations(&j.check_run_url)
                                .await
                                .unwrap_or_default()
                                .iter()
                                .map(|a| app::AnnotationItem::new(j.job_id, &j.job_name, a))
                                .collect::<Vec<_>>()
                        }
                    });
                    let items = futures::future::join_all(fetches)
                        .await
                        .into_iter()
                        .flatten()
                        .collect();
                    let _ = tx.send(DataMsg::Annotations { run_id, items });
                });
            }
            Command::FetchWorkflows { repo } => {
                let (gh, tx) = (gh.clone(), tx.clone());
                tokio::spawn(async move {
                    match gh.list_workflows(&repo).await {
                        Ok(workflows) => {
                            let _ = tx.send(DataMsg::Workflows { repo, workflows });
                        }
                        Err(e) => {
                            let _ = tx.send(DataMsg::Error(format!("workflows: {e}")));
                        }
                    }
                });
            }
            Command::FetchWorkflowRuns { repo, workflow_id } => {
                let (gh, tx) = (gh.clone(), tx.clone());
                tokio::spawn(async move {
                    let n = app::TIMING_HISTORY;
                    match gh.list_workflow_runs(&repo, workflow_id, n).await {
                        Ok(Cond::Modified(runs)) => {
                            let _ = tx.send(DataMsg::WorkflowRuns { workflow_id, runs });
                        }
                        // Nothing new since we last read it — the chart still has it.
                        Ok(Cond::NotModified) => {
                            let _ = tx.send(DataMsg::WorkflowRunsUnchanged { workflow_id });
                        }
                        Err(e) => {
                            let _ = tx.send(DataMsg::Error(format!("workflow runs: {e}")));
                        }
                    }
                });
            }
            Command::FetchWorkflowInputs { repo, path, git_ref } => {
                let (gh, tx) = (gh.clone(), tx.clone());
                tokio::spawn(async move {
                    match gh.workflow_inputs(&repo, &path, &git_ref).await {
                        Ok(WfDispatch::Inputs(inputs)) => {
                            let _ = tx.send(DataMsg::WorkflowInputs { repo, dispatchable: true, inputs });
                        }
                        Ok(WfDispatch::NotDispatchable) => {
                            let _ = tx.send(DataMsg::WorkflowInputs { repo, dispatchable: false, inputs: vec![] });
                        }
                        Err(e) => {
                            // Couldn't read/parse the YAML — let the user dispatch
                            // with just a ref rather than blocking entirely.
                            let _ = tx.send(DataMsg::Error(format!("reading inputs: {e}")));
                            let _ = tx.send(DataMsg::WorkflowInputs { repo, dispatchable: true, inputs: vec![] });
                        }
                    }
                });
            }
            Command::Dispatch { repo, workflow_id, git_ref, inputs, placeholder_id } => {
                let (gh, tx) = (gh.clone(), tx.clone());
                let ok = format!("Dispatched workflow on {repo}@{git_ref}");
                tokio::spawn(async move {
                    let msg = match gh.dispatch(&repo, workflow_id, &git_ref, inputs).await {
                        Ok(()) => DataMsg::Action(ok),
                        // Drop the optimistic placeholder we showed on submit.
                        Err(e) => DataMsg::DispatchFailed {
                            placeholder_id,
                            err: format!("dispatch: {e}"),
                        },
                    };
                    let _ = tx.send(msg);
                });
            }
            Command::Cancel { repo, run_id } => {
                let gh = gh.clone();
                spawn_action(tx, "Cancellation requested", "cancel", move || async move {
                    gh.cancel(&repo, run_id).await
                });
            }
            Command::Rerun { repo, run_id } => {
                let gh = gh.clone();
                spawn_action(tx, "Re-run requested", "rerun", move || async move {
                    gh.rerun(&repo, run_id).await
                });
            }
            Command::RerunFailed { repo, run_id } => {
                let gh = gh.clone();
                spawn_action(tx, "Re-run (failed jobs) requested", "rerun-failed", move || async move {
                    gh.rerun_failed(&repo, run_id).await
                });
            }
            Command::RerunJob { repo, job_id } => {
                let gh = gh.clone();
                spawn_action(tx, "Re-run (job) requested", "rerun-job", move || async move {
                    gh.rerun_job(&repo, job_id).await
                });
            }
            Command::Approve { repo, run_id } => {
                let gh = gh.clone();
                spawn_action(tx, "Run approved", "approve", move || async move {
                    gh.approve(&repo, run_id).await
                });
            }
            Command::FetchPendingDeployments { repo, run_id } => {
                let (gh, tx) = (gh.clone(), tx.clone());
                tokio::spawn(async move {
                    match gh.pending_deployments(&repo, run_id).await {
                        Ok(items) => { let _ = tx.send(DataMsg::PendingDeployments { run_id, items }); }
                        Err(e) => {
                            // Report, and send an empty set so the UI falls back to
                            // the fork-PR approval path rather than hanging.
                            let _ = tx.send(DataMsg::Error(format!("pending deployments: {e}")));
                            let _ = tx.send(DataMsg::PendingDeployments { run_id, items: vec![] });
                        }
                    }
                });
            }
            Command::ReviewDeployments { repo, run_id, env_ids, approve, comment } => {
                let gh = gh.clone();
                // The same word is both the API `state` and the success notice.
                let word = if approve { "approved" } else { "rejected" };
                spawn_action(tx, format!("Deployment {word}"), "review", move || async move {
                    gh.review_deployments(&repo, run_id, &env_ids, word, &comment).await
                });
            }
            Command::FetchRefs { repo } => {
                let (gh, tx) = (gh.clone(), tx.clone());
                tokio::spawn(async move {
                    // Branches and tags in parallel; either failing yields an empty
                    // list so the picker still opens with whatever resolved.
                    let (branches, tags) =
                        tokio::join!(gh.list_branches(&repo), gh.list_tags(&repo));
                    if let Err(e) = &branches {
                        let _ = tx.send(DataMsg::Error(format!("branches: {e}")));
                    }
                    let _ = tx.send(DataMsg::Refs {
                        repo,
                        branches: branches.unwrap_or_default(),
                        tags: tags.unwrap_or_default(),
                    });
                });
            }
            Command::FetchRunners { orgs, repos } => {
                let (gh, tx) = (gh.clone(), tx.clone());
                let concurrency = cfg.concurrency.max(1);
                tokio::spawn(async move {
                    // Merge the user's org memberships with the owners derived from
                    // loaded runs, de-duped case-insensitively.
                    let mut names = gh.list_orgs().await.unwrap_or_default();
                    for o in orgs {
                        if !names.iter().any(|n| n.eq_ignore_ascii_case(&o)) {
                            names.push(o);
                        }
                    }
                    // Each org's runners in parallel; a 403 (no admin) becomes a
                    // per-org note rather than failing the whole view.
                    let org_lists = futures::future::join_all(names.iter().map(|org| gh.list_org_runners(org)));
                    // Each watched repo's own runners, a few at a time. Repos that
                    // refuse (no admin) or have none are left out.
                    let repo_lists = futures::stream::iter(repos)
                        .map(|repo| {
                            let gh = gh.clone();
                            async move {
                                let runners = gh.list_repo_runners(&repo).await;
                                runners.ok().filter(|r| !r.is_empty()).map(|r| (repo, r))
                            }
                        })
                        .buffer_unordered(concurrency)
                        .filter_map(std::future::ready)
                        .collect::<Vec<_>>();
                    let (org_lists, mut repo_lists) = futures::join!(org_lists, repo_lists);
                    repo_lists.sort_by_key(|(repo, _)| repo.to_lowercase());

                    let mut groups: Vec<RunnerGroup> = names
                        .into_iter()
                        .zip(org_lists)
                        .map(|(owner, listed)| RunnerGroup {
                            owner,
                            org: match listed {
                                Ok(runners) => OrgRunners::Listed(runners),
                                Err(e) => OrgRunners::Failed(e.to_string()),
                            },
                            repos: Vec::new(),
                        })
                        .collect();
                    for (repo, runners) in repo_lists {
                        let owner = repo.split_once('/').map_or(repo.as_str(), |(o, _)| o).to_string();
                        match groups.iter_mut().find(|g| g.owner.eq_ignore_ascii_case(&owner)) {
                            Some(g) => g.repos.push((repo, runners)),
                            None => groups.push(RunnerGroup {
                                owner,
                                org: OrgRunners::NotAsked,
                                repos: vec![(repo, runners)],
                            }),
                        }
                    }
                    let _ = tx.send(DataMsg::Runners { groups });
                });
            }
            Command::FetchArtifacts { repo, run_id } => {
                let (gh, tx) = (gh.clone(), tx.clone());
                tokio::spawn(async move {
                    match gh.list_artifacts(&repo, run_id).await {
                        Ok(artifacts) => { let _ = tx.send(DataMsg::Artifacts { run_id, artifacts }); }
                        Err(e) => { let _ = tx.send(DataMsg::Error(format!("artifacts: {e}"))); }
                    }
                });
            }
            Command::FetchTags { repo, runs } => {
                let (gh, tx) = (gh.clone(), tx.clone());
                tokio::spawn(async move {
                    // A repo whose tags we can't read just shows nothing —
                    // complaining every poll would be noise. Staying quiet also
                    // leaves those runs unanswered, so the next poll tries again.
                    if let Ok(found) = gh.tags_for_commits(&repo, &runs).await {
                        let _ = tx.send(DataMsg::Tags { found });
                    }
                });
            }
            Command::DownloadArtifact { repo, artifact_id, name } => {
                let (gh, tx) = (gh.clone(), tx.clone());
                tokio::spawn(async move {
                    match gh.download_artifact(&repo, artifact_id).await {
                        Ok(bytes) => {
                            let file = format!("{name}.zip");
                            match std::fs::write(&file, &bytes) {
                                Ok(()) => { let _ = tx.send(DataMsg::Action(format!("Saved {file}"))); }
                                Err(e) => { let _ = tx.send(DataMsg::Error(format!("writing {file}: {e}"))); }
                            }
                        }
                        Err(e) => { let _ = tx.send(DataMsg::Error(format!("download: {e}"))); }
                    }
                });
            }
            Command::SaveLogs { name, content } => {
                let tx = tx.clone();
                match std::fs::write(&name, content) {
                    Ok(()) => { let _ = tx.send(DataMsg::Action(format!("Saved {name}"))); }
                    Err(e) => { let _ = tx.send(DataMsg::Error(format!("writing {name}: {e}"))); }
                }
            }
            Command::Pr(cmd) => pulls::run_command(cmd, gh, tx),
            Command::OpenUrl(url) => {
                let _ = open::that_detached(&url);
            }
            Command::Notify { title, body, failed } => {
                if cfg.bell {
                    use std::io::Write;
                    let mut out = std::io::stdout();
                    let _ = out.write_all(b"\x07");
                    let _ = out.flush();
                }
                if cfg.notify {
                    // Showing a toast can briefly block; keep it off the UI thread.
                    tokio::task::spawn_blocking(move || {
                        use notify_rust::Notification;
                        let mut n = Notification::new();
                        n.summary(&title).body(&body).appname("actui");
                        #[cfg(target_os = "linux")]
                        n.urgency(if failed {
                            notify_rust::Urgency::Critical
                        } else {
                            notify_rust::Urgency::Normal
                        });
                        #[cfg(not(target_os = "linux"))]
                        let _ = failed;
                        let _ = n.show();
                    });
                }
            }
        }
    }
}

/// Discover repos, then stream their recent runs back as they arrive.
fn spawn_refresh(
    gh: &Github,
    cfg: &Config,
    tx: &UnboundedSender<DataMsg>,
    deep: Option<String>,
) {
    let gh = gh.clone();
    let cfg = cfg.clone();
    let tx = tx.clone();
    tokio::spawn(async move {
        // Conditional: on 304 reuse the cached repo list (no quota spent).
        let keep = |r: &Repo| !(cfg.skip_archived && r.archived) && cfg.keep_repo(&r.full_name);
        let repos = match gh.list_repos(keep, cfg.max_repos).await {
            Ok(Cond::Modified(r)) => r,
            Ok(Cond::NotModified) => gh.cached_repos(),
            Err(e) => {
                let _ = tx.send(DataMsg::Error(format!("listing repos: {e}")));
                let _ = tx.send(DataMsg::RefreshDone);
                return;
            }
        };
        // `list_repos` returns repos sorted by most-recently-pushed, so the cap
        // keeps the repos most likely to have active runs.
        let mut repos: Vec<_> = repos.into_iter().filter(|r| keep(r)).collect();
        if cfg.max_repos > 0 {
            repos.truncate(cfg.max_repos);
        }

        let names = repos.iter().map(|r| r.full_name.clone()).collect();
        let _ = tx.send(DataMsg::Repos(names));
        if repos.is_empty() {
            let _ = tx.send(DataMsg::RefreshDone);
            return;
        }

        futures::stream::iter(repos)
            .for_each_concurrent(cfg.concurrency.max(1), |repo| {
                let gh = gh.clone();
                let tx = tx.clone();
                let per_page = cfg.runs_for(&repo.full_name, deep.as_deref());
                async move {
                    let name = repo.full_name;
                    let msg = match gh.list_runs(&name, per_page).await {
                        Ok(Cond::Modified(runs)) => DataMsg::Runs { repo: name.clone(), runs },
                        Ok(Cond::NotModified) => DataMsg::RunsUnchanged,
                        Err(e) => DataMsg::RepoError { repo: name.clone(), err: e.to_string() },
                    };
                    let _ = tx.send(msg);
                    let _ = tx.send(DataMsg::Fetched(Fetch::Runs(name)));
                }
            })
            .await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Cli {
        parse_args(list.iter().map(|a| a.to_string()))
    }

    #[test]
    fn reads_the_command_line() {
        assert_eq!(args(&[]), Cli::Run);
        assert_eq!(args(&["-h"]), Cli::Help);
        assert_eq!(args(&["--help"]), Cli::Help);
        assert_eq!(args(&["-V"]), Cli::Version);
        assert_eq!(args(&["--version"]), Cli::Version);
        assert_eq!(args(&["--frobnicate"]), Cli::Unknown("--frobnicate".into()));
    }

    #[test]
    fn a_configured_theme_beats_anything_detected() {
        let colors = Some(TermColors { fg: (0, 0, 0), bg: (255, 255, 255) });
        assert_eq!(resolve_theme("dark", colors), ThemeSource::Configured { dark: true });
        assert_eq!(resolve_theme("light", None), ThemeSource::Configured { dark: false });
    }

    #[test]
    fn the_terminal_colors_decide_when_it_answers() {
        let colors = TermColors { fg: (205, 214, 244), bg: (30, 30, 46) };
        assert_eq!(resolve_theme("auto", Some(colors)), ThemeSource::Terminal(colors));
    }

    #[test]
    fn ansi_colors_stand_in_when_the_terminal_stays_silent() {
        assert_eq!(resolve_theme("auto", None), ThemeSource::Ansi);
    }
}
