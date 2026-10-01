//! The reviewer `i` menu on a pull request (PR mode v2, phase 6): it
//! replaces the plain Codex menu while a PR is on screen, `tab` widens its
//! scope, and its actions dispatch through the same `dispatch_codex` plumbing
//! the plain menu uses.
//!
//! **No test here can reach the real `codex` binary — or even let
//! `dispatch_codex`'s background task run at all.** `dispatch_codex` gathers
//! material and runs Codex inside `cx.background_spawn(smol::unblock(...))`;
//! gpui's deterministic test scheduler treats any waker firing from that
//! background thread as nondeterminism and panics the test, whatever the
//! task actually does. So no test here ever calls `cx.run_until_parked()`
//! (or otherwise drains the executor) after a keystroke that dispatches —
//! the background task is scheduled but simply never polled. What a
//! dispatch *would* have sent is instead checked through the synchronous
//! state `dispatch_codex` sets up before backgrounding anything:
//! `codex_run_title_for_test`, `last_dispatch_instructions_for_test`, and
//! `reviewer_scope_material_for_test` (the pure scope-to-`Material` mapping,
//! computed without ever calling `gather`).

use super::*;
use crate::reviewer::{AgentDoc, ReviewerConfig};
use crate::view::codex_panel::Material;
use crate::view::reviewer_menu::ReviewScope;

fn pr_file(path: &str) -> crate::github::PullRequestFile {
    crate::github::PullRequestFile {
        path: path.to_string(),
        additions: 1,
        deletions: 0,
    }
}

fn reviewer_pr_detail(
    number: u64,
    base_oid: &str,
    head_oid: &str,
    files: &[&str],
) -> crate::github::PullRequestDetail {
    crate::github::PullRequestDetail {
        number,
        title: "Title".to_string(),
        body: "Body text.".to_string(),
        body_truncated: false,
        url: String::new(),
        author: String::new(),
        created_at: String::new(),
        head: "feature".to_string(),
        head_oid: head_oid.to_string(),
        base: "main".to_string(),
        base_oid: base_oid.to_string(),
        is_draft: false,
        is_cross_repository: false,
        state: "OPEN".to_string(),
        review: None,
        mergeable: None,
        additions: 1,
        deletions: 1,
        changed_files: files.len() as u64,
        files: files.iter().map(|path| pr_file(path)).collect(),
        checks: Default::default(),
        check_runs: vec![],
        conversation: vec![],
        reviewers: vec![],
        commits: vec![],
    }
}

/// Review mode, with a PR detail seeded so `reviewer_context` resolves: the
/// setup every test here starts from. `base_oid`/`head_oid` need not be real
/// git commits unless the test also drives `dispatch_codex`'s background
/// gather to completion.
fn open_review_with_detail(
    cx: &mut gpui::VisualTestContext,
    view: &gpui::Entity<super::super::super::GitCometView>,
    repo_id: RepoId,
    number: u64,
    workdir: &std::path::Path,
    base_oid: &str,
    head_oid: &str,
) {
    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            let repo = opening_repo_state(repo_id, workdir);
            let next_state = Arc::new(AppState {
                repos: vec![repo],
                active_repo: Some(repo_id),
                sidebar_mode: gitcomet_state::model::SidebarMode::PullRequests,
                ..AppState::test_default()
            });
            push_test_state(this, next_state, cx);
            this.open_review_for_test(repo_id, number, vec!["a.rs".to_string()], head_oid, cx);
            this.seed_pull_request_detail_for_test(
                repo_id,
                reviewer_pr_detail(number, base_oid, head_oid, &["a.rs"]),
                base_oid.to_string(),
            );
        });
    });
    draw_and_drain_test_window(cx);
}

