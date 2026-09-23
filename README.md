# actui

A fast, beautiful terminal UI for viewing and managing **GitHub Actions** across all the repos and orgs your account can see — without leaving the keyboard or opening a browser tab.

```
╭ actui @you  ● 3  ○ 1  ● 2  ● 44   48 runs ──────────────────────────────── api 99% · updated 12s ╮
 Repos › Runs › Jobs › Logs
 All  Running  Queued  Failed  Success
╭ Repos 1/6 ───────────────╮╭ Runs 1/48 ─────────────────────────────────╮╭ Detail ────────────────╮
│   All repos        ●3 ●2 ││     Repository    Workflow   Tag      Age  ││● success  #296         │
│ ★ org/api             ●3 ││▌●  org/api       CI #296     v0.4.0  1m12s ││ release  v0.4.0        │
│   org/infra              ││ ●  org/web       Deploy #88  nightly  42s  ││    repo  org/api       │
│   org/mobile          ○1 ││ ●  you/dotfiles  lint #5              18s  ││ Jobs ──────────────────│
│   org/web             ●1 ││ ○  org/mobile    Release #3           3s   ││▌● build        1m12s   │
│   you/dotfiles       12m ││                                            ││ ● test           48s   │
╰──────────────────────────╯╰────────────────────────────────────────────╯╰────────────────────────╯
 j/k move · Tab focus · / search · ⏎ jobs · l logs · d dispatch · c cancel · x/X rerun · v failures · p repos · ? help · q quit
```

## Features

- **Aggregated view** of recent workflow runs across the repos you own and your org repos, sorted by latest activity. Each row shows the repo, workflow + run number, branch, trigger event, who triggered it, run **duration** (live-ticking while active), and age.
- **Repos sidebar** — every repo you watch, in one predictable place, so you don't scroll a mixed list to find one. Scoping to one **reads further back into its history** (`scoped_runs`, 100 by default) than the sweep keeps for every repo, so you're not stuck with the last handful of runs. **Pinned repos first** (config order, marked `★`), then **alphabetical** — a repo stays where you last saw it. Set `sort = "used"` if you'd rather have the repos you work in most float to the top instead, ranked by selecting their runs, opening their logs, and dispatching in them, decayed week by week. Each row rolls up the repo's active and failed runs, or how long ago it last ran. Moving the cursor **scopes the runs list** to that repo (the now-redundant repository column makes way for the rest of the table); `Esc` goes back to *All repos*, `p` hides the sidebar, and a narrow terminal drops it on its own. A name too long for the pane **slides past** rather than being cut off. Because it lists **every watched repo — including ones with no recent runs** — `d` there dispatches a workflow in a repo you'd otherwise have to go to github.com for.
- **A layout you set stays set** — **drag any pane border** to resize the sidebar, the detail pane, or the live-steps pane, and **drag a column header's edge** to size that column of the runs table; `<` / `>` resize the sidebar from the keyboard, and `=` puts everything back to the defaults. Widths (and the repo history behind `sort = "used"`) are remembered in `~/.config/actui/state.toml`. Panes never squeeze the runs table below what it needs: when there isn't room for all of them — a narrow terminal, or the live-steps pane opening as a fourth column — the sidebar steps out until there is.
- **Live status** with color-coded states: running, queued, failed, success, cancelled, skipped. lazyactions-style panes: the focused pane gets an accent border and a highlighted (inverted) title tab; the unfocused pane dims its border and keeps a dimmed selection so you never lose your place. Popups float on a filled background.
- **Completion notifications** — a terminal bell plus a desktop toast the moment a watched run flips to success/failure/cancelled, so you can leave it running in the background. Configurable (`notify` / `bell`).
- **Automatic light/dark theme** — reads the **terminal's own background color**, so a dark profile on a light desktop (or a light one reached over ssh) gets the palette that suits the screen you're actually looking at. Terminals that don't answer fall back to your OS appearance setting, which still switches live when you flip it. Pin it with `theme = "dark"` / `"light"` if you'd rather not auto-detect.
- **Three-pane navigation** (Repos ⟷ Runs ⟷ Jobs) with a `Repos › Runs › Jobs › Logs` breadcrumb; `Tab` moves focus, `j`/`k` move within the focused pane.
- **Filter** by status (`1`–`5`) and **fuzzy search** (`/`) across repo, workflow, and branch.
- **Job detail pane** that auto-loads the selected run's jobs with per-job durations.
- **What a run shipped** — a tag that lands on a run's head commit is that run's doing, so the detail pane names it right under the run's status: a green run tells you what it actually produced, not just that it passed. A **tag** on its own counts; once a **release** is published under it the row says so and picks up the release's name (and a `pre` marker for a prerelease). Both **appear mid-run**, the moment the workflow makes them, because the lookup rides the same fast cadence as the live job polling — and a bare tag is still watched, so it upgrades itself the instant the release lands. The runs table gets a **`Tag` column** too, which shows up only when the table has width to spare *and* something on the list actually tagged, with released tags in the accent color so they stand out from plain ones. Thrifty by construction: one tag list answers for a whole repo, it's ETag-conditional so repeat looks come back `304` and don't touch your rate limit, only a commit that really carries a tag costs a release lookup, and an answer holds for the session. `t` opens it on github.com.
- **Rich logs viewer**:
  - **live step view for running jobs** — GitHub's API doesn't expose in-progress log *text* (the log blob 404s until a job finishes), so for a running job actui shows its **steps updating in real time**: which step is running, each step's status, and a ticking elapsed timer. The full text logs **load automatically the moment the job completes**.
  - syntax-highlighted — GitHub `##[error]`/`##[warning]`/`##[group]` markers and embedded ANSI color
  - **foldable step tree** (`Enter`) — each `##[group]` step folds into a tree node showing its **line count and elapsed time**; error/warning steps auto-expand
  - **in-log search** (`/`, then `n`/`N`) that reveals folded matches
