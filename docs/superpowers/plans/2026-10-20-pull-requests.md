# Pull Requests Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A PRs screen in actui listing the user's open PRs and review requests, a PR screen with overview, diff and conversation, and acting on PRs (comments, line-comment reviews, merge, merge queue, draft, update branch, re-run checks).

**Architecture:** PR API calls and models live in a new `src/pulls/` module (an `impl Github` block plus a worker that runs PR commands). PR state, keys and the diff model live in `src/app/prs/`; drawing in `src/ui/prs/`. They talk over one new `Command::Pr(PrCommand)` and one `DataMsg::Pr(PrMsg)` variant, so `protocol.rs`, `app.rs`, `ui.rs` and `main.rs` gain only a few lines each.

**Tech Stack:** Rust 2021, ratatui 0.29, crossterm 0.28, tokio, reqwest 0.12, serde_json, GitHub REST + GraphQL, `tui-textarea` 0.7 (added in Task 7).

**Spec:** `docs/superpowers/specs/2026-10-20-pull-requests-design.md`

## Global Constraints

- Same token as today (`repo`, `workflow` scopes). No new auth.
- New dependencies: only `tui-textarea` (approved). Use the 0.7 line, which builds on ratatui 0.29 and crossterm 0.28. Anything else: ask the user.
- `src/ui.rs`, `src/app.rs`, `src/app/input.rs`, `src/main.rs`, `src/github.rs` gain only wiring lines. PR logic goes in the new modules.
- Keys must work in Windows Terminal, macOS and Linux terminals. Send is `Ctrl+S`, single comment is `Ctrl+G`. No `Ctrl+Enter`.
- UI follows existing conventions: `pane()`, `popup()`, `select_style()`, `scrollbar()`, `kv()`, the theme accessors (`accent()`, `dim()`, `text()`, …), the footer hint line, and `?` help entries.
- Polling respects the existing `throttled` gate in `main.rs` (rate-limit back-off or quota below 50).
- Every GraphQL query and mutation is checked against the live schema before use: run it once with `gh api graphql -f query='…'` on a real PR. The field names in this plan are my best reading of the schema, not verified.
- Tests: `TestBackend` screen tests via the `screen()` helper in `src/ui.rs` tests, unit tests on state types, and JSON-fixture parsing tests. No HTTP mocking.
- User's rules for commits: **ask the user before every commit**. Subjects are verb first, sentence case, no period, no area prefix, and describe what a person sees, not the code. Bodies wrap at 80 and have no attribution trailers. No `@` anywhere in a message.
- Plain English in comments and UI copy. Match the surrounding code's comment density and naming.

## Review Focus

1. **PR descriptions written from templates**: HTML comments (`<!-- … -->`), `\r\n` line endings, tables, very long lines. Expected: comments hidden, no stray `\r`, long lines wrap. Pinned in Task 3.
2. **Large PRs**: files GitHub sends without a `patch` (binary, too large), more than 100 files, diffs of tens of thousands of lines. Expected: a plain message with `o` for the browser, all files listed, scrolling stays instant. Pinned in Task 5.
3. **Someone pushes while you review or merge.** The head commit changes, so GitHub refuses the review or merge. Expected: the error shows, the PR refreshes, and unsent comments survive. Pinned in Tasks 8 and 9.
4. **PRs from forks.** The head branch lives in another repo. Expected: "delete branch" isn't offered. Pinned in Task 8.
5. **Tabs and wide characters in diff lines.** Expected: columns stay aligned. Tabs are drawn as 4 spaces and nothing writes past the pane. Pinned in Task 5.

---

## File Structure

| File | Responsibility |
|------|----------------|
| `src/pulls.rs` | Declares `api`, `model`, `worker`; re-exports `model::*` and `worker::run_command`. |
| `src/pulls/model.rs` | PR data types (`PrRef`, `PrSummary`, `PrDetail`, `PrFile`, …) and the inputs mutations take. |
| `src/pulls/api.rs` | `impl Github`: `graphql()`, list/detail/files reads, every PR mutation; JSON → model parsing. |
| `src/pulls/worker.rs` | `run_command(cmd, gh, tx)`: spawns the task for each `PrCommand`, sends `PrMsg`s back. |
| `src/app/prs.rs` | `PrsState`, `PrList`, `PrView`, `Loadable`, `PendingReview`; `App::apply_pr()`. Declares submodules. |
| `src/app/prs/protocol.rs` | `PrCommand`, `PrAction`, `PrActionKind`, `PrMsg`. |
| `src/app/prs/diff.rs` | `DiffLine`, `parse_patch()`, `DiffView` (cursor, range mark, search), thread anchoring. |
| `src/app/prs/compose.rs` | `Compose` (wraps `tui_textarea::TextArea`) and `ComposeTarget`. |
| `src/app/prs/input.rs` | `impl App`: keys and mouse for the PRs and PR screens, the compose box and PR overlays. |
| `src/ui/prs.rs` | Draws the PRs list and the PR screen frame and tabs. Declares submodules. |
| `src/ui/prs/overview.rs` | Overview tab, checks list, `markdown_lines()`. |
| `src/ui/prs/diff.rs` | Files list and diff view. |
| `src/ui/prs/conversation.rs` | Conversation tab, thread popup. |
| `src/ui/prs/overlays.rs` | Compose box, merge dialog, verdict picker, pending-review panel. |
| `src/github.rs` | Only `pub(crate)` on `Github::client`, `send`, `cond_get`, `Cond`, `API`, `ensure_ok`, `check`; `workflow_run()`. |
| `src/app.rs`, `src/app/protocol.rs`, `src/app/input.rs`, `src/ui.rs`, `src/main.rs` | Wiring only: new `Mode::Prs`, the `Pr` command and message variants, the `P` key, the tab, the body branch, the confirm refactor, run-jump filter, refresh cadence. |

---

### Task 1: PR module, GraphQL helper and list parsing

**Files:**
- Create: `src/pulls.rs` (declares only `api` and `model` for now; Task 2 adds `worker`), `src/pulls/model.rs`, `src/pulls/api.rs`
- Modify: `src/main.rs` (add `mod pulls;`), `src/github.rs` (visibility only)

