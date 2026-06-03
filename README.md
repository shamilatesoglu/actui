# actui

A fast, beautiful terminal UI for viewing and managing **GitHub Actions** across all the repos and orgs your account can see — without leaving the keyboard or opening a browser tab.

```
╭ actui @you  ▶ 3  ◌ 1  ✘ 2  ✔ 44   48 runs ──────────────── api 4982/5000 · updated 12s ╮
 All  Running  Queued  Failed  Success
╭ Runs ───────────────────────────────────╮╭ Detail ───────────────────╮
▌✔ org/api          CI            main  …  ││ ✔ success  #296            │
 ▶ org/web          Deploy        main  …  ││     repo  org/api          │
 ✘ you/dotfiles     lint          main  …  ││     flow  CI               │
 ◌ org/mobile       Release       v1.2  …  ││   branch  main             │
                                            ││ ── Jobs ──────────────────│
                                            ││ ▌✔ build        1m12s      │
                                            ││  ✔ test         48s        │
╰────────────────────────────────────────╯╰───────────────────────────╯
 j/k move · [/] job · Tab filter · / search · ⏎/o open · l logs · d dispatch · c cancel · x/X rerun · r refresh · ? help · q quit
```

## Features

- **Aggregated view** of recent workflow runs across the repos you own and your org repos, sorted by latest activity.
- **Live status** with color-coded states: running, queued, failed, success, cancelled, skipped.
- **Two-pane navigation** (Runs ⟷ Jobs) with a `Runs › Jobs › Logs` breadcrumb; `Tab` moves focus, `j`/`k` move within the focused pane.
- **Filter** by status (`1`–`5`) and **fuzzy search** (`/`) across repo, workflow, and branch.
- **Job detail pane** that auto-loads the selected run's jobs with per-job durations.
- **Rich logs viewer**:
  - **live step view for running jobs** — GitHub's API doesn't expose in-progress log *text* (the log blob 404s until a job finishes), so for a running job actui shows its **steps updating in real time**: which step is running, each step's status, and a ticking elapsed timer. The full text logs **load automatically the moment the job completes**.
  - syntax-highlighted — GitHub `##[error]`/`##[warning]`/`##[group]` markers and embedded ANSI color
  - **foldable step tree** (`Enter`) — each `##[group]` step folds into a tree node showing its **line count and elapsed time**; error/warning steps auto-expand
  - **in-log search** (`/`, then `n`/`N`) that reveals folded matches
- **Manage runs** without the browser:
  - `d` — trigger a `workflow_dispatch`: pick the workflow, then fill a **typed form** built from the workflow's declared inputs (text fields, boolean toggles, choice pickers — defaults pre-filled, required fields marked)
  - `c` — cancel a running run
  - `x` / `X` — re-run failed jobs / re-run all jobs
  - `o` — open the run on github.com
- **Fully automatic, thrifty refresh** — no refresh key, and deliberately frugal with requests:
  - **Two-tier polling** — a slow *broad sweep* of all repos (`refresh_secs`) catches new/finished runs; only the **selected run's jobs** poll on the fast cadence (`active_refresh_secs`), and only while that run is still running. Idle = almost no traffic.
  - **Conditional requests (ETags)** — every poll sends `If-None-Match`; unchanged resources return `304 Not Modified`, which **doesn't count against the rate limit**.
  - **Automatic back-off** — on a primary or secondary rate limit (`403`/`429`), all polling pauses until `Retry-After`/`X-RateLimit-Reset` clears, shown in the header (`rate-limited · resuming in 42s`). It also eases off when remaining quota is low.
  - Rate-limit numbers come from response headers (no extra `/rate_limit` request), and concurrency is kept low (default 3) to avoid request bursts.
- **Rate-limit aware** — caps the number of repos scanned so large org memberships don't exhaust your API quota.

## Install

Requires the [GitHub CLI](https://cli.github.com/) (`gh`) for auth, or a `GITHUB_TOKEN`.

```sh
cargo install --path .
# or, once published:
# cargo install actui
```

## Auth

actui uses, in order:

1. `$GITHUB_TOKEN` / `$GH_TOKEN`
2. `gh auth token` (run `gh auth login` once)

The token needs `repo` and `workflow` scopes to manage Actions.

## Configuration

Optional, at `~/.config/actui/config.toml` (Windows: `%APPDATA%\actui\config.toml`):

```toml
refresh_secs        = 45  # auto-refresh interval when everything is idle
active_refresh_secs = 10  # faster interval while a run is queued/in progress
runs_per_repo       = 15  # recent runs pulled per repo
concurrency         = 8   # repos fetched in parallel
max_repos           = 60  # cap, from most-recently-pushed repos (0 = no cap)
skip_archived       = true

# Only watch repos whose full name contains one of these (empty = all):
include = []            # e.g. ["my-org/", "you/important-repo"]
# Always exclude repos whose full name contains one of these:
exclude = []            # e.g. ["fork-of-"]
```

> **Tip:** If you belong to large orgs, set `include` to the orgs/repos you actually care about, or lower `max_repos`, to stay well under the 5000 req/hour API limit.

## Keys

**Runs / Jobs panes**

| Key | Action |
|-----|--------|
| `j` / `k`, `↑` / `↓` | move within the focused pane |
| `g` / `G` | top / bottom |
| `Tab` | switch focus between Runs and Jobs |
| `Enter` / `l` / `→` | drill Runs → Jobs, or open a job's logs |
| `h` / `←` / `Esc` | back to Runs |
| `1`–`5`, `[` / `]` | status filter |
| `/` | fuzzy search runs |
| `o` | open run in browser |
| `d` | dispatch a workflow |
| `c` | cancel run |
| `x` / `X` | re-run failed / all |
| `?` | help · `q` / `Ctrl-C` quit |

**Logs viewer**

| Key | Action |
|-----|--------|
| `j` / `k`, `g` / `G` | move cursor |
| `Enter` / `Space` | fold / unfold group |
| `e` / `f` | expand all / fold all |
| `/`, `n` / `N` | search, next / prev match |
| `Esc` / `q` | close logs |

Refreshing is automatic — there is no refresh key.

## License

MIT
