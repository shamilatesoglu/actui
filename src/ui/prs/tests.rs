//! Screen tests for the PR screens, drawn through the `screen()` helper of
//! the main UI tests.

use crate::ui::tests::{column_of, demo_app, mouse, row_of, screen};
use crate::ui::*;
use crossterm::event::{MouseButton, MouseEventKind};

fn pr(repo: &str, number: u64, title: &str) -> crate::pulls::model::PrSummary {
    use crate::pulls::model::*;
    PrSummary {
        pr: PrRef { repo: repo.into(), number },
        title: title.into(),
        author: "ana".into(),
        updated_at: Utc::now(),
        draft: false,
        checks: ChecksState::Failing,
        review: ReviewDecision::ChangesRequested,
    }
}

fn prs_app() -> App {
    use crate::app::{DataMsg, PrMsg};
    use crate::pulls::model::PrGroup;
    use crossterm::event::{KeyCode, KeyEvent};
    let mut app = demo_app();
    app.handle_key(KeyEvent::from(KeyCode::Char('P')));
    app.apply(DataMsg::Pr(PrMsg::List {
        group: PrGroup::ReviewRequested,
        result: Ok(vec![pr("org/api", 7, "Fix login")]),
    }));
    app.apply(DataMsg::Pr(PrMsg::List { group: PrGroup::Authored, result: Ok(vec![]) }));
    app
}

#[test]
fn p_opens_the_prs_screen_with_both_groups() {
    let mut app = prs_app();
    let out = screen(&mut app, 140, 24);
    println!("{out}");
    assert!(out.contains("Review requested"));
    assert!(out.contains("api#7"));
    assert!(out.contains("Fix login"));
    assert!(out.contains("changes requested"));
    assert!(out.contains("Opened by you  none"));
    assert!(out.contains("⇄ PRs 1  P"), "the tab counts what waits on you");
}

#[test]
fn a_pr_with_a_draft_is_marked_in_the_list() {
    use crate::app::PendingReview;
    use crate::pulls::model::{DraftComment, LineSpan, PrRef, Side};
    let mut app = prs_app();
    assert!(!screen(&mut app, 140, 24).contains('✎'));
    let comment = DraftComment { path: "a.rs".into(), side: Side::Right, span: LineSpan::One(1), body: "why?".into() };
    let review = PendingReview { head_oid: "abc".into(), comments: vec![comment], replies: vec![] };
    app.prs.drafts.insert(PrRef { repo: "org/api".into(), number: 7 }, review);
    let out = screen(&mut app, 140, 24);
    assert!(out.contains("api#7✎"), "{out}");
}

#[test]
fn the_tabs_row_switches_to_the_prs_screen() {
    let mut app = demo_app();
    let out = screen(&mut app, 140, 24);
    assert!(out.contains("⇄ PRs  P"), "no count while nothing waits");
    let row = row_of(&out, "⇄ PRs");
    let at = column_of(out.lines().nth(row as usize).unwrap(), "⇄ PRs") as u16;
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), at, row));
    assert!(matches!(app.mode, crate::app::Mode::Prs));
    assert!(matches!(app.pending.last(), Some(crate::app::Command::Pr(crate::app::PrCommand::FetchList))));

    // A filter tab goes back to the runs.
    let out = screen(&mut app, 140, 24);
    let row = row_of(&out, "Running");
    let at = column_of(out.lines().nth(row as usize).unwrap(), "Running") as u16;
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), at, row));
    assert!(matches!(app.mode, crate::app::Mode::Normal));
}

#[test]
fn the_prs_keys_open_refresh_and_leave() {
    use crate::app::{Command, Mode};
    use crossterm::event::{KeyCode, KeyEvent};
    let mut app = prs_app();
    screen(&mut app, 140, 24);
    app.pending.clear();
    app.handle_key(KeyEvent::from(KeyCode::Char('o')));
    assert!(matches!(&app.pending[..], [Command::OpenUrl(u)] if u == "https://github.com/org/api/pull/7"));
    app.pending.clear();
    app.handle_key(KeyEvent::from(KeyCode::Char('r')));
    assert!(matches!(&app.pending[..], [Command::Pr(_)]));
    app.handle_key(KeyEvent::from(KeyCode::Char('4')));
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.filter, crate::app::Filter::Failed);
    app.handle_key(KeyEvent::from(KeyCode::Char('P')));
    app.handle_key(KeyEvent::from(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Normal));
}

fn pr_detail() -> crate::pulls::model::PrDetail {
    use crate::pulls::model::*;
    PrDetail {
        node_id: "PR_7".into(),
        title: "Fix login".into(),
        body: "Stops the loop.\n<!-- template -->".into(),
        author: "ana".into(),
        head: HeadRef { branch: "main".into(), repo: "org/api".into(), oid: "abc".into() },
        base_branch: "release".into(),
        draft: false,
        labels: vec!["bug".into()],
        merge_status: MergeStatus::Blocked,
        reviewers: vec![Reviewer { login: "bob".into(), verdict: Verdict::ChangesRequested }],
        checks: vec![Check {
            name: "Unit tests".into(),
            state: CheckState::Pending,
            target: CheckTarget::ActionsRun { run_id: 555 },
        }],
        comments: vec![],
        reviews: vec![],
        threads: vec![],
        merge_methods: vec![MergeMethod::Squash],
        queue: QueueState::Queued { position: 2 },
    }
}