/// `i` opens the reviewer menu on a pull request — from review mode, and
/// from the PR tab alone (no review mode) — and the plain Codex menu
/// everywhere else.
#[gpui::test]
fn i_shows_reviewer_menu_on_a_pr_and_the_plain_menu_elsewhere(cx: &mut gpui::TestAppContext) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        super::super::super::GitCometView::new(store, events, None, window, cx)
    });
    bind_app_keys_and_global_diff_fallback_for_test(cx);

    let repo_id = RepoId(80101);
    let workdir = std::env::temp_dir().join(format!(
        "gitcomet_ui_test_{}_reviewer_menu_open",
        std::process::id()
    ));
    open_review_with_detail(cx, &view, repo_id, 42, &workdir, "base1", "head1");

    cx.simulate_keystrokes("i");
    draw_and_drain_test_window(cx);
    let (reviewer_open, codex_open) = cx.update(|_window, app| {
        let this = view.read(app);
        (this.reviewer_menu.is_some(), this.codex_menu_open)
    });
    assert!(reviewer_open, "i on a PR must open the reviewer menu");
    assert!(!codex_open, "the plain Codex menu must not also open");

    // Leave review mode but stay on the PR tab with the PR selected: `i`
    // must still open the reviewer menu.
    cx.simulate_keystrokes("escape");
    draw_and_drain_test_window(cx);
    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            this.review = None;
            this.pull_requests.repo_mut(repo_id).selected = Some(42);
            cx.notify();
        });
    });
    draw_and_drain_test_window(cx);
    cx.simulate_keystrokes("i");
    draw_and_drain_test_window(cx);
    let reviewer_open_from_pr_tab =
        cx.update(|_window, app| view.read(app).reviewer_menu.is_some());
    assert!(
        reviewer_open_from_pr_tab,
        "i on the PR tab alone (no review mode) must also open the reviewer menu"
    );
    cx.simulate_keystrokes("escape");
    draw_and_drain_test_window(cx);

    // Leave the PR entirely: `i` now opens the plain menu.
    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            let repo = opening_repo_state(RepoId(80102), &workdir);
            let next_state = Arc::new(AppState {
                repos: vec![repo],
                active_repo: Some(RepoId(80102)),
                sidebar_mode: gitcomet_state::model::SidebarMode::Files,
                ..AppState::test_default()
            });
            push_test_state(this, next_state, cx);
        });
    });
    draw_and_drain_test_window(cx);

    cx.simulate_keystrokes("i");
    draw_and_drain_test_window(cx);
    let (reviewer_open, codex_open) = cx.update(|_window, app| {
        let this = view.read(app);
        (this.reviewer_menu.is_some(), this.codex_menu_open)
    });
    assert!(!reviewer_open, "i outside a PR must not open the reviewer menu");
    assert!(codex_open, "i outside a PR must open the plain Codex menu");

    let _ = std::fs::remove_dir_all(&workdir);
}

/// `tab` widens the scope one step at a time: File -> Commits -> Pr, then
/// stays there.
#[gpui::test]
fn tab_widens_the_reviewer_scope(cx: &mut gpui::TestAppContext) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        super::super::super::GitCometView::new(store, events, None, window, cx)
    });
    bind_app_keys_and_global_diff_fallback_for_test(cx);

    let repo_id = RepoId(80111);
    let workdir = std::env::temp_dir().join(format!(
        "gitcomet_ui_test_{}_reviewer_menu_tab",
        std::process::id()
    ));
    open_review_with_detail(cx, &view, repo_id, 43, &workdir, "base1", "head1");

    cx.simulate_keystrokes("i");
    draw_and_drain_test_window(cx);
    let scope = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_window, app| view.read(app).reviewer_menu.map(|state| state.scope))
    };
    // A file is open (review mode's file 0) but no line range is selected,
    // so the starting scope is File.
    assert_eq!(scope(cx), Some(ReviewScope::File));

    cx.simulate_keystrokes("tab");
    draw_and_drain_test_window(cx);
    assert_eq!(scope(cx), Some(ReviewScope::Commits));

    cx.simulate_keystrokes("tab");
    draw_and_drain_test_window(cx);
    assert_eq!(scope(cx), Some(ReviewScope::Pr));

    cx.simulate_keystrokes("tab");
    draw_and_drain_test_window(cx);
    assert_eq!(scope(cx), Some(ReviewScope::Pr), "Pr is the widest scope");

    let _ = std::fs::remove_dir_all(&workdir);
}

