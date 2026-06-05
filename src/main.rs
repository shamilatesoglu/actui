//! actui — a TUI to view and manage GitHub Actions across your repos and orgs.

mod app;
mod config;
mod github;
mod ui;

use anyhow::Result;
use app::{App, Command, DataMsg};
use config::Config;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
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
    let res = run(&mut terminal, gh, cfg).await;
    ratatui::restore();
    res
}

async fn run(terminal: &mut ratatui::DefaultTerminal, gh: Github, cfg: Config) -> Result<()> {
    let mut app = App::new();
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
    let mut last_broad = std::time::Instant::now();
    let mut last_focused = std::time::Instant::now();

    loop {
        terminal.draw(|f| ui::draw(f, &app))?;
        if app.should_quit {
            break;
        }

        tokio::select! {
            maybe_ev = events.next() => {
                match maybe_ev {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        app.handle_key(key);
                    }
                    Some(Ok(_)) => {}      // resize/mouse/etc — redraw handles it
                    Some(Err(_)) | None => break,
                }
            }
            Some(msg) = rx.recv() => {
                app.apply(msg);
            }
            _ = tick.tick() => {
                app.tick(); // animates spinner while loading, expires stale status
            }
            _ = sched.tick() => {
                // Surface live rate-limit / back-off state from response headers.
                app.rate = gh.rate();
                let paused = gh.pause_remaining();
                app.paused_secs = paused.map(|d| d.as_secs());

                // When rate-limited, do nothing until the back-off clears.
                let low = app.rate.as_ref().is_some_and(|r| r.remaining < 50);
                let blocked = app.loading || paused.is_some();

                // Honor X-Poll-Interval as a floor (absent on Actions endpoints today).
                let floor = Duration::from_secs(gh.poll_interval_secs());

                if !blocked && !low && last_broad.elapsed() >= broad_iv.max(floor) {
                    app.queue_broad_refresh();
                    last_broad = std::time::Instant::now();
                    last_focused = std::time::Instant::now(); // broad already covers jobs
                } else if !blocked
                    && app.any_run_active()
                    && last_focused.elapsed() >= focused_iv.max(floor)
                {
                    app.queue_focused_refresh();
                    last_focused = std::time::Instant::now();
                }
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
            Command::Dispatch { repo, workflow_id, git_ref, inputs } => {
                let (gh, tx) = (gh.clone(), tx.clone());
                tokio::spawn(async move {
                    match gh.dispatch(&repo, workflow_id, &git_ref, inputs).await {
                        Ok(()) => {
                            let _ = tx.send(DataMsg::Action(format!(
                                "Dispatched workflow on {repo}@{git_ref}"
                            )));
                        }
                        Err(e) => {
                            let _ = tx.send(DataMsg::Error(format!("dispatch: {e}")));
                        }
                    }
                });
            }
            Command::Cancel { repo, run_id } => {
                let (gh, tx) = (gh.clone(), tx.clone());
                tokio::spawn(async move {
                    match gh.cancel(&repo, run_id).await {
                        Ok(()) => { let _ = tx.send(DataMsg::Action("Cancellation requested".into())); }
                        Err(e) => { let _ = tx.send(DataMsg::Error(format!("cancel: {e}"))); }
                    }
                });
            }
            Command::Rerun { repo, run_id } => {
                let (gh, tx) = (gh.clone(), tx.clone());
                tokio::spawn(async move {
                    match gh.rerun(&repo, run_id).await {
                        Ok(()) => { let _ = tx.send(DataMsg::Action("Re-run requested".into())); }
                        Err(e) => { let _ = tx.send(DataMsg::Error(format!("rerun: {e}"))); }
                    }
                });
            }
            Command::RerunFailed { repo, run_id } => {
                let (gh, tx) = (gh.clone(), tx.clone());
                tokio::spawn(async move {
                    match gh.rerun_failed(&repo, run_id).await {
                        Ok(()) => { let _ = tx.send(DataMsg::Action("Re-run (failed jobs) requested".into())); }
                        Err(e) => { let _ = tx.send(DataMsg::Error(format!("rerun-failed: {e}"))); }
                    }
                });
            }
            Command::RerunJob { repo, job_id } => {
                let (gh, tx) = (gh.clone(), tx.clone());
                tokio::spawn(async move {
                    match gh.rerun_job(&repo, job_id).await {
                        Ok(()) => { let _ = tx.send(DataMsg::Action("Re-run (job) requested".into())); }
                        Err(e) => { let _ = tx.send(DataMsg::Error(format!("rerun-job: {e}"))); }
                    }
                });
            }
            Command::Approve { repo, run_id } => {
                let (gh, tx) = (gh.clone(), tx.clone());
                tokio::spawn(async move {
                    match gh.approve(&repo, run_id).await {
                        Ok(()) => { let _ = tx.send(DataMsg::Action("Run approved".into())); }
                        Err(e) => { let _ = tx.send(DataMsg::Error(format!("approve: {e}"))); }
                    }
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
                let (gh, tx) = (gh.clone(), tx.clone());
                tokio::spawn(async move {
                    let state = if approve { "approved" } else { "rejected" };
                    match gh.review_deployments(&repo, run_id, &env_ids, state, &comment).await {
                        Ok(()) => {
                            let word = if approve { "approved" } else { "rejected" };
                            let _ = tx.send(DataMsg::Action(format!("Deployment {word}")));
                        }
                        Err(e) => { let _ = tx.send(DataMsg::Error(format!("review: {e}"))); }
                    }
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
