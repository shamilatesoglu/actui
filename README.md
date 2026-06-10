# actui

A fast, beautiful terminal UI for viewing and managing **GitHub Actions** across all the repos and orgs your account can see — without leaving the keyboard or opening a browser tab.

```
╭ actui @you  ● 3  ○ 1  ● 2  ● 44   48 runs ──────────────── api 4982/5000 · updated 12s ╮
 Runs › Jobs › Logs
 All  Running  Queued  Failed  Success
╭ Runs ────────────────────────────────────╮╭ Detail ───────────────────╮
▌● org/api      CI #296   main push 1m12s   ││ ● success  #296            │
 ● org/web      Deploy#88 main push  42s…   ││     repo  org/api          │
 ● you/dotfiles lint #5   main PR    18s    ││     flow  CI               │
 ◌ org/mobile   Release#3 v1.2 manual 3s    ││── Jobs ────────────────────│
                                             ││▌● build        1m12s       │
                                             ││ ● test         48s         │
╰─────────────────────────────────────────╯╰────────────────────────────╯
 j/k move · Tab focus · / search · ⏎/o open · l logs · d dispatch · c cancel · x/X rerun · v failures · A artifacts · r refresh · ? help · q quit
```

## Features

- **Aggregated view** of recent workflow runs across the repos you own and your org repos, sorted by latest activity. Each row shows the repo, workflow + run number, branch, trigger event, who triggered it, run **duration** (live-ticking while active), and age.
- **Live status** with color-coded states: running, queued, failed, success, cancelled, skipped. lazyactions-style panes: the focused pane gets an accent border and a highlighted (inverted) title tab; the unfocused pane dims its border and keeps a dimmed selection so you never lose your place. Popups float on a filled background.
- **Completion notifications** — a terminal bell plus a desktop toast the moment a watched run flips to success/failure/cancelled, so you can leave it running in the background. Configurable (`notify` / `bell`).
- **Automatic light/dark theme** — follows your OS appearance setting and switches live when you flip it. Pin it with `theme = "dark"` / `"light"` if you'd rather not auto-detect.
- **Two-pane navigation** (Runs ⟷ Jobs) with a `Runs › Jobs › Logs` breadcrumb; `Tab` moves focus, `j`/`k` move within the focused pane.
- **Filter** by status (`1`–`5`) and **fuzzy search** (`/`) across repo, workflow, and branch.
- **Job detail pane** that auto-loads the selected run's jobs with per-job durations.
- **Rich logs viewer**:
  - **live step view for running jobs** — GitHub's API doesn't expose in-progress log *text* (the log blob 404s until a job finishes), so for a running job actui shows its **steps updating in real time**: which step is running, each step's status, and a ticking elapsed timer. The full text logs **load automatically the moment the job completes**.
  - syntax-highlighted — GitHub `##[error]`/`##[warning]`/`##[group]` markers and embedded ANSI color
  - **foldable step tree** (`Enter`) — each `##[group]` step folds into a tree node showing its **line count and elapsed time**; error/warning steps auto-expand
  - **in-log search** (`/`, then `n`/`N`) that reveals folded matches
- **Failure annotations** (`v`) — the fastest path from a red run to its root cause. GitHub already distills each job's output into **check-run annotations** (the `file:line` error/warning boxes you see on a PR); actui aggregates them across the run's failed jobs into one panel, **color-coded** by level (failure / warning / notice) and tagged with the producing tool. Press `⏎` on any annotation to **jump straight into that job's logs, pre-searched** for the offending line — no scrolling through raw output. From the Jobs pane, `v` scopes to the focused job; a run with no failures falls back to surfacing its warnings.
- **Manage runs** without the browser:
  - `d` — trigger a `workflow_dispatch`: pick the workflow, then fill a **typed form** built from the workflow's declared inputs (text fields, boolean toggles, choice pickers — defaults pre-filled, required fields marked). On the **ref** field, press `Space`/`→` to open a **branch & tag picker** (fuzzy-filterable) instead of typing the ref by hand.
  - `c` — cancel a running run
  - `x` / `X` — re-run failed jobs / re-run all jobs; `R` — re-run just the selected job
  - `a` — approve a run that's held for approval. actui detects which kind it is and only offers the key when the run is actually awaiting approval:
    - **fork pull-request** awaiting maintainer approval → a confirm, then approve.
    - **environment deployment** gated by required reviewers → a **review picker**: select which environments to act on (`Space`), add an optional comment (`c`), then **approve** (`⏎`) or **reject** (`x`). Environments you aren't a reviewer for are shown but locked.
  - `A` — browse a run's **artifacts** and download one as a `.zip`
  - `o` — open the run on github.com
- **Resilient API client** — every request (polls *and* mutations) shares one rate-limit budget and back-off window, honors `Retry-After`/`X-RateLimit-Reset`, and **re-resolves an expired token** automatically mid-session. Repo pagination is fault-tolerant: a failing later page keeps the repos already fetched instead of dropping everything.
- **Fully automatic, thrifty refresh** — no refresh key, and deliberately frugal with requests:
  - **Two-tier polling** — a slow *broad sweep* of all repos (`refresh_secs`) catches new/finished runs; the jobs of **every active run** (bounded) poll on the fast cadence (`active_refresh_secs`), and only while something is still running. Idle = almost no traffic.
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
notify              = true  # desktop notification when a watched run finishes
bell                = true  # ring the terminal bell when a watched run finishes
theme               = "auto"  # "auto" follows the OS light/dark setting; or "dark" / "light"

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
| `h` / `←` / `Backspace` / `Esc` | back to Runs (`Backspace`/`Esc` also close any popup or the logs viewer) |
| `1`–`5`, `[` / `]` | status filter |
| `/` | fuzzy search runs (repo, workflow, branch) |
| `o` | open in browser — the selected job's page when Jobs is focused, otherwise the run |
| `L` | open the selected job's logs (works anywhere) |
| `d` | dispatch a workflow |
| `c` | cancel run |
| `x` / `X` | re-run failed / all jobs |
| `R` | re-run the selected job |
| `a` | approve a held run (fork-PR approval or environment deployment review) — only when awaiting approval |
| `A` | browse / download run artifacts |
| `r` / `F5` | refresh now |
| `E` | show repos that failed to load |
| `?` | help · `q` / `Ctrl-C` quit |

**Logs viewer**

| Key | Action |
|-----|--------|
| `j` / `k`, `g` / `G` | move cursor |
| `←` / `→` | scroll horizontally |
| `Enter` / `Space` | fold / unfold group |
| `e` / `f` | expand all / fold all |
| `/`, `n` / `N` | search, next / prev match |
| `s` | save the log to a file |
| `Esc` / `q` / `Backspace` | close logs |

Auto-refresh is always on; `r` / `F5` force an immediate sweep.

**Mouse**

The wheel scrolls whichever pane is under the pointer (runs, jobs, logs, or
any open picker). Clicking selects a run or job row, switches pane focus,
and picks a filter tab; a click also dismisses the help and error popups.

## License

MIT