/// Pressing an action's key dispatches with the expected scope's `Material`
/// (checked through the pure, no-IO `reviewer_scope_material_for_test` seam)
/// and the expected `.reviewer` involvement: none when no folder was
/// involved, and the trusted README text in the *instructions* — never the
/// gathered material — once one is seeded.
#[gpui::test]
fn an_action_dispatches_with_the_expected_scope_and_material(cx: &mut gpui::TestAppContext) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        super::super::super::GitCometView::new(store, events, None, window, cx)
    });
    bind_app_keys_and_global_diff_fallback_for_test(cx);

    let repo_id = RepoId(80121);
    let workdir = std::env::temp_dir().join(format!(
        "gitcomet_ui_test_{}_reviewer_menu_dispatch",
        std::process::id()
    ));
    open_review_with_detail(cx, &view, repo_id, 46, &workdir, "base1", "head1");

    // The File scope's material is exactly `a.rs`'s own diff between the
    // review's base and head — never the whole range.
    let material = cx.update(|_window, app| {
        view.read(app).reviewer_scope_material_for_test(ReviewScope::File, app)
    });
    assert_eq!(
        material,
        Ok(Material::ReviewerScopeDiff {
            base: "base1".to_string(),
            head: "head1".to_string(),
            path: Some("a.rs".to_string()),
            generated: Default::default(),
        })
    );

    // Test builds never run the real `.reviewer` load, so seed an empty
    // config (the built-in reviewer) before dispatching.
    cx.update(|_window, app| {
        view.update(app, |this, _cx| {
            this.seed_reviewer_config_for_test("base1", ReviewerConfig::default());
        })
    });
    cx.simulate_keystrokes("i");
    cx.simulate_keystrokes("e");
    draw_and_drain_test_window(cx);

    let (run_title, instructions) = cx.update(|_window, app| {
        let this = view.read(app);
        (
            this.codex_run_title_for_test(repo_id),
            this.last_dispatch_instructions_for_test(),
        )
    });
    assert_eq!(run_title.as_deref(), Some("Explain this"));
    assert!(
        !instructions.unwrap_or_default().contains(".reviewer"),
        "no .reviewer folder was involved; instructions must say nothing about it"
    );

    // Seed a `.reviewer/README.md` directly (bypassing the real git-backed
    // load) and dispatch again: its text must appear in the instructions.
    cx.update(|_window, app| {
        view.update(app, |this, _cx| {
            this.seed_reviewer_config_for_test(
                "base1",
                ReviewerConfig {
                    readme: Some("Mind the untrusted-data boundary.".to_string()),
                    ..Default::default()
                },
            );
        });
    });
    cx.simulate_keystrokes("i");
    draw_and_drain_test_window(cx);
    cx.simulate_keystrokes("t");
    draw_and_drain_test_window(cx);

    let instructions = cx.update(|_window, app| {
        view.read(app).last_dispatch_instructions_for_test().unwrap_or_default()
    });
    assert!(
        instructions.contains("Mind the untrusted-data boundary"),
        "seeded .reviewer text must reach the trusted instructions: {instructions:?}"
    );

    let _ = std::fs::remove_dir_all(&workdir);
}

/// `h` (Thread) is disabled with no thread under the cursor: it shows why
/// instead of dispatching, and the menu stays open.
#[gpui::test]
fn h_is_disabled_without_a_thread_under_the_cursor(cx: &mut gpui::TestAppContext) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        super::super::super::GitCometView::new(store, events, None, window, cx)
    });
    bind_app_keys_and_global_diff_fallback_for_test(cx);

    let repo_id = RepoId(80151);
    let workdir = std::env::temp_dir().join(format!(
        "gitcomet_ui_test_{}_reviewer_menu_h_disabled",
        std::process::id()
    ));
    open_review_with_detail(cx, &view, repo_id, 47, &workdir, "base1", "head1");

    cx.simulate_keystrokes("i");
    draw_and_drain_test_window(cx);
    cx.simulate_keystrokes("h");
    draw_and_drain_test_window(cx);

    let (still_open, toasts) = cx.update(|_window, app| {
        let this = view.read(app);
        (
            this.reviewer_menu.is_some(),
            this.toast_host.read(app).toasts_for_tests(app),
        )
    });
    assert!(still_open, "a disabled row must not close the menu");
    assert!(
        toasts.iter().any(|(_, message)| message.contains("No thread under the cursor")),
        "expected a toast explaining why h is disabled, got {toasts:?}"
    );

    let _ = std::fs::remove_dir_all(&workdir);
}