#[test]
fn enter_opens_the_pr_overview() {
    use crate::app::{Command, DataMsg, Mode, PrCommand, PrMsg};
    use crossterm::event::{KeyCode, KeyEvent};
    let mut app = prs_app();
    screen(&mut app, 140, 24);
    app.pending.clear();
    app.handle_key(KeyEvent::from(KeyCode::Enter));
    let pr = match &app.pending[..] {
        [Command::Pr(PrCommand::FetchDetail(pr))] => pr.clone(),
        _ => panic!("expected a detail fetch"),
    };
    assert_eq!(pr.label(), "org/api#7");
    let out = screen(&mut app, 140, 24);
    assert!(out.contains("loading"), "{out}");

    app.apply(DataMsg::Pr(PrMsg::Detail { pr, result: Ok(Box::new(pr_detail())) }));
    let out = screen(&mut app, 140, 24);
    println!("{out}");
    assert!(out.contains("org/api#7  Fix login"));
    assert!(out.contains("main → release"));
    assert!(out.contains("blocked: changes requested, queued #2"));
    assert!(out.contains("Stops the loop."));
    assert!(!out.contains("template"));
    assert!(out.contains("Unit tests"));
    assert!(out.contains("Conversation 0"), "the count of conversation entries");
    assert!(app.prs.open_checks_pending());

    // Another PR's answer, arriving late, changes nothing.
    let other = crate::pulls::model::PrRef { repo: "org/api".into(), number: 9 };
    app.apply(DataMsg::Pr(PrMsg::Detail { pr: other, result: Err("late".into()) }));
    assert!(screen(&mut app, 140, 24).contains("Unit tests"));

    app.handle_key(KeyEvent::from(KeyCode::Tab));
    app.handle_key(KeyEvent::from(KeyCode::Tab));
    assert!(screen(&mut app, 140, 24).contains("ana  description"));
    app.pending.clear();
    app.handle_key(KeyEvent::from(KeyCode::Char('o')));
    assert!(matches!(&app.pending[..], [Command::OpenUrl(u)] if u == "https://github.com/org/api/pull/7"));
    app.pending.clear();
    app.handle_key(KeyEvent::from(KeyCode::Char('r')));
    assert!(matches!(&app.pending[..], [Command::Pr(PrCommand::FetchDetail(_))]));

    app.handle_key(KeyEvent::from(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Prs));
    let out = screen(&mut app, 140, 24);
    assert!(out.contains("Review requested"), "back on the list");
}

/// Clicks the first column of `label` on the screen as drawn.
fn click_label(app: &mut App, label: &str) {
    let out = screen(app, 140, 30);
    let row = row_of(&out, label);
    let at = column_of(out.lines().nth(row as usize).unwrap(), label) as u16;
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), at, row));
}

#[test]
fn tab_clicks_leave_an_open_box_alone() {
    use crate::app::{DataMsg, Mode, PrMsg};
    use crate::pulls::model::QueueState;
    use crossterm::event::{KeyCode, KeyEvent};
    let mut app = prs_app();
    app.handle_key(KeyEvent::from(KeyCode::Enter));
    let pr = app.prs.open.as_ref().unwrap().pr.clone();
    let mut detail = pr_detail();
    detail.queue = QueueState::NoQueue;
    app.apply(DataMsg::Pr(PrMsg::Detail { pr, result: Ok(Box::new(detail)) }));
    app.handle_key(KeyEvent::from(KeyCode::Char('c')));
    for c in "half a thought".chars() {
        app.handle_key(KeyEvent::from(KeyCode::Char(c)));
    }
    for label in ["⇄ PRs", "⚙ Runners", "◷ Duration", "Running"] {
        click_label(&mut app, label);
        assert!(app.mode == Mode::Prs, "a click on {label} keeps the PR screen");
        let compose = app.prs.open.as_ref().and_then(|v| v.compose.as_ref());
        assert_eq!(compose.map(|c| c.text()).as_deref(), Some("half a thought"), "after {label}");
    }

    // The merge box and the review boxes are kept the same way.
    app.prs.open.as_mut().unwrap().compose = None;
    app.handle_key(KeyEvent::from(KeyCode::Char('m')));
    click_label(&mut app, "⚙ Runners");
    assert!(app.prs.open.as_ref().is_some_and(|v| v.merge_dialog.is_some()));
    app.handle_key(KeyEvent::from(KeyCode::Esc));
    app.handle_key(KeyEvent::from(KeyCode::Char('R')));
    click_label(&mut app, "Running");
    assert!(app.prs.open.as_ref().is_some_and(|v| v.review_popup.is_some()));

    // With nothing open, the tabs work as before.
    app.handle_key(KeyEvent::from(KeyCode::Esc));
    click_label(&mut app, "Running");
    assert!(app.mode == Mode::Normal);
}

