# Pull requests in actui

## Why

The user wants to handle their pull requests in actui instead of switching to
github.com: the ones they opened, and the ones waiting on their review. Not only
to look at them but to work on them: read the description and the diff, review
with line comments the way GitHub reviews work, comment, and merge.

## Goals

- A PRs screen listing open PRs that the user opened or whose review is
  requested from them, across every repo the token can see.
- A PR's details: description, branches, reviewers and their verdicts, labels,
  merge state, checks, changed files with their diffs, and the conversation.
- Acting on a PR from actui:
  - review: approve, request changes, or comment, with a message;
  - line comments in the diff, single lines and ranges, collected into a
    pending review and sent with the verdict, as on GitHub;
  - reply to an existing review thread, and resolve or unresolve it;
  - comment on the conversation;
  - merge with any method the repo allows, optionally deleting the branch;
  - mark ready for review, or convert to draft;
  - update the branch from its base when it is behind;
  - re-run failed checks (actui can already re-run Actions jobs).
- Moving from a PR's check to that run in actui's runs screen.

## Non-goals (first version)

Listed so the user can pull any of them back in:

- Creating PRs, or editing a PR's title, body, labels, reviewers or assignees.
- Enabling auto-merge.
- Syntax highlighting in diffs.
- Full Markdown rendering (see "Text" below).
- Notifications for new review requests.
- Closed or merged PRs, and PRs the user is only mentioned in or assigned to.

## Anti-goals

- Burning the rate limit. actui is careful about quota today (conditional
  requests, slow polling when idle); the PRs screen must not change that.
- Losing a review the user has typed. Unsent line comments or review text must
  never vanish without a warning.
- A slower main screen. Nothing about PRs may delay the runs list.
- Growing the already large files. `ui.rs` (2800 lines) and `app.rs` (2000)
  must not absorb this; PR code goes in its own modules.

## Constraints

- Rust, ratatui 0.29, crossterm 0.28, tokio, reqwest; follow the existing
  structure: `App` state and `Command`/`DataMsg` messages in `src/app/`,
  drawing in `src/ui/`, API calls in `src/github.rs`, workers spawned from
  `main.rs`'s command dispatch.
- The same token as today (`repo` and `workflow` scopes). No new auth.
- New dependencies need the user's approval first.
- Builds on the runner and theme changes made alongside this design, which
  land first.
- Must work in Windows Terminal, macOS and Linux terminals. Key choices must
  not rely on combinations Windows Terminal can't send.
- Matches existing UI conventions: bordered panes, focus moves with Tab and
  arrows, mouse select/scroll/drag-to-resize, a footer of key hints, entries in
  the `?` help, the theme accessors in `ui.rs`.
- Tests in the existing style: `TestBackend` screen tests and unit tests on
  the state types.
- README features and keys updated.

## What was decided

These are the user's decisions unless marked *(my call)*; the user can
correct any of the calls.

**The list**

- Opened with `P` *(my call; `p` already toggles the sidebar)*, and from a
  `⇄ PRs  P` switcher in the tabs row beside Runners and Duration
  *(my call)*.
- Two groups: **Review requested** first, then **Opened by you**
  *(order is my call)*. A PR in both shows once, under Review requested.
- "Review requested" means what github.com's own review-requests tab shows,
  which includes requests to a team the user is on *(my call)*.
- The list is global: not limited to watched repos or to the repo the sidebar
  is scoped to, since review requests come from anywhere *(my call)*.
- Each row: repo, number, title, author, age (last update), checks state,
  draft marker, review state (approved, changes requested, review required).
- The switcher shows how many PRs wait on the user's review *(my call)*.
- `o` opens the selected PR on github.com.

**The PR screen** (Enter on a row)

- Overview: description, head and base branches, author, reviewers and what
  each decided, labels, mergeable and merge state (blocked, behind, conflicts,
  clean), and the checks on the head commit.