/// A `.reviewer/agents` entry gets a `1`-`9` row and dispatches with its own
/// title and body as the instructions' task, the same as any other action.
#[gpui::test]
fn an_agent_key_dispatches_its_own_instructions(cx: &mut gpui::TestAppContext) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        super::super::super::GitCometView::new(store, events, None, window, cx)
    });
    bind_app_keys_and_global_diff_fallback_for_test(cx);

    let repo_id = RepoId(80161);
    let workdir = std::env::temp_dir().join(format!(
        "gitcomet_ui_test_{}_reviewer_menu_agent_key",
        std::process::id()
    ));
    open_review_with_detail(cx, &view, repo_id, 48, &workdir, "base1", "head1");
    cx.update(|_window, app| {
        view.update(app, |this, _cx| {
            this.seed_reviewer_config_for_test(
                "base1",
                ReviewerConfig {
                    agents: vec![AgentDoc {
                        name: "agents/security.md".to_string(),
                        title: "Security review".to_string(),
                        key: '3',
                        scope: None,
                        paths: vec![],
                        body: "Look specifically for injection risks.".to_string(),
                    }],
                    ..Default::default()
                },
            );
        });
    });

    cx.simulate_keystrokes("i");
    draw_and_drain_test_window(cx);
    cx.simulate_keystrokes("3");
    // No `run_until_parked`: the background gather/run is scheduled but
    // never polled, so it never reaches a `codex` process.
    draw_and_drain_test_window(cx);

    let (menu_closed, run_title) = cx.update(|_window, app| {
        let this = view.read(app);
        (this.reviewer_menu.is_none(), this.codex_run_title_for_test(repo_id))
    });
    assert!(menu_closed);
    assert_eq!(run_title.as_deref(), Some("Security review"));

    let _ = std::fs::remove_dir_all(&workdir);
}

/// Commits and Whole PR scopes must send their own real touched files, not
/// an empty placeholder that `ReviewerConfig` used to read as "match every
/// area" (AGENTS.md MED finding): an area scoped to `b.rs` must stay out of
/// Commits' instructions when only `a.rs` is on screen, and must join Whole
/// PR's once `b.rs` is one of the PR's own changed files.
#[gpui::test]
fn commits_and_pr_scopes_use_their_own_real_files_not_every_area(cx: &mut gpui::TestAppContext) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        super::super::super::GitCometView::new(store, events, None, window, cx)
    });
    bind_app_keys_and_global_diff_fallback_for_test(cx);

    let repo_id = RepoId(80173);
    let workdir = std::env::temp_dir().join(format!(
        "gitcomet_ui_test_{}_reviewer_menu_scope_files",
        std::process::id()
    ));
    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            let repo = opening_repo_state(repo_id, &workdir);
            let next_state = Arc::new(AppState {
                repos: vec![repo],
                active_repo: Some(repo_id),
                sidebar_mode: gitcomet_state::model::SidebarMode::PullRequests,
                ..AppState::test_default()
            });
            push_test_state(this, next_state, cx);
            // The on-screen range covers only `a.rs`; the PR's own full
            // changed-file list (`context.changed_files`) also has `b.rs`.
            this.open_review_for_test(repo_id, 53, vec!["a.rs".to_string()], "head1", cx);
            this.seed_pull_request_detail_for_test(
                repo_id,
                reviewer_pr_detail(53, "base1", "head1", &["a.rs", "b.rs"]),
                "base1".to_string(),
            );
            this.seed_reviewer_config_for_test(
                "base1",
                ReviewerConfig {
                    areas: vec![crate::reviewer::AreaDoc {
                        name: "areas/b.md".to_string(),
                        paths: vec!["b.rs".to_string()],
                        body: "Mind b.rs's own invariant.".to_string(),
                    }],
                    ..Default::default()
                },
            );
        });
    });
    draw_and_drain_test_window(cx);

    // File -> Commits: still just `a.rs`, so the `b.rs`-scoped area stays out.
    cx.simulate_keystrokes("i");
    draw_and_drain_test_window(cx);
    cx.simulate_keystrokes("tab");
    draw_and_drain_test_window(cx);
    cx.simulate_keystrokes("e");
    draw_and_drain_test_window(cx);
    let commits_instructions = cx.update(|_window, app| {
        view.read(app).last_dispatch_instructions_for_test().unwrap_or_default()
    });
    assert!(
        !commits_instructions.contains("Mind b.rs's own invariant"),
        "Commits scope must not pull in an area that doesn't touch its files: {commits_instructions:?}"
    );

    // Pr (reopening the menu resets to its initial scope, File; two `tab`s
    // widen past Commits to Pr): the PR's real changed files include `b.rs`,
    // so the area joins.
    cx.simulate_keystrokes("i");
    draw_and_drain_test_window(cx);
    cx.simulate_keystrokes("tab");
    draw_and_drain_test_window(cx);
    cx.simulate_keystrokes("tab");
    draw_and_drain_test_window(cx);
    cx.simulate_keystrokes("e");
    draw_and_drain_test_window(cx);
    let pr_instructions = cx.update(|_window, app| {
        view.read(app).last_dispatch_instructions_for_test().unwrap_or_default()
    });
    assert!(
        pr_instructions.contains("Mind b.rs's own invariant"),
        "Whole PR scope must use the PR's real changed files: {pr_instructions:?}"
    );

    let _ = std::fs::remove_dir_all(&workdir);
}