#[test]
fn the_comment_box_and_a_discard_question_draw_over_the_pr() {
    use crate::app::{DataMsg, PrMsg};
    use crossterm::event::{KeyCode, KeyEvent};
    let mut app = prs_app();
    app.handle_key(KeyEvent::from(KeyCode::Enter));
    let pr = app.prs.open.as_ref().unwrap().pr.clone();
    app.apply(DataMsg::Pr(PrMsg::Detail { pr, result: Ok(Box::new(pr_detail())) }));
    app.handle_key(KeyEvent::from(KeyCode::Char('c')));
    for c in "looks good".chars() {
        app.handle_key(KeyEvent::from(KeyCode::Char(c)));
    }
    let out = screen(&mut app, 140, 30);
    assert!(out.contains("Comment on org/api#7"));
    assert!(out.contains("looks good"));
    assert!(out.contains("Ctrl+S send · Esc cancel"));
    assert!(out.contains("Unit tests"), "the PR stays visible behind the box");

    app.handle_key(KeyEvent::from(KeyCode::Esc));
    let out = screen(&mut app, 140, 30);
    assert!(out.contains("Discard what you typed?"));
    assert!(out.contains("Unit tests"), "the PR, not the runs, stays behind the question");
}

#[test]
fn the_footer_lists_update_and_rerun_keys_when_they_apply() {
    use crate::app::{DataMsg, PrMsg};
    use crate::pulls::model::{CheckState, MergeStatus};
    use crossterm::event::{KeyCode, KeyEvent};
    let mut app = prs_app();
    app.handle_key(KeyEvent::from(KeyCode::Enter));
    let pr = app.prs.open.as_ref().unwrap().pr.clone();
    let mut detail = pr_detail();
    app.apply(DataMsg::Pr(PrMsg::Detail { pr: pr.clone(), result: Ok(Box::new(detail.clone())) }));
    let out = screen(&mut app, 200, 24);
    assert!(!out.contains("u update branch") && !out.contains("x re-run failed"));

    detail.merge_status = MergeStatus::Behind;
    detail.checks[0].state = CheckState::Failed;
    detail.checks[0].target = crate::pulls::model::CheckTarget::ActionsRun { run_id: 5 };
    app.apply(DataMsg::Pr(PrMsg::Detail { pr, result: Ok(Box::new(detail)) }));
    let out = screen(&mut app, 200, 24);
    assert!(out.contains("u update branch") && out.contains("x re-run failed"));
}

#[test]
fn the_merge_box_lists_methods_warns_when_blocked_and_hides_branch_deletion_for_forks() {
    use crate::app::{DataMsg, PrMsg};
    use crate::pulls::model::{MergeMethod, QueueState};
    use crossterm::event::{KeyCode, KeyEvent};
    let mut app = prs_app();
    app.handle_key(KeyEvent::from(KeyCode::Enter));
    let pr = app.prs.open.as_ref().unwrap().pr.clone();
    let mut detail = pr_detail();
    detail.queue = QueueState::NoQueue;
    detail.merge_methods = vec![MergeMethod::Squash, MergeMethod::Rebase];
    app.apply(DataMsg::Pr(PrMsg::Detail { pr: pr.clone(), result: Ok(Box::new(detail.clone())) }));
    assert!(screen(&mut app, 200, 24).contains("m merge"), "the footer offers it");

    app.handle_key(KeyEvent::from(KeyCode::Char('m')));
    let out = screen(&mut app, 140, 30);
    assert!(out.contains("Merge org/api#7"));
    assert!(out.contains("Squash and merge") && out.contains("Rebase and merge"));
    assert!(!out.contains("Create a merge commit"), "only the allowed methods");
    // The warning wraps in the box, so its two ends are checked.
    assert!(out.contains("Blocked: changes requested. Merging works only"));
    assert!(out.contains("protection."));
    assert!(out.contains("[ ] Delete branch main after merging"));
    app.handle_key(KeyEvent::from(KeyCode::Char('d')));
    assert!(screen(&mut app, 140, 30).contains("[x] Delete branch main"));
    app.handle_key(KeyEvent::from(KeyCode::Esc));

    detail.head.repo = "fork/api".into();
    app.apply(DataMsg::Pr(PrMsg::Detail { pr, result: Ok(Box::new(detail)) }));
    app.handle_key(KeyEvent::from(KeyCode::Char('m')));
    let out = screen(&mut app, 140, 30);
    assert!(out.contains("Squash and merge"));
    assert!(!out.to_lowercase().contains("delete branch"), "{out}");
}