**Interfaces:**
- Produces (in `pulls::model`):
  - `#[derive(Clone, PartialEq, Eq, Hash, Debug)] pub struct PrRef { pub repo: String, pub number: u64 }` with `fn url(&self) -> String` (`https://github.com/{repo}/pull/{number}`) and `fn label(&self) -> String` (`{repo}#{number}`).
  - `#[derive(Clone, Copy, PartialEq, Eq, Debug)] pub enum PrGroup { ReviewRequested, Authored }` with `const ALL: [PrGroup; 2]`, `fn query(self) -> &'static str`, `fn title(self) -> &'static str`.
  - `pub enum ChecksState { Pending, Passing, Failing, NoChecks }` (Copy, PartialEq, Debug).
  - `pub enum ReviewDecision { Approved, ChangesRequested, ReviewRequired, NotRequired }` (Copy, PartialEq, Debug).
  - `#[derive(Clone, Debug)] pub struct PrSummary { pub pr: PrRef, pub node_id: String, pub title: String, pub author: String, pub updated_at: DateTime<Utc>, pub draft: bool, pub checks: ChecksState, pub review: ReviewDecision }`.
- Produces (in `pulls::api`):
  - `fn parse_graphql<T: DeserializeOwned>(body: &str) -> anyhow::Result<T>`: returns `data`. If `errors` is non-empty, returns `Err` with the first error's `message`.
  - `impl Github { pub(crate) async fn graphql<T: DeserializeOwned>(&self, query: &str, variables: serde_json::Value) -> Result<T> }`: POST `https://api.github.com/graphql` through `self.send`, then `ensure_ok`, then `parse_graphql`.
  - `fn parse_search(data: serde_json::Value) -> Result<Vec<PrSummary>>`.
  - `impl Github { pub async fn search_prs(&self, group: PrGroup) -> Result<Vec<PrSummary>> }`.

Exact values:
- `PrGroup::ReviewRequested.query()` = `"is:pr is:open archived:false review-requested:@me sort:updated-desc"`, title `"Review requested"`.
- `PrGroup::Authored.query()` = `"is:pr is:open archived:false author:@me sort:updated-desc"`, title `"Opened by you"`.
- Search query: `search(query: $q, type: ISSUE, first: 50)`. Each node `... on PullRequest` takes `id number title url isDraft updatedAt reviewDecision author{login} repository{nameWithOwner} commits(last:1){nodes{commit{statusCheckRollup{state}}}}`.
- Checks mapping: `SUCCESS` → `Passing`. `FAILURE`/`ERROR` → `Failing`. `PENDING`/`EXPECTED` → `Pending`. No rollup → `NoChecks`.
- Review decision mapping: `APPROVED`, `CHANGES_REQUESTED`, `REVIEW_REQUIRED`, null → `NotRequired`.
- A deleted user's PR has `author: null`, which maps to `"ghost"`.

- [ ] **Step 1: Write the failing tests** in `src/pulls/api.rs`:

```rust
#[test]
fn graphql_errors_become_the_error_message() {
    let body = r#"{"data":null,"errors":[{"message":"Field 'nope' doesn't exist"}]}"#;
    let err = parse_graphql::<serde_json::Value>(body).unwrap_err();
    assert_eq!(err.to_string(), "Field 'nope' doesn't exist");
}

#[test]
fn search_results_become_summaries() {
    let data = serde_json::json!({"search":{"nodes":[
        {"id":"PR_1","number":12,"title":"Fix login","url":"https://github.com/org/api/pull/12",
         "isDraft":true,"updatedAt":"2026-10-20T09:00:00Z","reviewDecision":"CHANGES_REQUESTED",
         "author":{"login":"ana"},"repository":{"nameWithOwner":"org/api"},
         "commits":{"nodes":[{"commit":{"statusCheckRollup":{"state":"FAILURE"}}}]}},
        {"id":"PR_2","number":3,"title":"Docs","url":"https://github.com/me/tool/pull/3",
         "isDraft":false,"updatedAt":"2026-10-19T09:00:00Z","reviewDecision":null,
         "author":null,"repository":{"nameWithOwner":"me/tool"},
         "commits":{"nodes":[{"commit":{"statusCheckRollup":null}}]}},
        {}
    ]}});
    let prs = parse_search(data).unwrap();
    assert_eq!(prs.len(), 2, "non-PR nodes are skipped");
    assert_eq!(prs[0].pr, PrRef { repo: "org/api".into(), number: 12 });
    assert!(prs[0].draft);
    assert_eq!(prs[0].checks, ChecksState::Failing);
    assert_eq!(prs[0].review, ReviewDecision::ChangesRequested);
    assert_eq!(prs[1].author, "ghost");
    assert_eq!(prs[1].checks, ChecksState::NoChecks);
    assert_eq!(prs[1].review, ReviewDecision::NotRequired);
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test --quiet pulls::`
Expected: compile errors for the missing module and functions.

- [ ] **Step 3: Implement**: the files above, `mod pulls;` in `main.rs`, and `pub(crate)` on the `github.rs` items listed in File Structure. Parse with private `#[derive(Deserialize)]` structs that mirror the JSON, then convert to the models. An empty object `{}` (non-PR node) fails to deserialize and is skipped.

- [ ] **Step 4: Run the tests**

Run: `cargo test --quiet`
Expected: all pass, no warnings from `cargo clippy --quiet` beyond dead-code allowances you mark `#[allow(dead_code)]` with a one-line reason until Task 2 uses them.

- [ ] **Step 5: Ask the user, then commit.** Subject: `Read open pull requests and review requests from GitHub`

---

### Task 2: PRs list screen

**Files:**
- Create: `src/app/prs.rs`, `src/app/prs/protocol.rs`, `src/app/prs/input.rs`, `src/pulls/worker.rs`, `src/ui/prs.rs`
- Modify: `src/app.rs` (`mod prs;`, `pub use prs::*`, `Mode::Prs`, field `pub prs: PrsState`, `apply` arm `DataMsg::Pr(m) => self.apply_pr(m)`, `queue_broad_refresh` also pushes `Command::Pr(PrCommand::FetchList)`, `HitMap.prs_tab: Rect`, `HitMap.prs_pane: Rect`), `src/app/protocol.rs` (`Command::Pr(PrCommand)`, `DataMsg::Pr(PrMsg)`), `src/app/input.rs` (`Mode::Prs => self.key_prs(key)`, `KeyCode::Char('P') => self.open_prs()` in `key_normal`, mouse on `prs_tab`/`prs_pane`), `src/ui.rs` (`draw_tabs`: `elsewhere` includes `Mode::Prs`, a third `screen_tab`; `draw_body`: `Mode::Prs` branch beside the sidebar like Runners; footer hints), `src/main.rs` (`Command::Pr(cmd) => pulls::run_command(cmd, gh, tx)`)