/// `i s` (draft my review summary) must open the review/submit dialog itself
/// — the same one `S` opens — rather than leaving `fill_pull_request_review_draft`
/// with nowhere to put the answer (AGENTS.md MED finding).
#[gpui::test]
fn draft_summary_opens_the_review_dialog(cx: &mut gpui::TestAppContext) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        super::super::super::GitCometView::new(store, events, None, window, cx)
    });
    bind_app_keys_and_global_diff_fallback_for_test(cx);

    let repo_id = RepoId(80172);
    let workdir = std::env::temp_dir().join(format!(
        "gitcomet_ui_test_{}_reviewer_menu_draft_summary",
        std::process::id()
    ));
    open_review_with_detail(cx, &view, repo_id, 52, &workdir, "base1", "head1");
    cx.update(|_window, app| {
        view.update(app, |this, _| {
            this.seed_reviewer_config_for_test("base1", ReviewerConfig::default());
        });
    });

    cx.simulate_keystrokes("i");
    draw_and_drain_test_window(cx);
    cx.simulate_keystrokes("s");
    draw_and_drain_test_window(cx);

    let (dialog_open, run_title) = cx.update(|_window, app| {
        let this = view.read(app);
        let kind = PopoverKind::PullRequestReview {
            repo_id,
            number: 52,
            kind: crate::github::ReviewKind::Comment,
        };
        (
            this.popover_host.read(app).is_kind_open(&kind),
            this.codex_run_title_for_test(repo_id),
        )
    });
    assert!(dialog_open, "`i s` must open the review/submit dialog");
    assert_eq!(run_title.as_deref(), Some("Draft my review summary"));

    let _ = std::fs::remove_dir_all(&workdir);
}

/// A rule review (`i r`) run on a historical commit range (one ending before
/// the PR head) must go to the Panel, never into the hidden
/// `ReviewSuggestions` queue: its findings carry the range-head's line
/// numbers, which don't line up with the PR head diff `Shift+C` -> All
/// changes shows once the range is left (AGENTS.md HIGH finding on
/// `reviewer_menu.rs`/`review.rs`).
#[gpui::test]
fn rule_review_on_a_historical_range_goes_to_the_panel(cx: &mut gpui::TestAppContext) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        super::super::super::GitCometView::new(store, events, None, window, cx)
    });
    bind_app_keys_and_global_diff_fallback_for_test(cx);

    let repo_id = RepoId(80171);
    let workdir = std::env::temp_dir().join(format!(
        "gitcomet_ui_test_{}_reviewer_menu_historical_range",
        std::process::id()
    ));
    open_review_with_detail(cx, &view, repo_id, 51, &workdir, "base1", "head1");
    cx.update(|_window, app| {
        view.update(app, |this, _| {
            this.seed_reviewer_config_for_test("base1", ReviewerConfig::default());
        });
    });
    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            this.set_review_commit_range(
                Some(crate::view::pull_requests::SelectedCommitRange {
                    oldest_oid: "old1".to_string(),
                    newest_oid: "old1".to_string(),
                    count: 1,
                    total: 3,
                }),
                cx,
            );
        });
    });
    draw_and_drain_test_window(cx);
    assert!(
        cx.update(|_window, app| view
            .read(app)
            .active_review()
            .is_some_and(crate::view::review::ReviewMode::historical_range)),
        "the range must end before the PR head"
    );

    // File -> Commits -> Pr: Pr's material never depends on the (still
    // `Loading`) commit-range changes, so the dispatch actually runs.
    cx.simulate_keystrokes("i");
    draw_and_drain_test_window(cx);
    cx.simulate_keystrokes("tab");
    cx.simulate_keystrokes("tab");
    draw_and_drain_test_window(cx);
    cx.simulate_keystrokes("r");
    // No `run_until_parked`: the background gather/run is scheduled but
    // never polled, so it never reaches a `codex` process.
    draw_and_drain_test_window(cx);

    let (run_title, destination) = cx.update(|_window, app| {
        let this = view.read(app);
        (
            this.codex_run_title_for_test(repo_id),
            this.last_dispatch_destination_for_test(),
        )
    });
    assert!(run_title.is_some(), "the rule review must have dispatched");
    assert_eq!(
        destination,
        Some(crate::view::codex_panel::CodexDestination::Panel),
        "a historical range must never route findings to ReviewSuggestions"
    );

    let _ = std::fs::remove_dir_all(&workdir);
}