#[test]
fn the_footer_names_the_queue_key_by_what_it_does() {
    use crate::app::{DataMsg, PrMsg};
    use crate::pulls::model::QueueState;
    use crossterm::event::{KeyCode, KeyEvent};
    let mut app = prs_app();
    app.handle_key(KeyEvent::from(KeyCode::Enter));
    let pr = app.prs.open.as_ref().unwrap().pr.clone();
    let mut detail = pr_detail();
    for (queue, hint) in [(QueueState::NotQueued, "m queue"), (QueueState::Queued { position: 1 }, "m unqueue")] {
        detail.queue = queue;
        app.apply(DataMsg::Pr(PrMsg::Detail { pr: pr.clone(), result: Ok(Box::new(detail.clone())) }));
        assert!(screen(&mut app, 200, 24).contains(hint), "{hint}");
    }
    detail.queue = QueueState::NoQueue;
    detail.merge_methods.clear();
    app.apply(DataMsg::Pr(PrMsg::Detail { pr: pr.clone(), result: Ok(Box::new(detail.clone())) }));
    assert!(!screen(&mut app, 200, 24).contains("m merge"), "not offered when no method is allowed");

    detail.queue = QueueState::Queued { position: 1 };
    detail.draft = true;
    app.apply(DataMsg::Pr(PrMsg::Detail { pr, result: Ok(Box::new(detail)) }));
    assert!(!screen(&mut app, 200, 24).contains("m unqueue"), "not offered on a draft");
}

#[test]
fn the_footer_offers_the_way_back_to_the_pr_after_a_jump() {
    use crate::app::{DataMsg, PrMsg};
    use crossterm::event::{KeyCode, KeyEvent};
    let mut app = prs_app();
    app.handle_key(KeyEvent::from(KeyCode::Enter));
    let pr = app.prs.open.as_ref().unwrap().pr.clone();
    app.apply(DataMsg::Pr(PrMsg::Detail { pr, result: Ok(Box::new(pr_detail())) }));
    app.handle_key(KeyEvent::from(KeyCode::Enter));
    assert!(matches!(app.mode, crate::app::Mode::Normal));
    assert!(screen(&mut app, 140, 24).contains("Esc back to PR"));
}

#[test]
fn a_failed_first_read_of_a_pr_shows_the_error() {
    use crate::app::{DataMsg, PrMsg};
    use crossterm::event::{KeyCode, KeyEvent};
    let mut app = prs_app();
    app.handle_key(KeyEvent::from(KeyCode::Enter));
    let pr = app.prs.open.as_ref().unwrap().pr.clone();
    app.apply(DataMsg::Pr(PrMsg::Detail { pr, result: Err("Could not resolve".into()) }));
    let out = screen(&mut app, 140, 24);
    assert!(out.contains("Could not resolve"));
}

#[test]
fn a_failed_search_shows_in_its_header_and_the_status_line() {
    use crate::app::{DataMsg, PrMsg};
    use crate::pulls::model::PrGroup;
    let mut app = prs_app();
    app.apply(DataMsg::Pr(PrMsg::List { group: PrGroup::Authored, result: Err("502".into()) }));
    let out = screen(&mut app, 140, 24);
    assert!(out.contains("Opened by you  none"), "the shown list stays");
    assert!(out.contains("Opened by you: 502"), "and the error is reported");
}

fn pr_file(path: &str, patch: Option<&str>) -> crate::pulls::model::PrFile {
    use crate::pulls::model::{FileStatus, PrFile};
    PrFile {
        path: path.into(),
        previous_path: None,
        status: FileStatus::Modified,
        additions: 3,
        deletions: 1,
        patch: patch.map(String::from),
    }
}

/// An app on the Files tab of PR 7 with these files loaded.
fn files_app(files: Vec<crate::pulls::model::PrFile>) -> App {
    files_app_with(files, pr_detail())
}

fn files_app_with(files: Vec<crate::pulls::model::PrFile>, detail: crate::pulls::model::PrDetail) -> App {
    use crate::app::{DataMsg, PrMsg};
    use crossterm::event::{KeyCode, KeyEvent};
    let mut app = prs_app();
    app.handle_key(KeyEvent::from(KeyCode::Enter));
    let pr = app.prs.open.as_ref().unwrap().pr.clone();
    app.apply(DataMsg::Pr(PrMsg::Detail { pr: pr.clone(), result: Ok(Box::new(detail)) }));
    app.handle_key(KeyEvent::from(KeyCode::Tab));
    app.apply(DataMsg::Pr(PrMsg::Files { pr, result: Ok(files) }));
    app
}

fn press(app: &mut App, code: crossterm::event::KeyCode) {
    app.handle_key(crossterm::event::KeyEvent::from(code));
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, crossterm::event::KeyCode::Char(c));
    }
}