- **Screen switcher** — the tabs row carries the run filters on the left and the screens on the right: **⚙ Runners** opens the org self-hosted runners view (`s` does too) and **◷ Duration** the duration chart (`w`); each marks itself while you're there and takes you back. The sidebar stays beside them, so your repo scope is where you left it.
- **Duration chart** (`w`) — whether a workflow is getting slower, in one screen. It charts the **successful runs of one workflow** — the only cut where durations compare — reading that workflow's last 100 successes rather than the handful the sweep keeps, so there's real history behind it. Oldest on the left, newest against the right edge, the **median drawn across them** and a **smoothed curve** over them — a running median, so the one run that hung doesn't drag it — and under it the spread plus a **trend arrow**: the median of the recent runs against the ones before, so a workflow drifting 12% slower says so. Bars widen to fill the terminal. The axis runs from the quickest run to the slowest and **names both ends** — CI durations cluster, and an axis from zero would spend its height on the part that never changes; the odd run that hangs is capped with `↑` rather than being allowed to flatten everything else. More runs than columns and each column stands for several, drawn at its longest so a spike survives. A run GitHub has housekept since — its logs or artifacts reached the repo's retention, which moves the timestamp a duration is read from — is counted but left off, rather than plotted as a 9600-hour run; the runs table shows `—` for the same reason. `j`/`k` (or `h`/`l`) walk the bars and the line underneath names that run; `⏎` goes back to it in the runs list, `o` opens it on github.com, and `[` / `]` move to another workflow in the repo. Costs one ETag-conditional request, so coming back is free.
- **Failure annotations** (`v`) — the fastest path from a red run to its root cause. GitHub already distills each job's output into **check-run annotations** (the `file:line` error/warning boxes you see on a PR); actui aggregates them across the run's failed jobs into one panel, **color-coded** by level (failure / warning / notice) and tagged with the producing tool. Press `⏎` on any annotation to **jump straight into that job's logs, pre-searched** for the offending line — no scrolling through raw output. From the Jobs pane, `v` scopes to the focused job; a run with no failures falls back to surfacing its warnings.
- **Manage runs** without the browser:
  - `d` — trigger a `workflow_dispatch` (the run appears immediately, spinning where its number will be until GitHub registers it — actui won't invent one): pick the workflow, then fill a **typed form** built from the workflow's declared inputs (text fields, boolean toggles, choice pickers — defaults pre-filled, required fields marked). On the **ref** field, press `Space`/`→` to open a **branch & tag picker** (fuzzy-filterable) instead of typing the ref by hand.
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
  - Rate-limit numbers come from response headers (no extra `/rate_limit` request), and at most `concurrency` repos (default 10) are read at once.
- **Rate-limit aware** — caps the number of repos scanned so large org memberships don't exhaust your API quota.

## Install

Requires the [GitHub CLI](https://cli.github.com/) (`gh`) for auth, or a `GITHUB_TOKEN`.

```sh
cargo install actui
# or, from a checkout:
cargo install --path .
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
runs_per_repo       = 15  # recent runs pulled per repo, across the sweep
scoped_runs         = 100 # and for the repo the sidebar is scoped to (max 100)
concurrency         = 10  # repos fetched in parallel
max_repos           = 60  # cap, from most-recently-pushed repos (0 = no cap)
skip_archived       = true
notify              = true  # desktop notification when a watched run finishes
bell                = true  # ring the terminal bell when a watched run finishes
theme               = "auto"  # "auto" reads the terminal's background, else the OS setting; or "dark" / "light"
sidebar             = true  # show the repos sidebar (`p` toggles it, drag or `<`/`>` resize it)
sort                = "alpha"  # sidebar order below the pinned repos; or "used"

# Repos kept at the top of the sidebar, in this order:
pinned = []             # e.g. ["my-org/api", "you/dotfiles"]

# Only watch repos whose full name contains one of these (empty = all):
include = []            # e.g. ["my-org/", "you/important-repo"]
# Always exclude repos whose full name contains one of these:
exclude = []            # e.g. ["fork-of-"]
```

> **Tip:** If you belong to large orgs, set `include` to the orgs/repos you actually care about, or lower `max_repos`, to stay well under the 5000 req/hour API limit.

## Keys

**Repos / Runs / Jobs panes**

| Key | Action |
|-----|--------|
| `j` / `k`, `↑` / `↓` | move within the focused pane |
| `g` / `G` | top / bottom |
| `Tab` | move focus one pane along (`Shift-Tab` goes back) |
| `p` | show / hide the repos sidebar |
| `s` | org self-hosted runners — or click **⚙ Runners** on the tabs row |
| `w` | duration chart for the selected run's workflow — or click **◷ Duration** |
| `  ↳` in the chart | `h` / `l` move · `⏎` go to that run · `o` open it · `[` / `]` another workflow |
| `<` / `>` | narrow / widen the repos sidebar |
| `=` | reset the pane and column widths |
| `Enter` / `l` / `→` | drill in: Repos → Runs → Jobs, or open a job's logs |
| `h` / `←` / `Backspace` | back out: Jobs → Runs → Repos (`Backspace` also closes any popup or the logs viewer) |
| `Esc` | clear the search, then the repo scope, then focus (also closes popups) |
| `1`–`5`, `[` / `]` | status filter |
| `/` | fuzzy search runs (repo, workflow, branch) |
| `o` | open in browser — the selected job's page when Jobs is focused, otherwise the run |
| `t` | open the tag / release this run produced, if it produced one |
| `L` | open the selected job's logs (works anywhere) |
| `d` | dispatch a workflow — in the repo the sidebar is on, else the selected run's |
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

The wheel scrolls whichever pane is under the pointer (repos, runs, jobs, logs,
or any open picker). Clicking selects a repo, run, or job row, switches pane
focus, and picks a filter tab; a click also dismisses the help and error popups.

**Dragging** a border between panes resizes them, and dragging the edge of a
column header resizes that column of the runs table. Both are remembered
between sessions; `=` puts them back to the defaults.

**Scrollbars** work the way scrollbars do: drag the thumb, press the track above
or below it to page, press an arrow to step a row, or roll the wheel over the
bar. The repos, runs, runners, and log panes all have one.

## License

MIT