**Interfaces:**
- Consumes: Task 1 models and `Github::search_prs`.
- Produces:
  - `pub enum Loadable<T> { Loading, Ready(T), Failed(String) }`. A failed refresh of something already `Ready` keeps it `Ready` and reports the error in the status line.
  - `pub enum PrCommand { FetchList, … }` (later tasks add variants) and `pub enum PrMsg { List { group: PrGroup, result: Result<Vec<PrSummary>, String> }, … }`.
  - `pub struct PrList { pub review_requested: Loadable<Vec<PrSummary>>, pub authored: Loadable<Vec<PrSummary>>, pub state: ListState }` with:
    - `fn set(&mut self, group: PrGroup, result: Result<Vec<PrSummary>, String>)`: keeps the cursor on the same `PrRef`, else the first PR.
    - `fn rows(&self) -> Vec<PrRow<'_>>` where `pub enum PrRow<'a> { Header { group: PrGroup, detail: String }, Pr(&'a PrSummary) }`. A PR in both groups appears only under Review requested. Header detail is `"loading…"`, the error text, `"none"`, or the count.
    - `fn selected(&self) -> Option<&PrSummary>`, `fn move_sel(&mut self, delta: i32)` (skips headers), `fn click_row(&mut self, row_from_top: usize)`, `fn waiting(&self) -> usize` (count of Review requested).
  - `pub struct PrsState { pub list: PrList, pub open: Option<PrView> /* Task 3 */, … }`.
  - `pub fn run_command(cmd: PrCommand, gh: &Github, tx: &UnboundedSender<DataMsg>)`: `FetchList` spawns one task per group.
- Keys on the list: `j/k/↑/↓/PgUp/PgDn/g/G` move. `Enter` opens the PR (Task 3; until then, does nothing). `o` opens `pr.url()`. `r` refetches. `Esc/q/P/Backspace` go back to the runs. `1`–`5` go back to the runs and set that filter, as Runners does.
- Row layout: `{checks glyph} {repo short}#{number}  {title}  {author}  {age}  {review}`, with `draft` dimmed before the title. Use `status_glyph`-like glyphs: Passing `●` green, Failing `●` red, Pending spinner yellow, NoChecks `·` dim. Review text: `approved` green, `changes requested` red, `review required` yellow, nothing for NotRequired.
- Tab: `" ⇄ PRs  P "`, or `" ⇄ PRs {n}  P "` when `waiting() > 0`. Placed left of Duration.

- [ ] **Step 1: Write the failing tests**. In `src/app/prs.rs`, `fn summary(repo: &str, n: u64) -> PrSummary` test helper, then:

```rust
#[test]
fn a_pr_in_both_groups_lists_once_under_review_requested() {
    let mut list = PrList::default();
    list.set(PrGroup::ReviewRequested, Ok(vec![summary("org/api", 1)]));
    list.set(PrGroup::Authored, Ok(vec![summary("org/api", 1), summary("me/tool", 2)]));
    let prs: Vec<String> = list.rows().iter().filter_map(|r| match r {
        PrRow::Pr(p) => Some(p.pr.label()), _ => None }).collect();
    assert_eq!(prs, ["org/api#1", "me/tool#2"]);
    assert_eq!(list.waiting(), 1);
}

#[test]
fn a_refresh_keeps_the_cursor_on_its_pr() {
    let mut list = PrList::default();
    list.set(PrGroup::Authored, Ok(vec![summary("a/a", 1), summary("a/a", 2)]));
    list.move_sel(1);
    list.set(PrGroup::Authored, Ok(vec![summary("a/a", 3), summary("a/a", 1), summary("a/a", 2)]));
    assert_eq!(list.selected().unwrap().pr.number, 2);
}

#[test]
fn a_failed_refresh_keeps_what_was_shown() {
    let mut list = PrList::default();
    list.set(PrGroup::Authored, Ok(vec![summary("a/a", 1)]));
    list.set(PrGroup::Authored, Err("502".into()));
    assert!(matches!(list.authored, Loadable::Ready(ref v) if v.len() == 1));
}
```

In `src/ui.rs` tests:

```rust
#[test]
fn p_opens_the_prs_screen_with_both_groups() {
    let mut app = demo_app();
    app.handle_key(KeyEvent::from(KeyCode::Char('P')));
    app.apply(DataMsg::Pr(PrMsg::List { group: PrGroup::ReviewRequested, result: Ok(vec![/* org/api#7 "Fix login" */]) }));
    app.apply(DataMsg::Pr(PrMsg::List { group: PrGroup::Authored, result: Ok(vec![]) }));
    let out = screen(&mut app, 140, 24);
    assert!(out.contains("Review requested"));
    assert!(out.contains("api#7"));
    assert!(out.contains("Fix login"));
    assert!(out.contains("Opened by you  none"));
    assert!(out.contains("⇄ PRs 1  P"), "the tab counts what waits on you");
}

#[test]
fn the_tabs_row_switches_to_the_prs_screen() { /* click the "⇄ PRs" tab → Mode::Prs; click a filter tab → Mode::Normal */ }
```

- [ ] **Step 2: Run them to see them fail**: `cargo test --quiet prs` gives compile errors.
- [ ] **Step 3: Implement** the interfaces above. `open_prs()` sets `Mode::Prs` and pushes `FetchList`.
- [ ] **Step 4: Run all tests and clippy**: `cargo test --quiet && cargo clippy --quiet`. All pass, clippy clean.
- [ ] **Step 5: Ask the user, then commit.** Subject: `Add a screen listing your open PRs and review requests`

---

### Task 3: Opening a PR: overview