- Enter on a check that is an Actions run opens that run's jobs in actui's
  runs screen; other checks open their page in the browser. The run is
  fetched by id and shown even when it is not in the runs list (repo not
  watched, filtered out, or too old), without changing filters, scope or the
  watched repos. Esc from there returns to the PR *(my call)*.
- Files: changed files with +/- counts. Picking one shows its unified diff:
  hunk headers, old and new line numbers, added and removed lines colored,
  scrollable, with the same search the logs viewer has.
- Existing review threads show inline in the diff at their lines, and can be
  opened to read, reply, resolve and unresolve.
- Conversation: the description, comments, reviews and review comments in
  time order.

**Reviewing**

- In the diff, the user puts the cursor on a line, or marks a range, and
  writes a comment. Comments collect in a pending review that lists them and
  lets the user edit or drop each one.
- Submitting asks for the verdict (approve, request changes, comment) and an
  optional message, and sends the review with all its comments at once.
- The pending review lives in actui's memory, not on GitHub, until sent
  *(my call)*. Each PR has its own, kept while the user moves around actui
  (other PRs, the runs screen, a check's run). PRs with an unsent review are
  marked in the list. Quitting with any unsent review asks first
  *(my call)*.
- Replies to a thread and line comments follow GitHub: while the PR has a
  pending review they join it; otherwise they post at once. A line comment can
  also be sent on its own instead of starting a review *(my call)*.

**Acting**

- Merge asks for confirmation (like cancel does today), offers only the
  methods the repo allows, and has a "delete branch" option.
- In a repo with a merge queue, merge adds the PR to the queue (GraphQL
  `enqueuePullRequest`), and the PR shows its queue position or state;
  removing it from the queue is also offered.
- When GitHub reports the PR as blocked, merge stays available with the reason
  shown in the confirmation, so an admin who can bypass branch protection can
  still merge; for everyone else GitHub's refusal appears in the status line
  *(my call)*.
- Ready for review / convert to draft, update branch, and re-run failed
  checks are one key each, with confirmation where the action is hard to undo.
- Every action reports success or the API's error in the status line, and the
  PR refreshes afterwards.

**Refreshing** *(my calls)*

- The list refreshes with the broad sweep (`refresh_secs`) and when the screen
  opens.
- An open PR refreshes on the faster `active_refresh_secs` cadence while its
  checks run, otherwise with the broad sweep, and after every action.
- Diffs are read when a file is opened, not ahead of time.

**API** *(my call)*

- The list comes from GraphQL search: one query per group returns the rows
  with checks state, review decision and draft flag, which REST search does
  not, at a cost of about one GraphQL point each. REST search would need extra
  calls per PR and has a 30-a-minute limit.
- Ready for review, convert to draft, and resolving threads only exist in
  GraphQL, so they use it.
- Everything else may use REST or GraphQL, the builder's choice, preferring
  conditional REST reads where the data is polled.

**Text**

- Descriptions and comments are shown as plain text with light styling
  (headings bold, code blocks set apart, links visible), without a Markdown
  library *(my call; adding one needs the user's approval)*.
- Comments and review messages are typed in a multi-line box inside actui,
  built on the `tui-textarea` crate (approved by the user; use a version
  that supports ratatui 0.29).

## Left to the builder

- Layout of the PRs screen and the PR screen within the conventions above,
  including whether files and diff sit side by side or stack.
- Key bindings inside the PRs and PR screens, and the send key for the
  multi-line box (it must work in Windows Terminal).
- How range selection works in the diff.
- Which REST or GraphQL calls back each part of the overview, files and
  conversation.
- How very large diffs are handled (GitHub omits the patch for some files;
  show that plainly and offer the browser).
- Build order. A suggested one, each step shippable: list and overview;
  files and diff; conversation; actions (comment, review verdict, merge,
  draft, update branch, re-run); line comments, thread replies and resolving.