/// `i q` with an empty question leaves a reviewer Ask pending on the PR it
/// was opened for. Leaving the Pull requests tab (and review mode) for
/// Branches must drop it, so a later plain `q` -> Enter in the classic Codex
/// menu never fires it against a PR that's no longer on screen (AGENTS.md
/// MED finding on `pending_reviewer_ask`).
#[gpui::test]
fn pending_reviewer_ask_is_dropped_once_the_pr_tab_is_left(cx: &mut gpui::TestAppContext) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        super::super::super::GitCometView::new(store, events, None, window, cx)
    });
    bind_app_keys_and_global_diff_fallback_for_test(cx);

    let repo_id = RepoId(80176);
    let workdir = std::env::temp_dir().join(format!(
        "gitcomet_ui_test_{}_reviewer_menu_pending_ask",
        std::process::id()
    ));
    open_review_with_detail(cx, &view, repo_id, 61, &workdir, "base1", "head1");
    cx.update(|_window, app| {
        view.update(app, |this, _| {
            this.seed_reviewer_config_for_test("base1", ReviewerConfig::default());
        });
    });

    cx.simulate_keystrokes("i");
    draw_and_drain_test_window(cx);
    cx.simulate_keystrokes("q");
    draw_and_drain_test_window(cx);
    assert!(
        cx.update(|_window, app| view.read(app).pending_reviewer_ask.is_some()),
        "q with no question must leave a pending reviewer ask"
    );

    // Leave review mode and switch off the Pull requests tab.
    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            this.review = None;
            let next_state = Arc::new(AppState {
                repos: vec![opening_repo_state(repo_id, &workdir)],
                active_repo: Some(repo_id),
                sidebar_mode: gitcomet_state::model::SidebarMode::Branches,
                ..AppState::test_default()
            });
            push_test_state(this, next_state, cx);
        });
    });
    draw_and_drain_test_window(cx);

    assert!(
        cx.update(|_window, app| view.read(app).pending_reviewer_ask.is_none()),
        "leaving the PR tab must drop the pending reviewer ask"
    );

    let _ = std::fs::remove_dir_all(&workdir);
}

/// The reviewer menu is wired into the root's capture phase (like the plain
/// Codex menu and the `?` list), so a key a focused element would otherwise
/// bind as an action — `escape` in the diff, closing it — can't steal it
/// from the menu.
#[gpui::test]
fn escape_closes_the_reviewer_menu_even_with_the_diff_focused(cx: &mut gpui::TestAppContext) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        super::super::super::GitCometView::new(store, events, None, window, cx)
    });
    bind_app_keys_and_global_diff_fallback_for_test(cx);

    let repo_id = RepoId(80171);
    let workdir = std::env::temp_dir().join(format!(
        "gitcomet_ui_test_{}_reviewer_menu_capture",
        std::process::id()
    ));
    open_review_with_detail(cx, &view, repo_id, 49, &workdir, "base1", "head1");
    // `open_review_for_test` is a minimal seam (no real diff load), so the
    // diff panel isn't focused on its own; force it, since the point of this
    // test is that the diff having focus can't steal `escape` from the menu.
    view.update_in(cx, |this, window, cx| {
        let handle = this.main_pane.read(cx).diff_panel_focus_handle.clone();
        window.focus(&handle, cx);
    });
    draw_and_drain_test_window(cx);
    let diff_focused = cx.update(|window, app| {
        view.read(app)
            .main_pane
            .read(app)
            .diff_panel_focus_handle
            .is_focused(window)
    });
    assert!(diff_focused, "the diff must be focused for this test to mean anything");

    cx.simulate_keystrokes("i");
    draw_and_drain_test_window(cx);
    assert!(cx.update(|_window, app| view.read(app).reviewer_menu.is_some()));

    cx.simulate_keystrokes("escape");
    draw_and_drain_test_window(cx);
    let (reviewer_open, diff_still_focused) = cx.update(|window, app| {
        let this = view.read(app);
        (
            this.reviewer_menu.is_some(),
            this.main_pane
                .read(app)
                .diff_panel_focus_handle
                .is_focused(window),
        )
    });
    assert!(!reviewer_open, "escape must close the reviewer menu");
    assert!(
        diff_still_focused,
        "escape must be consumed by the menu in capture, before it ever reaches whatever the \
         focused diff element would have bound it to"
    );

    let _ = std::fs::remove_dir_all(&workdir);
}

