//! actui — a TUI to view and manage GitHub Actions across your repos and orgs.

mod app;
mod config;
mod github;
mod ui;

use anyhow::Result;
use app::{App, Command, DataMsg, RunnerGroup};
use config::Config;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::{FutureExt, StreamExt};
use github::{Cond, Github, WfDispatch};
use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedSender};

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = Config::load();

    let token = match github::resolve_token() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("actui: {e}");
            std::process::exit(1);
        }
    };
    let gh = Github::new(&token)?;

    let mut terminal = ratatui::init();
    // Mouse: wheel scrolls, click selects rows / panes / filter tabs.
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture);
    let res = run(&mut terminal, gh, cfg).await;
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    ratatui::restore();
    res
}

/// Resolve the active palette: an explicit "dark"/"light" override, else the
/// system's current light/dark setting (defaulting to dark when unknown).
fn resolve_theme(cfg: &Config) -> ui::Theme {
    match cfg.theme.as_str() {
        "light" => ui::Theme::light(),
        "dark" => ui::Theme::dark(),
        _ => match dark_light::detect() {
            dark_light::Mode::Light => ui::Theme::light(),
            _ => ui::Theme::dark(), // Dark or Default
        },
    }
}

async fn run(terminal: &mut ratatui::DefaultTerminal, gh: Github, cfg: Config) -> Result<()> {
    let mut app = App::new();
    ui::set_theme(resolve_theme(&cfg));
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
    spawn_refresh(&gh, &cfg, &tx);

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
    // Re-check the system theme periodically so it switches live (auto mode only).
    let auto_theme = cfg.theme != "dark" && cfg.theme != "light";
    let mut last_theme = std::time::Instant::now();

    let mut redraw = true;
    loop {
        if redraw {
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
                // Follow the system light/dark setting while running (auto mode).
                // Detection can block (dbus on Linux), so keep it off the UI loop.
                if auto_theme && last_theme.elapsed() >= Duration::from_secs(3) {
                    last_theme = std::time::Instant::now();
                    tokio::task::spawn_blocking(|| {
                        let t = match dark_light::detect() {
                            dark_light::Mode::Light => ui::Theme::light(),
                            _ => ui::Theme::dark(),
                        };
                        ui::set_theme(t);
                    });
                }
                // Surface live rate-limit / back-off state from response headers.
                app.rate = gh.rate();
                let paused = gh.pause_remaining();
                app.paused_secs = paused.map(|d| d.as_secs());

                // Pause all polling on a rate-limit back-off or when quota is low.
                let low = app.rate.as_ref().is_some_and(|r| r.remaining < 50);
                let throttled = paused.is_some() || low;

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

        dispatch_commands(&mut app, &gh, &cfg, &tx);
    }
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
    for cmd in app.pending.drain(..).collect::<Vec<_>>() {
        match cmd {
            Command::Refresh => {
                app.loading = true;
                spawn_refresh(gh, cfg, tx);
            }
            Command::FetchJobs { repo, run_id } => {
                let (gh, tx) = (gh.clone(), tx.clone());
                tokio::spawn(async move {
                    // NotModified → keep the jobs we already have.
                    if let Ok(Cond::Modified(jobs)) = gh.list_jobs(&repo, run_id).await {
                        let _ = tx.send(DataMsg::Jobs { run_id, jobs });
                    }
                });
            }
            Command::FetchLogs { repo, job_id, title } => {
                let (gh, tx) = (gh.clone(), tx.clone());
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
            Command::FetchRunners { orgs } => {
                let (gh, tx) = (gh.clone(), tx.clone());
                tokio::spawn(async move {
                    // Merge the user's org memberships with the owners derived from
                    // loaded runs, de-duped case-insensitively.
                    let mut names = gh.list_orgs().await.unwrap_or_default();
                    for o in orgs {
                        if !names.iter().any(|n| n.eq_ignore_ascii_case(&o)) {
                            names.push(o);
                        }
                    }
                    names.sort_by_key(|s| s.to_lowercase());
                    if names.is_empty() {
                        let _ = tx.send(DataMsg::Runners { groups: vec![] });
                        return;
                    }
                    // Each org's runners in parallel; a 403 (no admin) becomes a
                    // per-org note rather than failing the whole view.
                    let groups = futures::future::join_all(names.into_iter().map(|org| {
                        let gh = gh.clone();
                        async move {
                            match gh.list_org_runners(&org).await {
                                Ok(runners) => RunnerGroup { org, runners, error: None },
                                Err(e) => RunnerGroup { org, runners: vec![], error: Some(e.to_string()) },
                            }
                        }
                    }))
                    .await;
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
fn spawn_refresh(gh: &Github, cfg: &Config, tx: &UnboundedSender<DataMsg>) {
    let gh = gh.clone();
    let cfg = cfg.clone();
    let tx = tx.clone();
    tokio::spawn(async move {
        // Conditional: on 304 reuse the cached repo list (no quota spent).
        let repos = match gh.list_repos().await {
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
        let mut repos: Vec<_> = repos
            .into_iter()
            .filter(|r| !(cfg.skip_archived && r.archived))
            .filter(|r| cfg.keep_repo(&r.full_name))
            .collect();
        if cfg.max_repos > 0 {
            repos.truncate(cfg.max_repos);
        }

        let _ = tx.send(DataMsg::Repos(repos.len()));
        if repos.is_empty() {
            let _ = tx.send(DataMsg::RefreshDone);
            return;
        }

        let per_page = cfg.runs_per_repo;
        futures::stream::iter(repos)
            .for_each_concurrent(cfg.concurrency.max(1), |repo| {
                let gh = gh.clone();
                let tx = tx.clone();
                async move {
                    match gh.list_runs(&repo.full_name, per_page).await {
                        Ok(Cond::Modified(runs)) => {
                            let _ = tx.send(DataMsg::Runs { repo: repo.full_name, runs });
                        }
                        Ok(Cond::NotModified) => {
                            let _ = tx.send(DataMsg::RunsUnchanged);
                        }
                        Err(e) => {
                            let _ = tx.send(DataMsg::RepoError {
                                repo: repo.full_name,
                                err: e.to_string(),
                            });
                        }
                    }
                }
            })
            .await;
    });
}