**Files:**
- Modify: `src/pulls/model.rs`, `src/pulls/api.rs`, `src/pulls/worker.rs`, `src/app/prs.rs`, `src/app/prs/protocol.rs`, `src/app/prs/input.rs`, `src/ui/prs.rs`, `src/main.rs` (open-PR refresh cadence)
- Create: `src/ui/prs/overview.rs`

**Interfaces:**
- Produces (model):
  - `pub struct PrDetail { pub node_id: String, pub title: String, pub body: String, pub author: String, pub url: String, pub head: HeadRef, pub base_branch: String, pub draft: bool, pub labels: Vec<String>, pub merge_status: MergeStatus, pub reviewers: Vec<Reviewer>, pub checks: Vec<Check>, pub comments: Vec<Comment>, pub reviews: Vec<Review>, pub threads: Vec<ReviewThread>, pub merge_methods: Vec<MergeMethod>, pub queue: QueueState }`
  - `pub struct HeadRef { pub branch: String, pub repo: String, pub oid: String }`
  - `pub enum MergeStatus { Clean, Behind, Blocked, Conflicts, Unstable, Draft, Unknown }`. Mapping from `mergeStateStatus`: `CLEAN`/`HAS_HOOKS` → Clean, `BEHIND`, `BLOCKED`, `DIRTY` → Conflicts, `UNSTABLE`, `DRAFT`, anything else → Unknown.
  - `pub struct Reviewer { pub login: String, pub verdict: Verdict }`, `pub enum Verdict { Requested, Approved, ChangesRequested, Commented, Dismissed }`. Requested reviewers come from `reviewRequests` (a team shows as its slug). Decided reviewers come from `latestOpinionatedReviews`.
  - `pub struct Check { pub name: String, pub state: CheckState, pub target: CheckTarget }`, `pub enum CheckState { Pending, Passed, Failed, Skipped, Neutral }`, `pub enum CheckTarget { ActionsRun { run_id: u64 }, Link(String), Nothing }`. A `CheckRun` whose `checkSuite.workflowRun.databaseId` is set becomes `ActionsRun`; else its `detailsUrl`; a `StatusContext` becomes its `targetUrl`.
  - `pub struct Comment { pub id: u64 /* databaseId */, pub author: String, pub body: String, pub created_at: DateTime<Utc> }`.
  - `pub struct Review { pub author: String, pub verdict: Verdict, pub body: String, pub submitted_at: DateTime<Utc> }`.
  - `pub struct ReviewThread { pub id: String, pub resolved: bool, pub outdated: bool, pub path: String, pub line: Option<u32>, pub start_line: Option<u32>, pub side: Side, pub comments: Vec<Comment> }`, `pub enum Side { Left, Right }`.
  - `pub enum MergeMethod { Merge, Squash, Rebase }`, with `fn api_name(self) -> &'static str` returning `"merge"`/`"squash"`/`"rebase"`. Built from `repository{mergeCommitAllowed squashMergeAllowed rebaseMergeAllowed viewerDefaultMergeMethod}`, the default first.
  - `pub enum QueueState { NoQueue, NotQueued, Queued { position: u32 } }`, from `isMergeQueueEnabled` and `mergeQueueEntry{position}`.
  - `fn blocked_reason(d: &PrDetail) -> Option<String>`: for `Blocked`, `"changes requested"` if any reviewer has ChangesRequested, else `"checks failing"` if any check Failed, else `"review required"` if no Approved, else `"branch protection"`. `None` when not Blocked.
- Produces (api): `fn parse_detail(data: serde_json::Value) -> Result<PrDetail>`; `impl Github { pub async fn pr_detail(&self, pr: &PrRef) -> Result<PrDetail> }`. One query on `repository(owner,name){pullRequest(number)}` reads everything above: `reviewThreads(first:100)` with `comments(first:50)`, `comments(last:100)` for issue comments, `latestOpinionatedReviews(first:50)`, `reviews(last:100)` for the conversation, and `commits(last:1){nodes{commit{statusCheckRollup{contexts(first:100){nodes{…}}}}}}`.
- Produces (app):
  - `PrCommand::FetchDetail(PrRef)` and `PrMsg::Detail { pr: PrRef, result: Result<Box<PrDetail>, String> }`.
  - `pub struct PrView { pub pr: PrRef, pub detail: Loadable<Box<PrDetail>>, pub tab: PrTab, pub overview_scroll: u16, pub checks: ListState, … }` and `pub enum PrTab { Overview, Files, Conversation }`.
  - `impl PrsState { pub fn open_checks_pending(&self) -> bool }`.