#[test]
fn the_files_tab_fetches_once_and_lists_the_files_with_a_count() {
    use crate::app::{Command, PrCommand};
    use crossterm::event::{KeyCode, KeyEvent};
    let mut app = prs_app();
    app.handle_key(KeyEvent::from(KeyCode::Enter));
    app.pending.clear();
    app.handle_key(KeyEvent::from(KeyCode::Tab));
    assert!(matches!(&app.pending[..], [Command::Pr(PrCommand::FetchFiles(pr))] if pr.number == 7));
    assert!(screen(&mut app, 140, 24).contains("loading"));
    let pr = app.prs.open.as_ref().unwrap().pr.clone();
    let files = vec![pr_file("src/a.rs", Some("@@ -1 +1 @@\n-a\n+b")), pr_file("README.md", None)];
    app.apply(crate::app::DataMsg::Pr(crate::app::PrMsg::Files { pr, result: Ok(files) }));
    let out = screen(&mut app, 140, 24);
    assert!(out.contains("Files 2"));
    assert!(out.contains("M src/a.rs"));
    assert!(out.contains("+3 \u{2212}1"));
    assert!(out.contains("M README.md"));

    // Coming back to the tab with the files loaded asks for nothing.
    app.handle_key(KeyEvent::from(KeyCode::Tab));
    app.pending.clear();
    app.handle_key(KeyEvent::from(KeyCode::BackTab));
    app.handle_key(KeyEvent::from(KeyCode::Tab));
    app.handle_key(KeyEvent::from(KeyCode::BackTab));
    assert!(app.pending.is_empty());
}

#[test]
fn the_diff_draws_tabs_as_spaces_and_clips_long_lines() {
    use crossterm::event::KeyCode;
    let patch = format!("@@ -1,2 +1,3 @@\n ctx\n+\tlet x = 1;\n+{}END", "x".repeat(500));
    let mut app = files_app(vec![pr_file("src/a.rs", Some(&patch))]);
    press(&mut app, KeyCode::Enter);
    let out = screen(&mut app, 100, 24);
    assert!(out.contains("    let x = 1;"));
    assert!(!out.contains("END"), "the long line is cut at the pane");
    let long = out.lines().find(|l| l.contains("xxxxxxxx")).unwrap();
    assert!(long.trim_end().ends_with('\u{2502}'), "the border stays put: {long}");
    assert!(out.lines().all(|l| l.chars().count() == 100));
}

#[test]
fn a_file_without_a_patch_says_so() {
    use crate::app::Command;
    use crossterm::event::KeyCode;
    let mut app = files_app(vec![pr_file("logo.png", None)]);
    let out = screen(&mut app, 160, 24);
    assert!(
        out.contains("GitHub doesn't send this diff (binary, too large, or no text changes). o opens it on github.com."),
        "{out}"
    );
    press(&mut app, KeyCode::Enter);
    assert!(screen(&mut app, 160, 24).contains("GitHub doesn't send this diff"));
    app.pending.clear();
    press(&mut app, KeyCode::Char('o'));
    assert!(matches!(&app.pending[..], [Command::OpenUrl(u)] if u == "https://github.com/org/api/pull/7/files"));
}

#[test]
fn a_pure_rename_names_the_old_path_and_has_no_missing_diff_message() {
    use crate::pulls::model::FileStatus;
    let mut renamed = pr_file("src/new.rs", None);
    renamed.previous_path = Some("src/old.rs".into());
    renamed.status = FileStatus::Renamed;
    renamed.additions = 0;
    renamed.deletions = 0;
    let mut app = files_app(vec![renamed]);
    let out = screen(&mut app, 160, 24);
    assert!(out.contains("R src/old.rs \u{2192} src/new.rs"), "{out}");
    assert!(out.contains("Renamed from src/old.rs with no content changes."), "{out}");
    assert!(!out.contains("GitHub doesn't send"));
}

#[test]
fn a_renamed_file_with_changes_shows_old_to_new_in_the_diff_header() {
    use crate::pulls::model::FileStatus;
    use crossterm::event::KeyCode;
    let mut renamed = pr_file("src/new.rs", Some("@@ -1 +1 @@\n-a\n+b"));
    renamed.previous_path = Some("src/old.rs".into());
    renamed.status = FileStatus::Renamed;
    let mut app = files_app(vec![renamed]);
    press(&mut app, KeyCode::Enter);
    let out = screen(&mut app, 160, 24);
    assert!(out.matches("src/old.rs \u{2192} src/new.rs").count() >= 2, "{out}");
}

#[test]
fn a_detail_refresh_that_adds_a_thread_marks_the_open_diff() {
    use crate::app::{DataMsg, PrMsg};
    use crate::pulls::model::{ReviewThread, Side};
    use crossterm::event::KeyCode;
    let mut app = files_app(vec![pr_file("a.rs", Some("@@ -1 +1 @@\n-a\n+b"))]);
    press(&mut app, KeyCode::Enter);
    assert!(!screen(&mut app, 140, 24).contains('\u{1F4AC}'));
    let mut detail = pr_detail();
    detail.threads = vec![ReviewThread {
        id: "T".into(),
        resolved: false,
        outdated: false,
        path: "a.rs".into(),
        lines: Some(crate::pulls::model::LineSpan::One(1)),
        side: Side::Right,
        comments: vec![],
    }];
    let pr = app.prs.open.as_ref().unwrap().pr.clone();
    app.apply(DataMsg::Pr(PrMsg::Detail { pr, result: Ok(Box::new(detail)) }));
    assert!(screen(&mut app, 140, 24).contains('\u{1F4AC}'));
}