/// With no GitHub remote there is no trusted commit to read `.reviewer/`
/// from, so the load settles as `Disabled` (reported in the menu) rather than
/// `Ready` or "still loading". Called directly: test builds never run the load
/// in the background.
#[test]
fn reviewer_load_without_a_github_remote_is_disabled() {
    let workdir = std::env::temp_dir().join(format!(
        "gitcomet_ui_test_{}_reviewer_menu_builtin",
        std::process::id()
    ));
    let load = crate::view::reviewer_menu::reviewer_load(&workdir, None, "base1");
    assert!(
        matches!(load, crate::view::reviewer_menu::ReviewerLoad::Disabled(_)),
        "no GitHub remote means .reviewer can't be trusted from anywhere"
    );
}

/// `enter` on a Brief/Review row must refuse to jump once the row's own
/// pull request or head no longer matches what's on screen, rather than
/// landing in whatever review happens to be open (AGENTS.md LOW finding).
/// Real dispatch never lands `findings` in tests (see the module doc), so
/// this seeds a finished run directly.
#[gpui::test]
fn codex_row_jump_refuses_a_stale_pull_request_or_head(cx: &mut gpui::TestAppContext) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        super::super::super::GitCometView::new(store, events, None, window, cx)
    });
    bind_app_keys_and_global_diff_fallback_for_test(cx);

    let repo_id = RepoId(80177);
    let workdir = std::env::temp_dir().join(format!(
        "gitcomet_ui_test_{}_codex_row_jump",
        std::process::id()
    ));
    open_review_with_detail(cx, &view, repo_id, 55, &workdir, "base1", "head1");

    let finding = crate::reviewer::RuleFinding {
        path: "a.rs".to_string(),
        line: 3,
        side: "RIGHT".to_string(),
        body: "Mind this.".to_string(),
    };
    let pending_jump = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_window, app| {
            view.read(app)
                .active_review()
                .and_then(|review| review.pending_jump)
        })
    };

    // Dispatched for a different pull request: `enter` refuses, no jump.
    cx.update(|window, app| {
        view.update(app, |this, cx| {
            this.seed_codex_run_findings_for_test(
                repo_id,
                vec![finding.clone()],
                Some((999, "head1".to_string())),
                window,
                cx,
            );
        });
    });
    draw_and_drain_test_window(cx);
    cx.update(|window, app| {
        view.update(app, |this, cx| this.handle_codex_panel_key("enter", window, cx))
    });
    draw_and_drain_test_window(cx);
    assert_eq!(pending_jump(cx), None, "a stale pull request must not jump");
    let toasts = cx.update(|_window, app| view.read(app).toast_host.read(app).toasts_for_tests(app));
    assert!(
        toasts.iter().any(|(_, message)| message.contains("different pull request")),
        "expected a refusal toast, got {toasts:?}"
    );

    // The matching pull request and head: `enter` jumps.
    cx.update(|window, app| {
        view.update(app, |this, cx| {
            this.seed_codex_run_findings_for_test(
                repo_id,
                vec![finding],
                Some((55, "head1".to_string())),
                window,
                cx,
            );
        });
    });
    draw_and_drain_test_window(cx);
    cx.update(|window, app| {
        view.update(app, |this, cx| this.handle_codex_panel_key("enter", window, cx))
    });
    draw_and_drain_test_window(cx);
    assert_eq!(pending_jump(cx), Some((crate::github::ReviewSide::Right, 3)));

    let _ = std::fs::remove_dir_all(&workdir);
}