- Produces (ui): `pub(super) fn markdown_lines(text: &str) -> Vec<Line<'static>>`. It strips `<!-- … -->` (even across lines) and `\r`, bolds `#` headings in `accent()`, draws fenced code blocks in `pale()` without the fence lines, keeps `- `/`* ` bullets as `• `, and writes `[text](url)` as `text (url)`. Wrapping is done by the caller's `Paragraph::wrap`.
- Keys on an open PR: `Tab`/`Shift+Tab` cycle the tabs, `Esc`/`Backspace` go back to the list, `o` opens `pr.url()`, `r` refetches. Overview: `j/k` scroll the text; `J/K` move in the checks list.
- The open PR takes the whole body, no sidebar. The title line shows `{label}  {title}`, then a tab strip `Overview  Files {n}  Conversation {n}`.
- Overview lines: `kv("author")`, `kv("branch", "{head} → {base}")` (head prefixed with its repo when it's a fork), `kv("labels")`, `kv("merge", …)` (status in words plus `blocked_reason`, and `queued #n` when queued), then reviewers with verdict glyphs, then the description, then a `Checks` list.
- Refresh: in `main.rs`'s `sched` tick, when not throttled and a PR is open, push `FetchDetail` every `focused_iv` while `open_checks_pending()`, else every `broad_iv`.

- [ ] **Step 1: Write the failing tests**
  - `src/pulls/api.rs`, `detail_parses_reviewers_checks_threads_and_queue`. The fixture is a trimmed real response; capture one with `gh api graphql` and cut it down. Assert:
    - `merge_status == Blocked`;
    - `blocked_reason == Some("changes requested")`;
    - one `ActionsRun { run_id: 555 }` check;
    - one thread with `line == None && outdated`;
    - `merge_methods == [Squash, Merge]` when squash is the default and rebase is off;
    - `queue == Queued { position: 2 }`.
  - `src/ui/prs/overview.rs`, `markdown_hides_template_comments_and_carriage_returns`: input `"## Summary\r\n<!-- describe\r\nthe change -->\r\nFixes [#3](https://x/3)\r\n```\r\nlet a = 1;\r\n```"`. The output texts are `["Summary", "Fixes #3 (https://x/3)", "let a = 1;"]`, with no `\r` in any span and the first line bold.
  - `src/ui.rs`, `enter_opens_the_pr_overview`: open the PRs list with one PR, press `Enter`, assert `Command::Pr(PrCommand::FetchDetail(_))` is pending and the screen shows `"loading"`. Apply a `Detail` message and assert the screen contains `"main → release"`, `"changes requested"` and the check name. `Esc` returns to the list.
- [ ] **Step 2: Run them to see them fail**: `cargo test --quiet` gives compile errors.
- [ ] **Step 3: Implement** the interfaces above.
- [ ] **Step 4: Run all tests and clippy.** Expected: pass, clean.
- [ ] **Step 5: Ask the user, then commit.** Subject: `Open a PR to see its description, reviewers, merge state and checks`

---

### Task 4: From a check to its run

**Files:**
- Modify: `src/github.rs` (`pub async fn workflow_run(&self, repo: &str, run_id: u64) -> Result<Run>`, GET `/repos/{repo}/actions/runs/{run_id}`), `src/app/prs/protocol.rs`, `src/pulls/worker.rs`, `src/app/prs.rs`, `src/app/prs/input.rs`, `src/app.rs` (view filter and one `Esc` branch in `key_normal` in `input.rs`)

**Interfaces:**
- Produces:
  - `pub struct RunJump { pub run_id: u64, pub stray: bool }` and `PrsState.jump: Option<RunJump>`.
  - `PrCommand::FetchRun { repo: String, run_id: u64 }` and `PrMsg::Run(Result<Run, String>)`.
  - In `recompute_view_keeping`, a run whose id equals `prs.jump.run_id` passes every filter (scope, status filter, search).
- Behavior:
  - `Enter` on a check with `ActionsRun` sets `jump`, switches to `Mode::Normal`, and selects that run. If the run isn't in `app.runs`, it pushes `FetchRun`. On `PrMsg::Run(Ok(run))` it inserts the run into `app.runs` with `stray: true` and selects it.
  - `Link(url)` opens the browser. `Nothing` sets the status `"That check has no page to open"`.
  - `Esc` in `Mode::Normal` with `jump` set: removes the stray run, clears `jump`, recomputes the view and returns to `Mode::Prs` with the PR still open.
- [ ] **Step 1: Write the failing tests** in `src/app/prs.rs` tests:
  - `a_check_run_outside_the_list_is_fetched_and_shown`: with the status filter on Failed and the run not loaded, `Enter` on the check. Assert `FetchRun` is pending. Apply `PrMsg::Run(Ok(run))` with a successful run and assert `selected_run().id == run.id` even though the filter is Failed.
  - `esc_from_a_jumped_run_returns_to_the_pr_and_drops_the_stray_run`: after the jump, `Esc`. Assert `mode == Mode::Prs`, `prs.open.is_some()`, and `app.runs` no longer has the run.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run all tests and clippy.**
- [ ] **Step 5: Ask the user, then commit.** Subject: `Jump from a PR's check to its run, and back with Esc`

---

### Task 5: Files and diff

**Files:**
- Create: `src/app/prs/diff.rs`, `src/ui/prs/diff.rs`
- Modify: `src/pulls/model.rs`, `src/pulls/api.rs`, `src/pulls/worker.rs`, `src/app/prs.rs`, `src/app/prs/protocol.rs`, `src/app/prs/input.rs`, `src/ui/prs.rs`

**Interfaces:**
- Produces (model): `pub struct PrFile { pub path: String, pub previous_path: Option<String>, pub status: FileStatus, pub additions: u32, pub deletions: u32, pub patch: Option<String> }` and `pub enum FileStatus { Added, Removed, Modified, Renamed, Copied, Changed, Unchanged }`.
- Produces (api): `impl Github { pub async fn pr_files(&self, pr: &PrRef) -> Result<Vec<PrFile>> }`. GET `/repos/{repo}/pulls/{n}/files`, `per_page=100`, following pages until a short page or page 30. Page 1 is conditional, keyed `prfiles:{repo}#{n}`, with a cache like `repo_runners_cache`.
- Produces (diff):
  - `pub enum DiffLine { Hunk(String), Context { old: u32, new: u32, text: String }, Added { new: u32, text: String }, Removed { old: u32, text: String }, NoNewline }`.
  - `pub fn parse_patch(patch: &str) -> Vec<DiffLine>`: numbers come from each `@@ -a,b +c,d @@` header, and the `\ No newline at end of file` line becomes `NoNewline`.
  - `impl DiffLine { pub fn anchor(&self) -> Option<(Side, u32)> }`: Removed is `(Left, old)`, Added and Context are `(Right, new)`, others `None`.
  - `pub fn thread_rows(lines: &[DiffLine], threads: &[ReviewThread], path: &str) -> HashMap<usize, Vec<usize>>`: diff row index → thread indices. A thread with `line: None` (outdated) is left out.
  - `pub struct DiffView { pub file: usize, pub lines: Vec<DiffLine>, pub cursor: usize, pub scroll: usize, pub mark: Option<usize>, pub search: String, pub searching: bool, pub matches: Vec<usize> }` with `fn move_cursor(&mut self, delta: i32)` (stops on any row), `fn set_search(&mut self, q: &str)`, `fn next_match(&mut self, forward: bool)`.
  - `pub struct FilesState { pub files: Loadable<Vec<PrFile>>, pub sel: ListState, pub diff: Option<DiffView> }` on `PrView`.
- Commands: `PrCommand::FetchFiles(PrRef)` and `PrMsg::Files { pr, result: Result<Vec<PrFile>, String> }`. Fetched the first time the Files tab opens, and on `r`.
- Layout: file list on the left (width 40, or a third of the body if narrower). Each row is `{status letter} {path}  +a −d`. The diff on the right has old and new line-number gutters, green Added, red Removed, dim hunk headers, `💬n` in the gutter on rows with threads, the cursor row in `select_style(true)`, and marked range rows in `bg_sel_dim()`.
- Drawing: draw only the visible window of `lines`. Expand `\t` to 4 spaces and clip each line to the pane width.
- File without patch: `"GitHub doesn't send this diff (binary or too large). o opens it on github.com."` and `o` opens `{pr.url()}/files`.
- Keys: in the file list, `j/k` move and `Enter`/`l` open the diff. In the diff, `j/k/PgUp/PgDn/g/G` move, `/` searches, `n/N` step through matches, and `Esc`/`h` return to the file list. Range marking and comments come in Task 9.
- [ ] **Step 1: Write the failing tests**
  - `src/app/prs/diff.rs`, `parse_patch_numbers_both_sides`: for `"@@ -10,3 +10,4 @@ fn a()\n ctx\n-old\n+new\n+added\n ctx2\n\\ No newline at end of file"`, assert the variants and numbers in order: `Hunk`, `Context{10,10}`, `Removed{11}`, `Added{11}`, `Added{12}`, `Context{12,13}`, `NoNewline`.
  - `outdated_threads_have_no_row`: a thread with `line: None` is absent from `thread_rows`, and one at `(Right, 12)` maps to the `Added{12}` row.
  - `a_huge_diff_parses_and_jumps_quickly`: a 30 000-line patch parses, and `G` puts the cursor on the last row. The screen test (Step 1, `src/ui.rs`) asserts the last line's text is on screen.
  - `src/ui.rs`, `the_diff_draws_tabs_as_spaces_and_clips_long_lines`: a file whose patch has `"+\tlet x = 1;"` and a 500-char line. Assert the screen contains `"    let x = 1;"` and no row is wider than the terminal.
  - `src/ui.rs`, `a_file_without_a_patch_says_so`: a `PrFile` with `patch: None` shows the message above.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run all tests and clippy.**
- [ ] **Step 5: Ask the user, then commit.** Subject: `Show a PR's changed files and their diffs`

---

### Task 6: Conversation

**Files:**
- Create: `src/ui/prs/conversation.rs`
- Modify: `src/app/prs.rs`, `src/app/prs/input.rs`, `src/ui/prs.rs`

**Interfaces:**
- Produces: `pub enum ConvEntry<'a> { Description, Comment(&'a Comment), Review(&'a Review), Thread(usize) }` and `fn conversation(d: &PrDetail) -> Vec<(DateTime<Utc>, ConvEntry<'_>)>`, sorted by time. A thread sorts by its first comment's time, and the description comes first. A review with an empty body and verdict `Commented` is left out, since its comments appear in their threads.
- Drawing: each entry gets a header line `{author}  {verdict or "commented"}  {age}`, then its body through `markdown_lines`. A thread shows `{path}:{line}` plus `outdated`/`resolved` tags, then its comments indented. The tab scrolls with `j/k/PgUp/PgDn/g/G`.
- [ ] **Step 1: Write the failing tests**
  - `src/app/prs.rs`, `conversation_is_in_time_order_with_threads_at_their_first_comment`: a comment at t2, a review at t1, and a thread whose first comment is at t3 give `[Description, Review, Comment, Thread(0)]`.
  - `src/ui.rs`, `an_outdated_thread_still_shows_in_the_conversation`: the screen contains the thread's path and `outdated`.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run all tests and clippy.**
- [ ] **Step 5: Ask the user, then commit.** Subject: `Show a PR's conversation: comments, reviews and threads`

---

### Task 7: Comment box and simple PR actions

**Files:**
- Create: `src/app/prs/compose.rs`, `src/ui/prs/overlays.rs`
- Modify: `Cargo.toml` (`tui-textarea = "0.7"`), `src/app/overlays.rs` (`PendingAction` variants), `src/app/input.rs` (confirm refactor), `src/app.rs` (field `confirm_from: Mode`), `src/pulls/model.rs`, `src/pulls/api.rs`, `src/pulls/worker.rs`, `src/app/prs/protocol.rs`, `src/app/prs.rs`, `src/app/prs/input.rs`

**Interfaces:**
- Confirm refactor: `fn ask(&mut self, action: PendingAction)` records `confirm_from = self.mode` and sets `Mode::Confirm`. All existing places that set `pending_action` go through it. `key_confirm` returns to `confirm_from` instead of calling `overlay_return_mode()`.
- `PendingAction` gains `Pr { pr: PrRef, action: PrAction, label: String }` and `DiscardCompose`. `prompt()` returns the label for `Pr` and `"Discard what you typed?"` for `DiscardCompose`.
- Protocol:
  - `PrCommand::Act { pr: PrRef, action: PrAction }`.
  - `pub enum PrAction { Comment { body: String }, SetDraft { node_id: String, draft: bool }, UpdateBranch { head_oid: String }, RerunFailed { run_ids: Vec<u64> }, … }`. Tasks 8 and 9 add more.
  - `pub enum PrActionKind`, one per variant with no data, plus `fn kind(&self) -> PrActionKind` and `fn done_text(k) -> &'static str` (`"Comment posted"`, `"Marked ready for review"` / `"Converted to draft"`, `"Branch updated"`, `"Re-running failed checks"`).
  - `PrMsg::ActDone { pr: PrRef, kind: PrActionKind, result: Result<(), String> }`. On any `ActDone`, the open PR (if it is `pr`) gets a `FetchDetail`, plus a `FetchFiles` if files were loaded.
- API:
  - `comment_on_pr(&self, pr: &PrRef, body: &str)`: REST POST `/repos/{repo}/issues/{n}/comments`.
  - `set_draft(&self, node_id: &str, draft: bool)`: GraphQL `markPullRequestReadyForReview` / `convertPullRequestToDraft`.
  - `update_branch(&self, pr: &PrRef, head_oid: &str)`: REST PUT `/repos/{repo}/pulls/{n}/update-branch` with `expected_head_sha`.
  - `RerunFailed` calls the existing `Github::rerun_failed(repo, run_id)` for each run id.
- Compose:
  - `pub enum ComposeTarget { PrComment, … }`. Task 9 adds line comments, replies, draft edits and the review body.
  - `pub struct Compose { pub target: ComposeTarget, pub editor: tui_textarea::TextArea<'static> }` with `fn text(&self) -> String`.
  - `PrView.compose: Option<Compose>` and `PrView.inflight: Option<(ComposeTarget, String)>`. The compose box takes every key while open.
- Keys:
  - `c` (Overview/Conversation): compose a PR comment.
  - In the box: `Ctrl+S` sends, which moves the text to `inflight`, pushes `Act` and closes the box. `Esc` closes the box when it's empty, otherwise asks `DiscardCompose`.
  - On `ActDone` with an error for an action that came from the box, the box reopens with the `inflight` text and the error goes to the status line. On success, `inflight` is cleared.
  - `D` asks "Mark ready for review?" or "Convert to draft?". `u` (only when `merge_status == Behind`) asks "Update branch from {base}?". `x` (only when some `ActionsRun` check `Failed`) asks "Re-run failed checks?" and collects the distinct run ids.
- The box is a popup at 70% × 50% titled for its target, with the hint `Ctrl+S send · Esc cancel`.
- [ ] **Step 1: Write the failing tests** in `src/app/prs.rs` tests:
  - `a_failed_comment_reopens_the_box_with_its_text`: type "hello", `Ctrl+S`, then apply `ActDone{kind: Comment, result: Err("boom")}`. Assert the compose box is open with text `"hello"` and the status shows `"boom"`.
  - `escape_with_text_asks_before_discarding`: type "x", `Esc`. Assert `Mode::Confirm` with `DiscardCompose`; `n` returns to `Mode::Prs` with the box and text intact.
  - `confirm_returns_to_the_screen_it_came_from`: the existing cancel-run confirm from `Mode::Normal` still returns to `Normal` (regression), and a PR confirm returns to `Mode::Prs`.
  - `update_branch_is_only_offered_when_behind`: with `merge_status: Clean`, `u` sets a status and pushes nothing.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run all tests and clippy.** Also check by hand in Windows Terminal that `Ctrl+S` reaches the box.
- [ ] **Step 5: Ask the user, then commit.** Subject: `Comment on a PR, toggle draft, update its branch and re-run failed checks`

---

### Task 8: Merge and merge queue

**Files:**
- Modify: `src/pulls/api.rs`, `src/pulls/worker.rs`, `src/app/prs/protocol.rs`, `src/app/prs.rs`, `src/app/prs/input.rs`, `src/ui/prs/overlays.rs`

**Interfaces:**
- `PrAction` gains `Merge { method: MergeMethod, head_oid: String, delete_branch: Option<String> }`, `Enqueue { node_id: String, head_oid: String }` and `Dequeue { node_id: String }`.
- API:
  - `merge_pr(&self, pr, method, head_oid)`: REST PUT `/repos/{repo}/pulls/{n}/merge` with `merge_method` and `sha`.
  - `delete_branch(&self, repo, branch)`: REST DELETE `/repos/{repo}/git/refs/heads/{branch}`. Called only after a successful merge. A failure here becomes `"Merged, but deleting the branch failed: …"`.
  - `enqueue_pr`: GraphQL `enqueuePullRequest(input:{pullRequestId, expectedHeadOid})`. `dequeue_pr`: `dequeuePullRequest(input:{id})`.
- `pub struct MergeDialog { pub methods: Vec<MergeMethod>, pub sel: usize, pub delete_branch: Option<bool> }` on `PrView`. `delete_branch` is `None` when `head.repo != pr.repo` (fork), else `Some(false)`.
- Keys:
  - `m` with `QueueState::NoQueue` opens the dialog: the methods list, `d` toggles delete branch, `Enter` merges, `Esc` closes. When the PR is blocked, the dialog shows `"Blocked: {reason}. Merging works only if you can bypass branch protection."`
  - `m` with `NotQueued` asks "Add {label} to the merge queue?". With `Queued` it asks "Remove {label} from the merge queue?".
  - `m` on a draft or with `Conflicts` sets a status and does nothing else.
- [ ] **Step 1: Write the failing tests** in `src/app/prs.rs` tests:
  - `a_fork_pr_offers_no_branch_deletion`: detail with `head.repo = "fork/api"` for `org/api#5`; `m` gives `merge_dialog.delete_branch == None`, and the dialog screen has no "delete branch".
  - `merge_offers_only_allowed_methods_default_first`: `merge_methods = [Squash, Rebase]` gives a dialog listing exactly those, Squash selected.
  - `a_queue_repo_queues_instead_of_merging`: `queue: NotQueued`, `m`, `y` pushes `Act{Enqueue{..}}`.
  - `a_refused_merge_shows_why_and_refreshes`: `ActDone{kind: Merge, result: Err("Head branch was modified")}` puts the error in the status and pushes `FetchDetail`.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run all tests and clippy.**
- [ ] **Step 5: Ask the user, then commit.** Subject: `Merge a PR or add it to the merge queue`

---

### Task 9: Line comments, review threads and submitting a review

**Files:**
- Modify: `src/pulls/model.rs`, `src/pulls/api.rs`, `src/pulls/worker.rs`, `src/app/prs/protocol.rs`, `src/app/prs.rs`, `src/app/prs/compose.rs`, `src/app/prs/diff.rs`, `src/app/prs/input.rs`, `src/ui/prs/diff.rs`, `src/ui/prs/conversation.rs`, `src/ui/prs/overlays.rs`, `src/ui/prs.rs`

**Interfaces:**
- Model:
  - `pub enum LineSpan { One(u32), Range { start: u32, end: u32 } }` with `fn new(a: u32, b: u32) -> LineSpan` (equal gives One, otherwise sorted Range).
  - `pub struct DraftComment { pub path: String, pub side: Side, pub span: LineSpan, pub body: String }`.
  - `pub struct DraftReply { pub thread_id: String, pub body: String }`.
  - `pub enum ReviewEvent { Approve, RequestChanges, Comment }`.
  - `pub struct ReviewSubmission { pub head_oid: String, pub event: ReviewEvent, pub body: String, pub comments: Vec<DraftComment>, pub replies: Vec<DraftReply> }`.
- App:
  - `#[derive(Default)] pub struct PendingReview { pub comments: Vec<DraftComment>, pub replies: Vec<DraftReply> }` with `fn is_empty(&self) -> bool`.
  - `PrsState.drafts: HashMap<PrRef, PendingReview>`, kept across closing the PR and leaving the screen.
  - `impl PrsState { pub fn has_unsent(&self) -> bool }` is true if any draft is non-empty or any `compose`/`inflight` has text.
- `PrAction` gains:
  - `LineComment { head_oid: String, comment: DraftComment }`: REST POST `/repos/{repo}/pulls/{n}/comments` with `commit_id`, `path`, `side`, `line`, and `start_line`/`start_side` for a range.
  - `Reply { comment_id: u64, body: String }`: REST POST `/repos/{repo}/pulls/{n}/comments/{comment_id}/replies`, where `comment_id` is the thread's first comment.
  - `SubmitReview { node_id: String, submission: ReviewSubmission }`: GraphQL, in order:
    1. `addPullRequestReview(input:{pullRequestId, commitOID, threads:[{path, line, side, startLine, startSide, body}]})` with no `event`, which leaves the review pending, and read its `id`;
    2. for each reply, `addPullRequestReviewThreadReply(input:{pullRequestReviewId, pullRequestReviewThreadId, body})`;
    3. `submitPullRequestReview(input:{pullRequestReviewId, event, body})`.
    If step 2 or 3 fails, call `deletePullRequestReview(input:{pullRequestReviewId})` and return the original error.
  - `ResolveThread { thread_id: String, resolved: bool }`: GraphQL `resolveReviewThread` / `unresolveReviewThread`.
- Compose targets: `LineComment { path, side, span }`, `Reply { thread_id, comment_id }`, `EditDraft { index: usize }` (index into the PR's `comments`) and `ReviewBody { event: ReviewEvent }`.
- Rules:
  - A line comment's `Ctrl+S` adds it to the PR's pending review, starting one if needed. Its `Ctrl+G` posts it on its own as a `LineComment`, and works only when the PR has no pending review; otherwise it sets the status `"You have a review in progress; Ctrl+S adds to it"`.
  - A reply's `Ctrl+S` joins the pending review when one exists, else posts at once as `Reply`.
  - `SubmitReview` success removes the PR's draft. Failure keeps it and shows the error.
- Keys:
  - In the diff, `v` marks the cursor row as a range start, or clears the mark. Rows without `anchor()` can't be marked. A range must stay on one side; otherwise the status says `"A range has to stay on one side of the diff"`.
  - `c` composes a line comment for the cursor row, or the marked range up to the cursor row.
  - `Enter` on a row with threads opens the thread popup, listing the comments. In it, `r` replies, `x` resolves or unresolves, `Esc` closes.
  - `p` (any tab) opens the pending-review panel: `j/k`, `e` edits the comment, `d` drops it, `R` submits.
  - `R` (any tab, or in the panel) opens the verdict picker: Approve, Request changes, Comment. `Enter` opens the `ReviewBody` box, and its `Ctrl+S` sends `SubmitReview` with every draft comment and reply. Approve with no drafts and an empty body is allowed. Comment with nothing at all sets the status `"Nothing to send"`.
- Drawing: rows with draft comments get a `✎` in the gutter, and the tab strip shows `review: n pending` when the PR has a draft.
- [ ] **Step 1: Write the failing tests** in `src/app/prs.rs` tests:
  - `line_comments_collect_into_the_pending_review`: two `c`+text+`Ctrl+S` on rows `Added{12}` and `Removed{11}` give `drafts[pr].comments` with `(Right, One(12))` and `(Left, One(11))`, and no command is pending.
  - `a_range_spans_from_the_mark_to_the_cursor`: `v` on `Added{11}`, move to `Added{12}`, `c` gives target `LineComment{ side: Right, span: Range{11,12} }`.
  - `a_range_across_sides_is_refused`.
  - `a_reply_posts_at_once_without_a_review_and_joins_one_with_it`.
  - `a_single_comment_is_refused_while_a_review_is_in_progress`.
  - `a_failed_review_keeps_every_draft`: submit, then `ActDone{kind: SubmitReview, result: Err("Pull request head changed")}`. The drafts are unchanged, the status shows the error and `FetchDetail` is pending.
  - `drafts_survive_closing_the_pr`: draft, `Esc` to the list, `Esc` to the runs, `P`, `Enter` on the same PR. The draft is still there.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run all tests and clippy.**
- [ ] **Step 5: Ask the user, then commit.** Subject: `Review a PR with line comments, replies and a verdict`

---

### Task 10: Unsent-review guard, list marker, help and README

**Files:**
- Modify: `src/app/input.rs` (`q` and `Ctrl+C`), `src/app/overlays.rs` (`PendingAction::Quit`), `src/ui/prs.rs` (list marker), `src/ui/overlays.rs` (`draw_help` entries), `src/ui.rs` (footer hints), `README.md`

**Interfaces:**
- `q` in `Mode::Normal` and `Ctrl+C` anywhere: when `prs.has_unsent()`, `ask(PendingAction::Quit)` with the prompt `"Quit and lose {n} unsent review comment(s)?"`. Otherwise quit as today. In that confirm, `y` or a second `Ctrl+C` quits.
- List rows whose PR has a non-empty draft show `✎` after the number.
- Help: a "Pull requests" section listing every key from Tasks 2–9. It must still fit a standard terminal, as `the_help_fits_a_standard_terminal` checks.
- README: a Features bullet `- Your open PRs and review requests (P): description, checks, diffs, conversation; comment, review with line comments, merge or queue, toggle draft.` and a `P` row in the Keys table.
- [ ] **Step 1: Write the failing tests**
  - `src/app/prs.rs`, `quitting_with_unsent_comments_asks_first`: a draft plus `q` gives `Mode::Confirm`, and `should_quit` stays false. Then `Ctrl+C` sets `should_quit`.
  - `quitting_without_drafts_is_immediate`.
  - `src/ui.rs`, `a_pr_with_a_draft_is_marked_in_the_list`.
  - `the_help_fits_a_standard_terminal` still passes.
- [ ] **Step 2: Run them to see them fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run all tests and clippy, then run actui against the real account.** Check that:
  - the list loads;
  - a PR opens and its diff scrolls;
  - a comment posts on a test PR;
  - `Ctrl+S` works in Windows Terminal.
- [ ] **Step 5: Ask the user, then commit.** Subject: `Ask before quitting with an unsent review, and document the PR keys`