fn thread_at(path: &str, line: Option<u32>, outdated: bool) -> crate::pulls::model::ReviewThread {
    use crate::pulls::model::{Comment, ReviewThread, Side};
    ReviewThread {
        id: "T".into(),
        resolved: false,
        outdated,
        path: path.into(),
        lines: line.map(crate::pulls::model::LineSpan::One),
        side: Side::Right,
        comments: vec![Comment {
            id: 1,
            author: "bob".into(),
            body: "Why this?".into(),
            created_at: chrono::Utc::now(),
        }],
    }
}

#[test]
fn an_outdated_thread_still_shows_in_the_conversation() {
    let mut detail = pr_detail();
    detail.threads = vec![thread_at("src/old.rs", None, true), thread_at("src/new.rs", Some(12), false)];
    let mut app = files_app_with(vec![], detail);
    press(&mut app, crossterm::event::KeyCode::Tab);
    let out = screen(&mut app, 140, 24);
    assert!(out.contains("src/old.rs"), "{out}");
    assert!(out.contains("outdated"), "{out}");
    assert!(out.contains("src/new.rs:12"), "{out}");
    assert!(out.contains("Why this?"), "{out}");
    assert!(out.contains("Conversation 2"), "the description is not counted: {out}");
    assert!(out.contains("⏎ threads"), "the footer says how to reach them: {out}");

    let mut app = files_app_with(vec![], pr_detail());
    press(&mut app, crossterm::event::KeyCode::Tab);
    assert!(!screen(&mut app, 140, 24).contains("⏎ threads"), "no threads, no hint");
}

#[test]
fn a_thread_on_several_lines_shows_its_range() {
    let mut detail = pr_detail();
    let mut thread = thread_at("src/new.rs", Some(12), false);
    thread.lines = Some(crate::pulls::model::LineSpan::new(10, 12));
    detail.threads = vec![thread];
    let mut app = files_app_with(vec![], detail);
    press(&mut app, crossterm::event::KeyCode::Tab);
    assert!(screen(&mut app, 140, 24).contains("src/new.rs:10-12"));
    press(&mut app, crossterm::event::KeyCode::Enter);
    assert!(screen(&mut app, 140, 30).contains("Review thread"), "Enter opens it");
}

#[test]
fn a_long_conversation_scrolls_to_its_last_line() {
    use crossterm::event::KeyCode;
    let mut detail = pr_detail();
    detail.comments = (1..=30)
        .map(|n| crate::pulls::model::Comment {
            id: n,
            author: "bob".into(),
            body: format!("note {n}"),
            created_at: chrono::Utc::now(),
        })
        .collect();
    let mut app = files_app_with(vec![], detail);
    press(&mut app, KeyCode::Tab);
    let first = screen(&mut app, 100, 20);
    assert!(!first.contains("note 30"), "{first}");
    press(&mut app, KeyCode::Char('G'));
    assert!(screen(&mut app, 100, 20).contains("note 30"));
    press(&mut app, KeyCode::Char('g'));
    assert!(!screen(&mut app, 100, 20).contains("note 30"));
    assert!(screen(&mut app, 100, 20).contains("note 1"));
    press(&mut app, KeyCode::PageDown);
    let paged = screen(&mut app, 100, 20);
    assert!(!paged.contains("note 1"), "the first notes are paged away: {paged}");
    assert!(paged.contains("note 5"), "{paged}");
}

#[test]
fn the_conversation_is_built_once_until_the_detail_or_width_changes() {
    use crate::app::{DataMsg, PrMsg};
    use crossterm::event::KeyCode;
    let mut app = files_app_with(vec![], pr_detail());
    press(&mut app, KeyCode::Tab);
    let builds = |app: &App| app.prs.open.as_ref().unwrap().conversation.builds;
    screen(&mut app, 100, 20);
    screen(&mut app, 100, 20);
    press(&mut app, KeyCode::Char('j'));
    screen(&mut app, 100, 20);
    assert_eq!(builds(&app), 1, "redrawing and scrolling reuse the built lines");
    screen(&mut app, 90, 20);
    assert_eq!(builds(&app), 2, "a new width rebuilds");
    let pr = app.prs.open.as_ref().unwrap().pr.clone();
    app.apply(DataMsg::Pr(PrMsg::Detail { pr, result: Ok(Box::new(pr_detail())) }));
    screen(&mut app, 90, 20);
    assert_eq!(builds(&app), 3, "a refreshed detail rebuilds");
}

#[test]
fn files_are_read_again_only_on_their_tab_or_on_r() {
    use crate::app::{Command, PrCommand};
    use crossterm::event::KeyCode;
    let mut app = prs_app();
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Tab);
    let fetches = |app: &App| {
        app.pending.iter().filter(|c| matches!(c, Command::Pr(PrCommand::FetchFiles(_)))).count()
    };
    assert_eq!(fetches(&app), 1);
    // Away and back while the first read is still out: no second one.
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::BackTab);
    assert_eq!(fetches(&app), 1);

    let pr = app.prs.open.as_ref().unwrap().pr.clone();
    app.apply(crate::app::DataMsg::Pr(crate::app::PrMsg::Files { pr, result: Ok(vec![]) }));
    app.pending.clear();
    app.queue_pr_refresh();
    assert_eq!(fetches(&app), 1, "the timer reads them on the Files tab");
    press(&mut app, KeyCode::BackTab);
    app.pending.clear();
    app.queue_pr_refresh();
    assert_eq!(fetches(&app), 0, "but not from the Overview tab");
    press(&mut app, KeyCode::Char('r'));
    assert_eq!(fetches(&app), 1, "r reads everything");
}

#[test]
fn g_shows_the_last_line_of_a_huge_diff() {
    use crossterm::event::KeyCode;
    let mut patch = String::from("@@ -1,15000 +1,15000 @@");
    for i in 0..15_000 {
        patch.push_str(&format!("\n-old {i}\n+new {i}"));
    }
    let mut app = files_app(vec![pr_file("big.txt", Some(&patch))]);
    press(&mut app, KeyCode::Enter);
    assert!(screen(&mut app, 140, 24).contains("old 0"));
    press(&mut app, KeyCode::Char('G'));
    let out = screen(&mut app, 140, 24);
    assert!(out.contains("new 14999"), "{out}");
    assert!(!out.contains("old 0"));
    press(&mut app, KeyCode::Char('g'));
    assert!(screen(&mut app, 140, 24).contains("old 0"));
}

#[test]
fn typing_a_search_does_not_trigger_other_keys_and_n_steps_matches() {
    use crossterm::event::KeyCode;
    let patch = "@@ -1,4 +1,4 @@\n alpha\n-quit\n+beta\n gamma quit";
    let mut app = files_app(vec![pr_file("a.rs", Some(patch))]);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('/'));
    type_text(&mut app, "quit");
    let out = screen(&mut app, 140, 24);
    assert!(out.contains("/quit"), "{out}");
    assert!(out.contains("2 matches"), "{out}");
    assert!(app.prs.open.is_some(), "q went into the box");
    press(&mut app, KeyCode::Enter);
    let at = |app: &App| app.prs.open.as_ref().unwrap().files.diff.as_ref().unwrap().cursor;
    assert_eq!(at(&app), 2);
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(at(&app), 4);
    press(&mut app, KeyCode::Char('N'));
    assert_eq!(at(&app), 2);
    press(&mut app, KeyCode::Esc);
    assert!(app.prs.open.as_ref().unwrap().files.diff.is_none(), "Esc returns to the list");
    assert!(screen(&mut app, 140, 24).contains("a.rs"));
    press(&mut app, KeyCode::Esc);
    assert!(app.prs.open.is_none());
}

#[test]
fn rows_with_review_threads_show_a_marker() {
    use crate::pulls::model::{ReviewThread, Side};
    use crossterm::event::KeyCode;
    let mut detail = pr_detail();
    detail.threads = vec![ReviewThread {
        id: "T".into(),
        resolved: false,
        outdated: false,
        path: "a.rs".into(),
        lines: Some(crate::pulls::model::LineSpan::One(1)),
        side: Side::Right,
        comments: vec![],
    }];
    let mut app = files_app_with(vec![pr_file("a.rs", Some("@@ -1 +1 @@\n-a\n+b"))], detail);
    press(&mut app, KeyCode::Enter);
    let out = screen(&mut app, 140, 24);
    let row = out.lines().find(|l| l.contains("+b")).unwrap();
    assert!(row.contains('\u{1F4AC}'), "{row}");
    let other = out.lines().find(|l| l.contains("-a")).unwrap();
    assert!(!other.contains('\u{1F4AC}'));
}

fn ctrl(app: &mut App, c: char) {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
}

#[test]
fn a_draft_comment_is_marked_on_its_row_and_counted_beside_the_tabs() {
    use crossterm::event::KeyCode;
    let mut app = files_app(vec![pr_file("a.rs", Some("@@ -1 +1 @@\n-a\n+b"))]);
    press(&mut app, KeyCode::Enter);
    let out = screen(&mut app, 200, 24);
    assert!(out.contains("v mark") && out.contains("c comment") && out.contains("R review"), "{out}");
    assert!(!out.contains("p pending") && !out.contains("pending"));

    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('c'));
    let out = screen(&mut app, 200, 30);
    assert!(out.contains("Comment on a.rs:1"), "{out}");
    assert!(out.contains("Ctrl+S add to review · Ctrl+G post alone · Esc cancel"), "{out}");
    type_text(&mut app, "note");
    ctrl(&mut app, 's');
    app.status_msg = None;

    let out = screen(&mut app, 200, 24);
    let row = out.lines().find(|l| l.contains("+b")).unwrap();
    assert!(row.contains('\u{270E}'), "{row}");
    assert!(!out.lines().find(|l| l.contains("-a")).unwrap().contains('\u{270E}'));
    assert!(out.contains("review: 1 pending"), "{out}");
    assert!(out.contains("p pending"), "{out}");
    assert!(out.lines().all(|l| l.chars().count() == 200), "the marker keeps the rows aligned");
}

#[test]
fn the_review_boxes_draw_over_the_pr() {
    use crossterm::event::KeyCode;
    let mut detail = pr_detail();
    detail.threads = vec![thread_at("a.rs", Some(1), false)];
    let mut app = files_app_with(vec![pr_file("a.rs", Some("@@ -1 +1 @@\n-a\n+b"))], detail);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('j'));
    assert!(screen(&mut app, 200, 24).contains("⏎ thread"));

    press(&mut app, KeyCode::Enter);
    let out = screen(&mut app, 140, 30);
    assert!(out.contains("Review thread"), "{out}");
    assert!(out.contains("Why this?"), "{out}");
    assert!(out.contains("j/k scroll · r reply · x resolve · Esc close"), "{out}");
    press(&mut app, KeyCode::Esc);

    press(&mut app, KeyCode::Char('c'));
    type_text(&mut app, "Rename it");
    ctrl(&mut app, 's');
    press(&mut app, KeyCode::Char('p'));
    let out = screen(&mut app, 140, 30);
    assert!(out.contains("Pending review of org/api#7"), "{out}");
    assert!(out.contains("› a.rs:1  Rename it"), "{out}");
    press(&mut app, KeyCode::Char('R'));
    let out = screen(&mut app, 140, 30);
    assert!(out.contains("Review org/api#7"), "{out}");
    assert!(out.contains("› Approve") && out.contains("Request changes"), "{out}");
    assert!(out.contains("Sends 1 comment with the verdict."), "{out}");
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Enter);
    assert!(screen(&mut app, 140, 30).contains("Request changes org/api#7"));
}

#[test]
fn the_verdict_picker_warns_when_the_branch_moved_under_the_review() {
    use crate::app::{DataMsg, PrMsg};
    use crossterm::event::KeyCode;
    const WARNING: &str = "The branch has new commits since you started this review.";
    let mut app = files_app(vec![pr_file("a.rs", Some("@@ -1 +1 @@\n-a\n+b"))]);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('c'));
    type_text(&mut app, "note");
    ctrl(&mut app, 's');
    press(&mut app, KeyCode::Char('R'));
    assert!(!screen(&mut app, 140, 30).contains(WARNING));
    press(&mut app, KeyCode::Esc);

    let mut detail = pr_detail();
    detail.head.oid = "newer".into();
    let pr = app.prs.open.as_ref().unwrap().pr.clone();
    app.apply(DataMsg::Pr(PrMsg::Detail { pr, result: Ok(Box::new(detail)) }));
    press(&mut app, KeyCode::Char('R'));
    let out = screen(&mut app, 140, 30);
    assert!(out.contains(WARNING), "{out}");
}

#[test]
fn a_refresh_keeps_the_open_diff_on_its_file_and_the_count_right() {
    use crossterm::event::KeyCode;
    let patch = "@@ -1,4 +1,4 @@\n a\n b\n c\n-d\n+e";
    let mut app = files_app(vec![pr_file("one.rs", Some("@@ -1 +1 @@\n-x\n+y")), pr_file("two.rs", Some(patch))]);
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('j'));
    let pr = app.prs.open.as_ref().unwrap().pr.clone();
    let files = vec![
        pr_file("new.rs", None),
        pr_file("one.rs", Some("@@ -1 +1 @@\n-x\n+y")),
        pr_file("two.rs", Some(patch)),
    ];
    app.apply(crate::app::DataMsg::Pr(crate::app::PrMsg::Files { pr: pr.clone(), result: Ok(files) }));
    let out = screen(&mut app, 140, 24);
    assert!(out.contains("Files 3"), "{out}");
    let files_state = &app.prs.open.as_ref().unwrap().files;
    let diff = files_state.diff.as_ref().unwrap();
    assert_eq!((diff.file, diff.cursor), (2, 2));
    assert_eq!(files_state.sel.selected(), Some(2));

    // The file leaves the PR, so its diff closes; a failed refresh changes nothing.
    let files = vec![pr_file("new.rs", None)];
    app.apply(crate::app::DataMsg::Pr(crate::app::PrMsg::Files { pr: pr.clone(), result: Ok(files) }));
    assert!(app.prs.open.as_ref().unwrap().files.diff.is_none());
    app.apply(crate::app::DataMsg::Pr(crate::app::PrMsg::Files { pr, result: Err("502".into()) }));
    assert!(screen(&mut app, 140, 24).contains("Files 1"));
}
