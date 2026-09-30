use super::*;
use crate::view::panel_focus::FocusPanel::{self, *};

const REPO: RepoId = RepoId(733);
type View = gpui::Entity<crate::view::GitCometView>;

/// Two modified files and two branches, with nothing selected and no diff open.
fn panel_repo() -> RepoState {
    let commit = CommitId("7337337337337337".into());
    let mut repo = simple_worktree_repo(
        REPO,
        Path::new("/tmp/panel-focus"),
        &commit,
        &["a.rs".into(), "b.rs".into()],
        Path::new("a.rs"),
    );
    repo.diff_state.diff_target = None;
    repo.diff_state.diff = Loadable::NotLoaded;
    let file = |path: &str| gitcomet_core::domain::FileStatus {
        path: path.into(),
        kind: FileStatusKind::Modified,
        conflict: None,
    };
    repo.worktree_status = Loadable::Ready(Arc::new(vec![file("a.rs"), file("b.rs")]));
    repo.staged_status = Loadable::Ready(Arc::new(vec![]));
    repo.worktree_status_rev = 1;
    repo.staged_status_rev = 1;
    let branch = |name: &str| gitcomet_core::domain::Branch {
        name: name.into(),
        target: commit.clone(),
        upstream: None,
        divergence: None,
    };
    repo.branches = Loadable::Ready(Arc::new(vec![branch("main"), branch("feature")]));
    repo.branches_rev = 1;
    repo.remote_branches = Loadable::Ready(Arc::new(vec![]));
    repo.remote_branches_rev = 1;
    repo
}

fn fixture(cx: &mut gpui::TestAppContext) -> (View, &mut gpui::VisualTestContext) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        crate::view::GitCometView::new(store, events, None, window, cx)
    });
    apply_state(cx, &view, app_state_with_active_repo(panel_repo()));
    bind_app_keys_and_global_diff_fallback_for_test(cx);
    cx.update(|_window, app| crate::app::bind_text_input_keys_for_test(app));
    draw_and_drain_test_window(cx);
    (view, cx)
}

fn press(cx: &mut gpui::VisualTestContext, keys: &str) {
    cx.simulate_keystrokes(keys);
    draw_and_drain_test_window(cx);
}

/// A full press and release: a focused button clicks on the key-up, which
/// `simulate_keystrokes` never sends.
fn tap(cx: &mut gpui::VisualTestContext, key: &str) {
    cx.simulate_keystrokes(key);
    cx.update(|window, app| {
        window.dispatch_event(
            gpui::PlatformInput::KeyUp(gpui::KeyUpEvent {
                keystroke: gpui::Keystroke::parse(key).expect("valid key"),
            }),
            app,
        );
    });
    draw_and_drain_test_window(cx);
}

fn focused(cx: &mut gpui::VisualTestContext, view: &View) -> Option<FocusPanel> {
    cx.update(|window, app| view.read(app).focused_panel(window, app))
}

/// The open diff as the main pane sees it, after pushing the store's latest
/// snapshot to the view (tests have no poller doing that on its own).
fn diff_path(cx: &mut gpui::VisualTestContext, view: &View) -> Option<std::path::PathBuf> {
    sync_store_snapshot(cx, view);
    cx.update(|_window, app| {
        match view.read(app).main_pane.read(app).state.repos[0]
            .diff_state
            .diff_target
            .as_ref()
        {
            Some(DiffTarget::WorkingTree { path, .. }) => Some(path.clone()),
            _ => None,
        }
    })
}

fn keys_help(cx: &mut gpui::VisualTestContext, view: &View) -> Option<FocusPanel> {
    cx.update(|_window, app| view.read(app).keys_help_panel)
}

/// The `?` list adds a plain-text symbol legend of its own under the pull
/// requests tab, rather than more `key_help` rows (those render through
/// `shortcut_keys`, which turns any left-column text into keycap chips, and
/// "PR shape" / "Review" / "Checks" / "Kind" aren't keys).
#[gpui::test]
fn pull_request_legend_opens_from_keyboard(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    apply_state(cx, &view, pull_request_state());
    press(cx, "1 ?");
    assert_eq!(keys_help(cx, &view), Some(Sidebar));
    assert!(
        cx.debug_bounds("legend_review_required").is_some(),
        "the PR symbol legend should render under the pull requests tab"
    );
    press(cx, "escape");
    assert_eq!(keys_help(cx, &view), None);

    // Not on the branches tab: nothing there uses these symbols.
    apply_state(cx, &view, Arc::new(AppState::test_default()));
    press(cx, "1 ?");
    assert!(
        cx.debug_bounds("legend_review_required").is_none(),
        "the PR symbol legend should not render outside the pull requests tab"
    );
    press(cx, "escape");
}

fn selected_branch(
    cx: &mut gpui::VisualTestContext,
    view: &View,
) -> Option<crate::view::branch_sidebar::BranchMenuTarget> {
    cx.update(|_window, app| {
        view.read(app)
            .sidebar_pane
            .read(app)
            .selected_branch()
            .map(|selected| selected.target.clone())
    })
}

#[gpui::test]
fn number_keys_focus_panels_and_open_collapsed_ones(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    assert_eq!(focused(cx, &view), None);

    for (key, panel) in [("1", Sidebar), ("2", History), ("4", Details)] {
        press(cx, key);
        assert_eq!(focused(cx, &view), Some(panel), "after {key}");
    }
    // 3 is Details as well: its number now that History and the diff share 2.
    press(cx, "2 3");
    assert_eq!(focused(cx, &view), Some(Details));

    cx.update(|_window, app| view.update(app, |this, cx| this.set_sidebar_collapsed(true, cx)));
    draw_and_drain_test_window(cx);
    press(cx, "1");
    assert_eq!(focused(cx, &view), Some(Sidebar));
    assert!(cx.update(|_window, app| !view.read(app).sidebar_collapsed));
}

#[gpui::test]
fn h_and_l_step_between_visible_panels_without_wrapping(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    press(cx, "2");
    // With History showing, Diff is not on screen and gets skipped.
    for (key, panel) in [
        ("l", Details),
        ("l", Details),
        ("h", History),
        ("left", Sidebar),
        ("h", Sidebar),
        ("right", History),
    ] {
        press(cx, key);
        assert_eq!(focused(cx, &view), Some(panel), "after {key}");
    }

    cx.update(|_window, app| view.update(app, |this, cx| this.set_details_collapsed(true, cx)));
    draw_and_drain_test_window(cx);
    press(cx, "l");
    assert_eq!(focused(cx, &view), Some(History));
}

#[gpui::test]
fn panel_keys_are_inert_while_typing(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    let input = cx.update(|window, app| {
        let input = view
            .read(app)
            .details_pane
            .read(app)
            .commit_message_input
            .clone();
        let handle = input.read(app).focus_handle();
        window.focus(&handle, app);
        input
    });
    draw_and_drain_test_window(cx);

    press(cx, "1 h l j");
    assert_eq!(focused(cx, &view), None);
    assert_eq!(
        cx.update(|_window, app| input.read(app).text().to_string()),
        "1hlj"
    );
}

#[gpui::test]
fn details_keys_open_files_and_escape_returns_focus(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    press(cx, "4");

    // j with nothing selected starts at the first file; focus stays in Details.
    press(cx, "j");
    wait_until(cx, "first file to open", |cx| {
        diff_path(cx, &view).as_deref() == Some(Path::new("a.rs"))
    });
    assert_eq!(focused(cx, &view), Some(Details));
    press(cx, "j");
    wait_until(cx, "next file to open", |cx| {
        diff_path(cx, &view).as_deref() == Some(Path::new("b.rs"))
    });

    press(cx, "enter");
    assert_eq!(focused(cx, &view), Some(Diff));

    // The `?` list is modal and sees keys first: its esc must not close the diff.
    press(cx, "?");
    assert_eq!(keys_help(cx, &view), Some(Diff));
    press(cx, "j escape");
    assert_eq!(keys_help(cx, &view), None);
    assert_eq!(diff_path(cx, &view).as_deref(), Some(Path::new("b.rs")));
    assert_eq!(focused(cx, &view), Some(Diff));

    press(cx, "escape");
    wait_until(cx, "diff to close and focus to return", |cx| {
        diff_path(cx, &view).is_none() && focused(cx, &view) == Some(Details)
    });
}

#[gpui::test]
fn sidebar_j_and_k_step_through_branches(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    press(cx, "1");
    assert_eq!(selected_branch(cx, &view), None);

    press(cx, "j");
    let first = selected_branch(cx, &view).expect("j selects the first branch");
    press(cx, "j");
    let second = selected_branch(cx, &view).expect("j moves to the next branch");
    assert_ne!(first, second);
    press(cx, "k");
    assert_eq!(selected_branch(cx, &view), Some(first));
    assert_eq!(focused(cx, &view), Some(Sidebar));
}

fn pull_request_state() -> Arc<AppState> {
    let mut repo = panel_repo();
    repo.remotes = Loadable::Ready(Arc::new(vec![gitcomet_core::domain::Remote {
        name: "origin".into(),
        url: Some("https://github.com/owner/repo.git".into()),
    }]));
    repo.remotes_rev = 1;
    Arc::new(AppState {
        repos: vec![repo],
        active_repo: Some(REPO),
        sidebar_mode: gitcomet_state::model::SidebarMode::PullRequests,
        ..AppState::test_default()
    })
}

fn popover_open(cx: &mut gpui::VisualTestContext, view: &View, kind: &PopoverKind) -> bool {
    cx.update(|_window, app| view.read(app).popover_host.read(app).is_kind_open(kind))
}

#[gpui::test]
fn pull_request_dialogs_open_from_keys_and_hand_focus_back(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    // Seeded before the tab shows, so it never runs gh.
    cx.update(|_window, app| {
        view.update(app, |this, _| {
            this.seed_pull_requests_for_test(
                REPO,
                vec![crate::github::PullRequestSummary {
                    number: 7,
                    title: "Keyboard nav".into(),
                    author: "someone".into(),
                    head: "feat".into(),
                    head_owner: "someone".into(),
                    base: "main".into(),
                    is_draft: false,
                    is_cross_repository: false,
                    review: None,
                    checks: Default::default(),
                    review_requested: false,
                    is_mine: false,
                }],
                Some(7),
            );
        })
    });
    apply_state(cx, &view, pull_request_state());
    press(cx, "1");
    assert_eq!(focused(cx, &view), Some(Sidebar));

    let review = PopoverKind::PullRequestReview {
        repo_id: REPO,
        number: 7,
        kind: crate::github::ReviewKind::Comment,
    };
    press(cx, "shift-s");
    assert!(popover_open(cx, &view, &review));
    // An empty comment can't post, so this is a no-op rather than a gh call.
    press(cx, "secondary-enter");
    assert!(popover_open(cx, &view, &review));
    press(cx, "escape");
    assert!(!popover_is_open(cx, &view));
    assert_eq!(focused(cx, &view), Some(Sidebar));

    press(cx, "n");
    assert!(popover_open(
        cx,
        &view,
        &PopoverKind::CreatePullRequest {
            repo_id: REPO,
            branch: None,
        }
    ));
    press(cx, "escape");
    assert_eq!(focused(cx, &view), Some(Sidebar));

    // Merging opens a confirm dialog whose method switches on Alt chords;
    // nothing reaches gh until Enter.
    let merge = |method| PopoverKind::MergePullRequest {
        repo_id: REPO,
        number: 7,
        method,
    };
    press(cx, "shift-m");
    assert!(popover_open(
        cx,
        &view,
        &merge(crate::github::MergeMethod::Merge)
    ));
    press(cx, "alt-s");
    assert!(popover_open(
        cx,
        &view,
        &merge(crate::github::MergeMethod::Squash)
    ));
    let submit_error = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_window, app| {
            view.read(app)
                .active_pull_requests()
                .and_then(|prs| prs.submit_error.clone())
        })
    };
    // Space is the checkout key, not a second way to merge.
    tap(cx, "space");
    assert_eq!(submit_error(cx), None);
    // Enter merges, but only onto a head the details have shown; they never
    // loaded here, so it stops before gh.
    tap(cx, "enter");
    assert!(submit_error(cx).is_some_and(|error| error.contains("still loading")));
    press(cx, "escape");
    assert!(!popover_is_open(cx, &view));
    assert_eq!(focused(cx, &view), Some(Sidebar));
}

fn stack_pull_request(number: u64, head: &str, base: &str) -> crate::github::PullRequestSummary {
    crate::github::PullRequestSummary {
        number,
        title: format!("pr {number}"),
        author: "someone".into(),
        head: head.into(),
        head_owner: "owner".into(),
        base: base.into(),
        is_draft: false,
        is_cross_repository: false,
        review: None,
        checks: Default::default(),
        review_requested: false,
        is_mine: false,
    }
}

#[gpui::test]
fn less_and_greater_walk_a_pull_requests_stack(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    let selected = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_window, app| {
            view.read(app)
                .active_pull_requests()
                .and_then(|prs| prs.selected)
        })
    };
    cx.update(|_window, app| {
        view.update(app, |this, _| {
            this.seed_pull_requests_for_test(
                REPO,
                vec![
                    stack_pull_request(1, "feat-a", "main"),
                    stack_pull_request(2, "feat-b", "feat-a"),
                    stack_pull_request(3, "feat-c", "feat-b"),
                ],
                Some(2),
            );
            this.pull_requests.repo_mut(REPO).stacks = vec![crate::github::PullRequestStack {
                members: vec![1, 2, 3],
                native: false,
            }];
        })
    });
    apply_state(cx, &view, pull_request_state());
    press(cx, "1");
    assert_eq!(focused(cx, &view), Some(Sidebar));
    assert_eq!(selected(cx), Some(2));

    press(cx, "shift-,");
    assert_eq!(selected(cx), Some(1));
    // The bottom of the stack: `<` again is a no-op.
    press(cx, "shift-,");
    assert_eq!(selected(cx), Some(1));

    press(cx, "shift-.");
    assert_eq!(selected(cx), Some(2));
    press(cx, "shift-.");
    assert_eq!(selected(cx), Some(3));
    // The top of the stack: `>` again is a no-op.
    press(cx, "shift-.");
    assert_eq!(selected(cx), Some(3));

    // Panel 2 (the PR's own conversation view): the same keys work there.
    press(cx, "enter");
    assert_eq!(focused(cx, &view), Some(History));
    press(cx, "shift-,");
    assert_eq!(selected(cx), Some(2));
    press(cx, "shift-,");
    assert_eq!(selected(cx), Some(1));

    // Details: same again, and the literal `<`/`>` (ISO/DE layouts) work too.
    press(cx, "3");
    assert_eq!(focused(cx, &view), Some(Details));
    press(cx, "shift-.");
    assert_eq!(selected(cx), Some(2));
    press(cx, ">");
    assert_eq!(selected(cx), Some(3));
    press(cx, "<");
    assert_eq!(selected(cx), Some(2));

    // Inert while typing: focus a text input and confirm the stack
    // selection never moves while it holds focus.
    cx.update(|window, app| {
        let input = view
            .read(app)
            .details_pane
            .read(app)
            .commit_message_input
            .clone();
        let handle = input.read(app).focus_handle();
        window.focus(&handle, app);
    });
    draw_and_drain_test_window(cx);
    press(cx, "shift-,");
    assert_eq!(focused(cx, &view), None);
    assert_eq!(selected(cx), Some(2));
    press(cx, ">");
    assert_eq!(focused(cx, &view), None);
    assert_eq!(selected(cx), Some(2));
}

/// Reviewing PR 1 in the stack 1<-2: `<`/`>` must not swap in a neighboring
/// PR out from under the open review (it would silently diff/list the wrong
/// PR — see AGENTS.md HIGH finding on `handle_pull_request_key`).
#[gpui::test]
fn stack_keys_are_inert_while_reviewing(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            this.seed_pull_requests_for_test(
                REPO,
                vec![
                    stack_pull_request(1, "feat-a", "main"),
                    stack_pull_request(2, "feat-b", "feat-a"),
                ],
                Some(1),
            );
            this.pull_requests.repo_mut(REPO).stacks = vec![crate::github::PullRequestStack {
                members: vec![1, 2],
                native: false,
            }];
            this.open_review_for_test(REPO, 1, vec!["a.rs".to_string()], "head1", cx);
        })
    });
    apply_state(cx, &view, pull_request_state());
    let selected = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_window, app| {
            view.read(app)
                .active_pull_requests()
                .and_then(|prs| prs.selected)
        })
    };

    press(cx, "1");
    assert_eq!(focused(cx, &view), Some(Sidebar));
    assert_eq!(selected(cx), Some(1));
    press(cx, ">");
    assert_eq!(selected(cx), Some(1), "sidebar: `>` must not change the review");
    press(cx, "shift-.");
    assert_eq!(selected(cx), Some(1), "sidebar: shift+. must not change the review");

    cx.update(|window, app| {
        view.update(app, |this, cx| {
            this.focus_panel(Details, window, cx);
        });
    });
    draw_and_drain_test_window(cx);
    assert_eq!(focused(cx, &view), Some(Details));
    press(cx, ">");
    assert_eq!(selected(cx), Some(1), "details: `>` must not change the review");
}

fn selected_commit(cx: &mut gpui::VisualTestContext, view: &View) -> Option<CommitId> {
    sync_store_snapshot(cx, view);
    cx.update(|_window, app| {
        view.read(app)
            .active_repo()
            .and_then(|repo| repo.history_state.selected_commit.clone())
    })
}

/// Opens `kind` with `keys`, closes it with escape, and checks focus is back
/// on `panel`.
fn assert_key_opens(
    cx: &mut gpui::VisualTestContext,
    view: &View,
    keys: &str,
    kind: PopoverKind,
    panel: FocusPanel,
) {
    press(cx, keys);
    assert!(popover_open(cx, view, &kind), "{keys} opens {kind:?}");
    press(cx, "escape");
    assert!(
        !popover_is_open(cx, view),
        "escape closes what {keys} opened"
    );
    assert_eq!(focused(cx, view), Some(panel), "focus after {keys}");
}

#[gpui::test]
fn commit_keys_open_their_dialogs_and_hand_focus_back(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    press(cx, "2 j");
    wait_until(cx, "a commit to be selected", |cx| {
        selected_commit(cx, &view).is_some()
    });
    let commit_id = selected_commit(cx, &view).expect("selected");
    let sha = commit_id.as_ref().to_string();
    for (keys, kind) in [
        (
            "t",
            PopoverKind::RevertCommitConfirm {
                repo_id: REPO,
                commit_id: commit_id.clone(),
            },
        ),
        (
            "g",
            PopoverKind::ResetPrompt {
                repo_id: REPO,
                target: sha.clone(),
                mode: gitcomet_core::services::ResetMode::Mixed,
            },
        ),
        (
            "shift-t",
            PopoverKind::CreateTagPrompt {
                repo_id: REPO,
                target: sha.clone(),
            },
        ),
        (
            "m",
            PopoverKind::CommitMenu {
                repo_id: REPO,
                commit_id: commit_id.clone(),
            },
        ),
    ] {
        assert_key_opens(cx, &view, keys, kind, History);
    }
    // The only commit is HEAD, which can't be cherry-picked onto itself.
    press(cx, "shift-c");
    assert!(!popover_is_open(cx, &view));
    assert_eq!(focused(cx, &view), Some(History));
}

/// Focuses the Sidebar on `feature`, which (unlike the checked-out `main`)
/// can be rebased onto, merged or deleted.
fn select_feature_branch(
    cx: &mut gpui::VisualTestContext,
    view: &View,
) -> crate::view::branch_sidebar::BranchMenuTarget {
    press(cx, "1 j");
    let feature = crate::view::branch_sidebar::BranchMenuTarget::Local {
        name: "feature".into(),
    };
    if selected_branch(cx, view).as_ref() != Some(&feature) {
        press(cx, "j");
    }
    assert_eq!(selected_branch(cx, view), Some(feature.clone()));
    feature
}

#[gpui::test]
fn branch_keys_open_their_dialogs_and_hand_focus_back(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    let feature = select_feature_branch(cx, &view);
    for (keys, kind) in [
        (
            "n",
            PopoverKind::CreateBranchFromRefPrompt {
                repo_id: REPO,
                target: "feature".into(),
                source_selectable: false,
                name_prefix: String::new(),
            },
        ),
        (
            "shift-r",
            PopoverKind::RebaseOntoConfirm {
                repo_id: REPO,
                onto: "feature".into(),
            },
        ),
        (
            "m",
            PopoverKind::BranchMenu {
                repo_id: REPO,
                target: feature.clone(),
            },
        ),
    ] {
        assert_key_opens(cx, &view, keys, kind, Sidebar);
    }
}

#[gpui::test]
fn details_keys_act_on_the_open_file_and_c_goes_to_the_commit_message(
    cx: &mut gpui::TestAppContext,
) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    press(cx, "4 j");
    wait_until(cx, "the first file to open", |cx| {
        diff_path(cx, &view).as_deref() == Some(Path::new("a.rs"))
    });
    for (keys, kind) in [
        (
            "d",
            PopoverKind::DiscardChangesConfirm {
                repo_id: REPO,
                area: DiffArea::Unstaged,
                path: Some("a.rs".into()),
            },
        ),
        (
            "m",
            PopoverKind::StatusFileMenu {
                repo_id: REPO,
                area: DiffArea::Unstaged,
                path: "a.rs".into(),
            },
        ),
        ("s", PopoverKind::StashPrompt),
    ] {
        assert_key_opens(cx, &view, keys, kind, Details);
    }

    press(cx, "c");
    let input = cx.update(|_window, app| {
        view.read(app)
            .details_pane
            .read(app)
            .commit_message_input
            .clone()
    });
    assert!(cx.update(|window, app| input.read(app).focus_handle().is_focused(window)));
    // Typing now goes to the message, not to panel keys.
    press(cx, "j k");
    assert_eq!(
        cx.update(|_window, app| input.read(app).text().to_string()),
        "jk"
    );
}

#[gpui::test]
fn deleting_a_branch_takes_a_second_press(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    let feature = select_feature_branch(cx, &view);
    let armed = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_window, app| view.read(app).armed_branch_key.clone())
    };
    press(cx, "shift-d");
    assert_eq!(armed(cx), Some(("D".to_string(), feature)));
    // Any other key stands it down.
    press(cx, "k");
    assert_eq!(armed(cx), None);
}

#[gpui::test]
fn enter_from_details_opens_the_diff_and_escape_comes_back(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    // No diff is open yet: focus moves to it before it first renders.
    press(cx, "4 enter");
    wait_until(cx, "the diff to open with focus", |cx| {
        diff_path(cx, &view).is_some() && focused(cx, &view) == Some(Diff)
    });
    press(cx, "escape");
    wait_until(cx, "focus back on Details", |cx| {
        diff_path(cx, &view).is_none() && focused(cx, &view) == Some(Details)
    });
}

#[gpui::test]
fn shift_o_on_a_branch_sets_up_a_pull_request_from_it(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    let mut repo = panel_repo();
    repo.remotes = Loadable::Ready(Arc::new(vec![gitcomet_core::domain::Remote {
        name: "origin".into(),
        url: Some("https://github.com/owner/repo.git".into()),
    }]));
    repo.remotes_rev = 1;
    apply_state(cx, &view, app_state_with_active_repo(repo));
    let _ = select_feature_branch(cx, &view);
    assert_key_opens(
        cx,
        &view,
        "shift-o",
        PopoverKind::CreatePullRequest {
            repo_id: REPO,
            branch: Some("feature".into()),
        },
        Sidebar,
    );
    // `o` goes straight to GitHub, which needs the branch there; this one was
    // never pushed, so it only says so.
    press(cx, "o");
    assert!(!popover_is_open(cx, &view));
    assert_eq!(focused(cx, &view), Some(Sidebar));
}

#[gpui::test]
fn review_mode_walks_files_keeps_comments_and_leaves_with_q(cx: &mut gpui::TestAppContext) {
    use crate::github::{ReviewAnchor, ReviewSide};

    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    let file = |path: &str| crate::github::PullRequestFile {
        path: path.into(),
        additions: 1,
        deletions: 0,
    };
    cx.update(|_window, app| {
        view.update(app, |this, _| {
            this.seed_pull_requests_for_test(REPO, vec![], Some(7));
            this.seed_pull_request_detail_for_test(
                REPO,
                crate::github::PullRequestDetail {
                    number: 7,
                    title: "Keyboard nav".into(),
                    body: String::new(),
                    body_truncated: false,
                    url: String::new(),
                    author: "someone".into(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                    head: "feat".into(),
                    head_oid: "a".repeat(40),
                    base: "main".into(),
                    base_oid: "b".repeat(40),
                    is_draft: false,
                    is_cross_repository: false,
                    state: "OPEN".into(),
                    review: None,
                    mergeable: None,
                    additions: 2,
                    deletions: 0,
                    changed_files: 2,
                    files: vec![file("a.rs"), file("b.rs")],
                    checks: Default::default(),
                    check_runs: vec![],
                    conversation: vec![],
                    reviewers: vec![],
                    commits: vec![],
                },
                "c".repeat(40),
            );
        })
    });
    apply_state(cx, &view, pull_request_state());
    let review = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_window, app| {
            view.read(app).active_review().map(|review| {
                (
                    review.file_ix,
                    review.draft.viewed.len(),
                    review.draft.comments.len(),
                )
            })
        })
    };

    press(cx, "1 r");
    assert_eq!(review(cx), Some((0, 0, 0)));
    press(cx, "1 ]");
    assert_eq!(review(cx).map(|(file, ..)| file), Some(1));
    // Viewed, then on to the first file still unviewed.
    press(cx, "space");
    assert_eq!(review(cx), Some((0, 1, 0)));

    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            this.add_review_comment(
                REPO,
                7,
                ReviewAnchor {
                    path: "a.rs".into(),
                    side: ReviewSide::Right,
                    line: 1,
                    start: None,
                },
                "Nit".into(),
                None,
                None,
                cx,
            );
        })
    });
    assert_eq!(review(cx).map(|(.., comments)| comments), Some(1));
    // The composer: typing, alt+s for the selected lines as a suggestion,
    // then ctrl+enter lands the comment in the review.
    let b_line_2 = ReviewAnchor {
        path: "b.rs".into(),
        side: ReviewSide::Right,
        line: 2,
        start: None,
    };
    let open = |cx: &mut gpui::VisualTestContext,
                reply_to: Option<crate::github::ReplyTarget>,
                suggestion: Option<String>| {
        let anchor = b_line_2.clone();
        cx.update(|window, app| {
            view.update(app, |this, cx| {
                this.open_review_composer(REPO, 7, anchor, None, reply_to, suggestion, window, cx);
            })
        });
        draw_and_drain_test_window(cx);
    };
    let bodies = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_window, app| {
            view.read(app)
                .active_review()
                .map(|review| {
                    review
                        .draft
                        .comments
                        .iter()
                        .map(|comment| (comment.body.clone(), comment.reply_to.clone()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    };
    open(cx, None, Some("let x = 1;".into()));
    press(cx, "o k alt-s secondary-enter");
    assert!(!popover_is_open(cx, &view));
    assert_eq!(
        bodies(cx)[1],
        (
            "ok
```suggestion
let x = 1;
```"
            .to_string(),
            None
        )
    );
    // A reply to someone's thread waits in the review like any comment.
    let octo = crate::github::ReplyTarget {
        id: 99,
        author: "octo".into(),
    };
    open(cx, Some(octo.clone()), None);
    press(cx, "t y secondary-enter");
    assert_eq!(bodies(cx)[2], ("ty".to_string(), Some(octo)));
    // Your review: the second d deletes, a single one only asks.
    press(cx, "4 j d");
    assert_eq!(bodies(cx).len(), 3);
    press(cx, "d");
    assert_eq!(bodies(cx).len(), 2);

    // S is the submit dialog for this pull request.
    press(cx, "shift-s");
    assert!(popover_open(
        cx,
        &view,
        &PopoverKind::PullRequestReview {
            repo_id: REPO,
            number: 7,
            kind: crate::github::ReviewKind::Comment,
        }
    ));
    press(cx, "escape");

    press(cx, "q");
    assert_eq!(review(cx), None);
    assert_eq!(focused(cx, &view), Some(Sidebar));
}

/// Pull request #7 with a.rs, b.rs and c.rs, its commits as good as local.
fn seed_three_file_pull_request(cx: &mut gpui::VisualTestContext, view: &View) {
    let file = |path: &str| crate::github::PullRequestFile {
        path: path.into(),
        additions: 1,
        deletions: 0,
    };
    cx.update(|_window, app| {
        view.update(app, |this, _| {
            this.seed_pull_requests_for_test(REPO, vec![], Some(7));
            this.seed_pull_request_detail_for_test(
                REPO,
                crate::github::PullRequestDetail {
                    number: 7,
                    title: "Keyboard nav".into(),
                    body: String::new(),
                    body_truncated: false,
                    url: String::new(),
                    author: "someone".into(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                    head: "feat".into(),
                    head_oid: "a".repeat(40),
                    base: "main".into(),
                    base_oid: "b".repeat(40),
                    is_draft: false,
                    is_cross_repository: false,
                    state: "OPEN".into(),
                    review: None,
                    mergeable: None,
                    additions: 3,
                    deletions: 0,
                    changed_files: 3,
                    files: vec![file("a.rs"), file("b.rs"), file("c.rs")],
                    checks: Default::default(),
                    check_runs: vec![],
                    conversation: vec![],
                    reviewers: vec![],
                    commits: vec![
                        crate::github::PullRequestCommit {
                            oid: "a".repeat(40),
                            headline: "Third".into(),
                            committed_at: "2026-01-03T00:00:00Z".into(),
                            author: None,
                        },
                        crate::github::PullRequestCommit {
                            oid: "d".repeat(40),
                            headline: "Second".into(),
                            committed_at: "2026-01-02T00:00:00Z".into(),
                            author: None,
                        },
                        crate::github::PullRequestCommit {
                            oid: "e".repeat(40),
                            headline: "First".into(),
                            committed_at: "2026-01-01T00:00:00Z".into(),
                            author: None,
                        },
                    ],
                },
                "c".repeat(40),
            );
        })
    });
    apply_state(cx, view, pull_request_state());
}

#[gpui::test]
fn pull_request_enter_focuses_the_conversation_panel(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    seed_three_file_pull_request(cx, &view);

    press(cx, "1 enter");
    assert_eq!(focused(cx, &view), Some(History));
    press(cx, "1 [");
    sync_store_snapshot(cx, &view);
    assert!(!cx.update(|_window, app| view.read(app).pull_request_content_active()));
    assert_eq!(
        cx.update(|_window, app| view.read(app).main_pane.read(app).state.sidebar_mode),
        gitcomet_state::model::SidebarMode::Files
    );
    press(cx, "4 j");
    wait_until(cx, "normal diff to open", |cx| {
        diff_path(cx, &view).is_some()
    });
    press(cx, "1 ]");
    sync_store_snapshot(cx, &view);
    wait_until(cx, "PR tab to clear the old diff", |cx| {
        diff_path(cx, &view).is_none()
    });
    assert_eq!(
        cx.update(|_window, app| view.read(app).main_pane.read(app).state.sidebar_mode),
        gitcomet_state::model::SidebarMode::PullRequests
    );
    press(cx, "1 [");
    sync_store_snapshot(cx, &view);
    press(cx, "2");
    assert_eq!(focused(cx, &view), Some(History));
    assert!(!cx.update(|_window, app| view.read(app).pull_request_content_active()));
    assert!(diff_path(cx, &view).is_none());
}

#[gpui::test]
fn leaving_review_clears_its_diff_before_history_returns(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    seed_three_file_pull_request(cx, &view);

    press(cx, "1 r");
    sync_store_snapshot(cx, &view);
    assert!(cx.update(|_window, app| {
        view.read(app).store.snapshot().repos[0]
            .diff_state
            .diff_target
            .is_some()
    }));
    press(cx, "q");
    sync_store_snapshot(cx, &view);
    assert!(cx.update(|_window, app| {
        view.read(app).store.snapshot().repos[0]
            .diff_state
            .diff_target
            .is_none()
    }));
    press(cx, "1 [");
    sync_store_snapshot(cx, &view);
    press(cx, "2");
    assert_eq!(focused(cx, &view), Some(History));
    assert!(!cx.update(|_window, app| view.read(app).diff_is_open()));
}

#[gpui::test]
fn review_reopens_its_file_when_the_merge_base_arrives_on_another_tab(
    cx: &mut gpui::TestAppContext,
) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    seed_three_file_pull_request(cx, &view);

    press(cx, "1 r");
    cx.update(|_window, app| {
        view.update(app, |this, _| {
            this.pull_requests.repo_mut(REPO).diff_base =
                crate::view::pull_requests::PrLoad::Loading;
            this.store
                .dispatch(Msg::ClearDiffSelection { repo_id: REPO });
        })
    });
    cx.update(|_window, app| {
        view.read(app).store.dispatch(Msg::SetSidebarMode {
            mode: gitcomet_state::model::SidebarMode::Files,
        });
    });
    sync_store_snapshot(cx, &view);
    draw_and_drain_test_window(cx);
    cx.update(|_window, app| {
        view.update(app, |this, _| {
            this.pull_requests.repo_mut(REPO).diff_base =
                crate::view::pull_requests::PrLoad::Ready("c".repeat(40));
        })
    });
    sync_store_snapshot(cx, &view);
    assert!(cx.update(|_window, app| {
        view.read(app).store.snapshot().repos[0]
            .diff_state
            .diff_target
            .is_none()
    }));

    press(cx, "1 ]");
    wait_until(cx, "review diff to reopen", |cx| {
        sync_store_snapshot(cx, &view);
        cx.update(|_window, app| {
            matches!(
                view.read(app).store.snapshot().repos[0].diff_state.diff_target.as_ref(),
                Some(DiffTarget::CommitRange {
                    from_commit_id,
                    to_commit_id: Some(head),
                    path: Some(path),
                }) if from_commit_id.as_ref() == "c".repeat(40)
                    && head.as_ref() == "a".repeat(40)
                    && path == Path::new("a.rs")
            )
        })
    });
}

#[gpui::test]
fn pull_request_details_keys_keep_the_hidden_diff_closed(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    seed_three_file_pull_request(cx, &view);

    press(cx, "3 j k shift-j shift-k");
    sync_store_snapshot(cx, &view);
    assert_eq!(focused(cx, &view), Some(Details));
    assert!(cx.update(|_window, app| {
        view.read(app).store.snapshot().repos[0]
            .diff_state
            .diff_target
            .is_none()
    }));
    assert!(cx.update(|_window, app| {
        view.read(app)
            .key_hints(Details)
            .iter()
            .any(|(key, _)| *key == "enter")
    }));
    assert!(cx.update(|_window, app| {
        view.read(app)
            .key_help(Details)
            .iter()
            .any(|(key, _)| *key == "enter")
    }));
    press(cx, "enter");
    assert_eq!(focused(cx, &view), Some(History));
}

#[gpui::test]
fn pr_commit_keys_and_picker_apply_scoped_review_diffs(cx: &mut gpui::TestAppContext) {
    use crate::view::panes::main::ReviewCommentScope;
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    seed_three_file_pull_request(cx, &view);
    let selection = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_, app| {
            view.read(app)
                .active_pull_requests()
                .unwrap()
                .commit_selection
                .range(3)
        })
    };
    press(cx, "3 j shift-j");
    assert_eq!(selection(cx), Some((0, 1)));
    press(cx, "escape");
    assert_eq!(selection(cx), None);
    press(cx, "j j shift-j r");
    assert!(cx.update(|_, app| {
        view.read(app)
            .active_review()
            .unwrap()
            .commit_range
            .is_some()
    }));
    cx.update(|_, app| {
        view.update(app, |this, cx| {
            this.seed_commit_range_for_test(
                "f".repeat(40),
                ["b.rs".to_string(), "old.rs".to_string()].into(),
                cx,
            )
        })
    });
    sync_store_snapshot(cx, &view);
    assert!(cx.update(|_, app| {
        let root = view.read(app);
        let review = root.active_review().unwrap();
        assert_eq!(review.file_ix, 0);
        assert_eq!(review.files, vec!["b.rs", "old.rs"]);
        assert_eq!(
            (0..review.files.len())
                .filter(|ix| review.file_listed(*ix))
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_eq!(
            root.main_pane.read(app).review_comment_scope,
            ReviewCommentScope::Historical
        );
        matches!(root.store.snapshot().repos[0].diff_state.diff_target.as_ref(),
            Some(DiffTarget::CommitRange { from_commit_id, to_commit_id: Some(head), .. })
                if from_commit_id.as_ref() == "f".repeat(40) && head.as_ref() == "d".repeat(40))
    }));
    press(cx, "1 j");
    sync_store_snapshot(cx, &view);
    assert!(cx.update(|_, app| matches!(
        view.read(app).store.snapshot().repos[0].diff_state.diff_target.as_ref(),
        Some(DiffTarget::CommitRange { path: Some(path), .. }) if path == Path::new("old.rs")
    )));
    cx.update(|window, app| {
        view.update(app, |root, app| root.focus_panel(Diff, window, app));
    });
    assert_eq!(focused(cx, &view), Some(Diff));
    press(cx, "shift-c");
    assert!(cx.update(|_, app| {
        let picker = view.read(app).commit_scope_picker.unwrap();
        picker.cursor == 4 && picker.selection.range(3) == Some((1, 2))
    }));
    press(cx, "enter");
    assert!(cx.update(|_, app| {
        let root = view.read(app);
        let review = root.active_review().unwrap();
        root.commit_scope_picker.is_none()
            && review.file_ix == 1
            && review
                .commit_range
                .as_ref()
                .is_some_and(|range| range.changes.ready().is_some())
    }));
    press(cx, "shift-c");
    press(cx, "shift-k");
    assert_eq!(
        cx.update(|_, app| view
            .read(app)
            .commit_scope_picker
            .unwrap()
            .selection
            .range(3)),
        Some((1, 1))
    );
    press(cx, "shift-j");
    assert_eq!(
        cx.update(|_, app| view
            .read(app)
            .commit_scope_picker
            .unwrap()
            .selection
            .range(3)),
        Some((1, 2))
    );
    press(cx, "escape shift-c k k enter");
    assert!(cx.update(|_, app| {
        view.read(app)
            .active_review()
            .unwrap()
            .commit_range
            .is_some()
    }));
    cx.update(|_, app| {
        view.update(app, |this, cx| {
            this.seed_commit_range_for_test("g".repeat(40), ["a.rs".to_string()].into(), cx)
        })
    });
    assert_eq!(
        cx.update(|_, app| view
            .read(app)
            .main_pane
            .read(app)
            .review_comment_scope
            .clone()),
        ReviewCommentScope::Range(crate::view::panes::main::SinceLines::Loading)
    );
    press(cx, "shift-c escape");
    assert!(cx.update(|_, app| view.read(app).commit_scope_picker.is_none()));
    press(cx, "shift-c k k k enter");
    assert!(cx.update(|_, app| {
        view.read(app)
            .active_review()
            .unwrap()
            .commit_range
            .is_none()
    }));
}

#[gpui::test]
fn shift_l_keeps_the_review_to_files_changed_since_your_last_review(cx: &mut gpui::TestAppContext) {
    use crate::view::panes::main::{ReviewCommentScope, SinceLines};
    use crate::view::review::SinceReview;

    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    seed_three_file_pull_request(cx, &view);
    let diff_base = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_window, app| view.read(app).review_diff_base(REPO, 7))
    };
    // The diff actually asked for: where it starts, and whether the review
    // takes it as its own (its keys work only then).
    let shown = |cx: &mut gpui::VisualTestContext| {
        sync_store_snapshot(cx, &view);
        cx.update(|_window, app| {
            let this = view.read(app);
            let from = match this.main_pane.read(app).state.repos[0]
                .diff_state
                .diff_target
                .as_ref()
            {
                Some(DiffTarget::CommitRange { from_commit_id, .. }) => {
                    Some(from_commit_id.as_ref().to_string())
                }
                _ => None,
            };
            (from, this.review_diff_shown())
        })
    };
    let scope = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_window, app| {
            view.read(app)
                .main_pane
                .read(app)
                .review_comment_scope
                .clone()
        })
    };
    let review = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_window, app| {
            view.read(app).active_review().map(|review| {
                (
                    review.file_ix,
                    review.only_changed,
                    review.draft.viewed.len(),
                )
            })
        })
    };
    let seed = |cx: &mut gpui::VisualTestContext,
                last: Option<crate::github::LastReview>,
                since: Option<SinceReview>| {
        cx.update(|_window, app| {
            view.update(app, |this, cx| {
                this.seed_last_review_for_test(last, since, cx)
            })
        });
    };

    press(cx, "1 r");
    assert_eq!(review(cx), Some((0, false, 0)));
    // Never reviewed: nothing to narrow to.
    seed(cx, None, None);
    press(cx, "shift-l");
    assert_eq!(review(cx), Some((0, false, 0)));

    seed(
        cx,
        Some(crate::github::LastReview {
            state: "CHANGES_REQUESTED".into(),
            body: "Please split this.".into(),
            submitted_at: "2026-01-01T00:00:00Z".into(),
            commit_id: "d".repeat(40),
        }),
        Some(SinceReview::Changed(crate::github::ChangesSince {
            files: ["b.rs".to_string(), "c.rs".to_string()].into(),
            commits: 2,
        })),
    );
    let line = cx.update(|_window, app| {
        let root = view.read(app);
        let review = root.active_review()?;
        let last = root.active_pull_requests()?.last_review.ready()?.as_ref()?;
        Some(review.last_review_line(last, std::time::SystemTime::now()))
    });
    assert!(
        line.as_deref().is_some_and(
            |line| line.starts_with("Your last review: Changes requested")
                && line.ends_with("at ddddddd · 2 commits since · 2 files changed since")
        ),
        "{line:?}"
    );
    assert_eq!(diff_base(cx), Some("c".repeat(40)));
    // On: a.rs didn't change, so the review moves to b.rs, and the diff
    // starts at the last review's commit.
    press(cx, "shift-l");
    assert_eq!(review(cx), Some((1, true, 0)));
    assert_eq!(diff_base(cx), Some("d".repeat(40)));
    assert_eq!(shown(cx), (Some("d".repeat(40)), true));
    // Comments wait for b.rs's lines in the pull request's own diff.
    assert_eq!(scope(cx), ReviewCommentScope::Since(SinceLines::Loading));
    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            this.seed_review_threads_for_test(Vec::new(), vec![("b.rs".into(), vec![(1, 5)])], cx)
        })
    });
    assert_eq!(
        scope(cx),
        ReviewCommentScope::Since(SinceLines::Ranges(vec![(1, 5)]))
    );
    // The Sidebar's j/k and ]/[ walk only b.rs and c.rs.
    press(cx, "1 j");
    assert_eq!(review(cx).map(|(file, ..)| file), Some(2));
    press(cx, "j");
    assert_eq!(review(cx).map(|(file, ..)| file), Some(2));
    press(cx, "[ [");
    assert_eq!(review(cx).map(|(file, ..)| file), Some(1));
    // Viewed, then on to the next unviewed file among them.
    press(cx, "space");
    assert_eq!(review(cx), Some((2, true, 1)));
    // Off: every file again, and the whole pull request's diff.
    press(cx, "shift-l");
    assert_eq!(diff_base(cx), Some("c".repeat(40)));
    assert_eq!(shown(cx), (Some("c".repeat(40)), true));
    assert_eq!(scope(cx), ReviewCommentScope::Full);
    press(cx, "k k");
    assert_eq!(review(cx), Some((0, false, 1)));

    press(cx, "q");
    assert_eq!(review(cx), None);
}

#[gpui::test]
fn viewed_marks_follow_github_and_outdated_threads_take_replies(cx: &mut gpui::TestAppContext) {
    use crate::github::{
        PrError, ReviewSide, ReviewThread, ThreadComment, ViewedState, ViewedStates,
    };
    use crate::view::review::ViewedSync;

    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    seed_three_file_pull_request(cx, &view);
    let marks = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_window, app| {
            view.read(app).active_review().map(|review| {
                (
                    review.draft.viewed.iter().cloned().collect::<Vec<String>>(),
                    review.dismissed.iter().cloned().collect::<Vec<String>>(),
                    review.viewed_sync.clone(),
                )
            })
        })
    };
    let seed_states =
        |cx: &mut gpui::VisualTestContext, seq: u64, result: Result<ViewedStates, PrError>| {
            cx.update(|_window, app| {
                view.update(app, |this, cx| {
                    this.seed_viewed_states_for_test(seq, result, cx)
                })
            });
        };
    // The newest load's number, and the presses GitHub hasn't confirmed.
    let pending = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_window, app| view.read(app).viewed_load_for_test())
            .expect("reviewing")
    };
    let reload = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_window, app| view.update(app, |this, cx| this.load_viewed_states(cx)));
        pending(cx).0
    };
    let pushed = |cx: &mut gpui::VisualTestContext, path: &str, viewed, result| {
        cx.update(|_window, app| {
            view.update(app, |this, cx| {
                this.review_viewed_pushed(REPO, 7, path, viewed, result, cx)
            })
        });
    };
    let states = |files: [(&str, ViewedState); 3]| {
        Ok(ViewedStates {
            pr_id: "PR_kw1".into(),
            files: files
                .into_iter()
                .map(|(path, state)| (path.to_string(), state))
                .collect(),
        })
    };
    use ViewedState::*;

    press(cx, "1 r");
    let first = pending(cx).0;
    assert_eq!(marks(cx), Some((vec![], vec![], ViewedSync::Loading)));
    // space while GitHub's marks are still loading: shown at once, kept.
    press(cx, "1 space");
    assert_eq!(pending(cx).1, [("a.rs".to_string(), true, false)]);
    // An answer to an older load is dropped.
    let second = reload(cx);
    seed_states(
        cx,
        first,
        states([("a.rs", Viewed), ("b.rs", Viewed), ("c.rs", Viewed)]),
    );
    assert_eq!(marks(cx).map(|(.., sync)| sync), Some(ViewedSync::Loading));
    // GitHub couldn't be reached: the marks here stay, and Your review says so.
    seed_states(cx, second, Err(PrError::Failed("offline".into())));
    assert_eq!(
        marks(cx),
        Some((vec!["a.rs".into()], vec![], ViewedSync::Offline))
    );
    // GitHub answers without the press: it stays on top, waiting to go up.
    // b.rs was viewed there, then changed.
    let third = reload(cx);
    seed_states(
        cx,
        third,
        states([("a.rs", Unviewed), ("b.rs", Dismissed), ("c.rs", Unviewed)]),
    );
    let github = ViewedSync::GitHub {
        pr_id: "PR_kw1".into(),
    };
    assert_eq!(
        marks(cx),
        Some((vec!["a.rs".into()], vec!["b.rs".into()], github.clone()))
    );
    // GitHub takes it; a load started after that has it and the press goes.
    pushed(cx, "a.rs", true, Ok(()));
    assert_eq!(pending(cx).1, [("a.rs".to_string(), true, true)]);
    let fourth = reload(cx);
    seed_states(
        cx,
        fourth,
        states([("a.rs", Viewed), ("b.rs", Dismissed), ("c.rs", Unviewed)]),
    );
    assert_eq!(pending(cx).1, []);
    // space on b.rs marks it at once; GitHub refusing puts it back, dismissed.
    press(cx, "1 space");
    assert_eq!(
        marks(cx),
        Some((vec!["a.rs".into(), "b.rs".into()], vec![], github.clone()))
    );
    pushed(cx, "b.rs", true, Err(PrError::Failed("no".into())));
    assert_eq!(
        marks(cx),
        Some((vec!["a.rs".into()], vec!["b.rs".into()], github))
    );

    // An outdated thread: listed after your pending comments, and r replies.
    let outdated = ReviewThread {
        root_id: 5,
        path: "a.rs".into(),
        side: ReviewSide::Right,
        line: None,
        original_line: Some(12),
        is_resolved: false,
        is_outdated: true,
        pull_request_review_id: None,
        diff_hunk: String::new(),
        comments: vec![ThreadComment {
            author: "octo".into(),
            body: "Old point\nmore".into(),
            body_truncated: false,
            at: String::new(),
        }],
    };
    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            this.seed_review_threads_for_test(vec![outdated], Vec::new(), cx)
        })
    });
    // A pending comment first, so the outdated one sits after it.
    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            this.add_review_comment(
                REPO,
                7,
                crate::github::ReviewAnchor {
                    path: "b.rs".into(),
                    side: ReviewSide::Right,
                    line: 1,
                    start: None,
                },
                "Nit".into(),
                None,
                None,
                cx,
            );
        })
    });
    press(cx, "4 j j r");
    assert!(popover_is_open(cx, &view));
    press(cx, "o k secondary-enter");
    let replies = cx.update(|_window, app| {
        view.read(app)
            .active_review()
            .map(|review| {
                review
                    .draft
                    .comments
                    .iter()
                    .map(|comment| {
                        (
                            comment.body.clone(),
                            comment.anchor.line,
                            comment.reply_to.as_ref().map(|to| to.id),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });
    assert_eq!(
        replies,
        [
            ("Nit".to_string(), 1, None),
            ("ok".to_string(), 12, Some(5))
        ]
    );

    press(cx, "q");
}

#[gpui::test]
fn the_review_file_list_hides_viewed_files_and_filters_with_slash(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    seed_three_file_pull_request(cx, &view);
    // The files listed, and the open one.
    let listed = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_window, app| {
            view.read(app)
                .active_review()
                .map(|review| {
                    let listed = (0..review.files.len())
                        .filter(|ix| review.file_listed(*ix))
                        .collect::<Vec<_>>();
                    (listed, review.file_ix)
                })
                .expect("reviewing")
        })
    };

    press(cx, "1 r");
    assert_eq!(listed(cx), (vec![0, 1, 2], 0));
    // Viewed, then on to b.rs: a.rs leaves the list.
    press(cx, "1 space");
    assert_eq!(listed(cx), (vec![1, 2], 1));
    // V shows viewed files, and hides them again.
    press(cx, "shift-v");
    assert_eq!(listed(cx), (vec![0, 1, 2], 1));
    press(cx, "shift-v");
    assert_eq!(listed(cx), (vec![1, 2], 1));
    // j/k skip the hidden a.rs.
    press(cx, "k");
    assert_eq!(listed(cx).1, 1);
    press(cx, "j k");
    assert_eq!(listed(cx).1, 1);

    // `/` opens the filter; typing narrows the list as it goes.
    press(cx, "/");
    press(cx, "c");
    assert_eq!(listed(cx).0, [2]);
    // Enter keeps it and hands the keyboard back to the list.
    press(cx, "enter");
    assert_eq!(focused(cx, &view), Some(Sidebar));
    assert_eq!(listed(cx).0, [2]);
    press(cx, "j");
    assert_eq!(listed(cx), (vec![2], 2));
    // Esc from the list clears it.
    press(cx, "escape");
    assert_eq!(listed(cx), (vec![1, 2], 2));

    press(cx, "q");
}

/// The open diff's file and lane, after syncing the store's latest snapshot.
fn diff_file(
    cx: &mut gpui::VisualTestContext,
    view: &View,
) -> Option<(std::path::PathBuf, DiffArea)> {
    sync_store_snapshot(cx, view);
    cx.update(|_window, app| {
        match view.read(app).main_pane.read(app).state.repos[0]
            .diff_state
            .diff_target
            .as_ref()
        {
            Some(DiffTarget::WorkingTree { path, area }) => Some((path.clone(), *area)),
            _ => None,
        }
    })
}

#[gpui::test]
fn the_changes_list_walks_every_file_once_and_flips_them_in_place(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    let mut repo = panel_repo();
    let file = |path: &str, kind| gitcomet_core::domain::FileStatus {
        path: path.into(),
        kind,
        conflict: None,
    };
    // a.rs is partly staged, b.rs not at all, c.rs only staged, new.rs untracked.
    repo.worktree_status = Loadable::Ready(Arc::new(vec![
        file("a.rs", FileStatusKind::Modified),
        file("b.rs", FileStatusKind::Modified),
        file("new.rs", FileStatusKind::Untracked),
    ]));
    repo.staged_status = Loadable::Ready(Arc::new(vec![
        file("a.rs", FileStatusKind::Modified),
        file("c.rs", FileStatusKind::Added),
    ]));
    apply_state(cx, &view, app_state_with_active_repo(repo));
    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            this.set_change_tracking_view(crate::view::ChangeTrackingView::Unified, cx)
        })
    });
    draw_and_drain_test_window(cx);
    let letters = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_window, app| {
            view.read(app)
                .details_pane
                .read(app)
                .changes_drawn(REPO)
                .unwrap_or_default()
                .into_iter()
                .map(|(path, lanes)| {
                    let [staged, unstaged] = lanes.letters();
                    format!("{staged}{unstaged} {}", path.display())
                })
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(letters(cx), ["MM a.rs", " M b.rs", "A  c.rs", "?? new.rs"]);

    press(cx, "4");
    let walk = [
        ("a.rs", DiffArea::Unstaged),
        ("b.rs", DiffArea::Unstaged),
        ("c.rs", DiffArea::Staged),
        ("new.rs", DiffArea::Unstaged),
    ];
    for (path, area) in walk {
        press(cx, "j");
        wait_until(cx, "the next file to open", |cx| {
            diff_file(cx, &view) == Some((path.into(), area))
        });
    }
    // One at a time: each step reads where the last one landed.
    press(cx, "k");
    wait_until(cx, "c.rs again", |cx| {
        diff_file(cx, &view) == Some(("c.rs".into(), DiffArea::Staged))
    });
    press(cx, "k");
    wait_until(cx, "b.rs again", |cx| {
        diff_file(cx, &view) == Some(("b.rs".into(), DiffArea::Unstaged))
    });

    // Shift+F narrows the list: Unstaged leaves out what's only staged, and
    // the untracked file.
    press(cx, "shift-f");
    let filter = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_window, app| view.read(app).details_pane.read(app).changes_filter)
    };
    assert_eq!(filter(cx), crate::view::ChangesFilter::Unstaged);
    assert_eq!(letters(cx), ["MM a.rs", " M b.rs"]);
    press(cx, "shift-f shift-f shift-f");
    assert_eq!(filter(cx), crate::view::ChangesFilter::All);

    // `space` stages b.rs where it stands: the diff follows it to the staged
    // side and focus stays in Details.
    press(cx, "space");
    wait_until(cx, "b.rs's staged diff", |cx| {
        diff_file(cx, &view) == Some(("b.rs".into(), DiffArea::Staged))
    });
    assert_eq!(focused(cx, &view), Some(Details));
}

#[gpui::test]
fn shift_n_sets_up_a_pull_request_from_anywhere(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    let mut repo = panel_repo();
    repo.remotes = Loadable::Ready(Arc::new(vec![gitcomet_core::domain::Remote {
        name: "origin".into(),
        url: Some("https://github.com/owner/repo.git".into()),
    }]));
    repo.remotes_rev = 1;
    apply_state(cx, &view, app_state_with_active_repo(repo));
    press(cx, "4");
    assert_key_opens(
        cx,
        &view,
        "shift-n",
        PopoverKind::CreatePullRequest {
            repo_id: REPO,
            branch: None,
        },
        Details,
    );
}

#[gpui::test]
fn the_create_dialog_steps_the_base_toggles_the_push_and_guards_submit(
    cx: &mut gpui::TestAppContext,
) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    let mut repo = panel_repo();
    repo.head_branch = Loadable::Ready("feature".into());
    repo.remotes = Loadable::Ready(Arc::new(vec![gitcomet_core::domain::Remote {
        name: "origin".into(),
        url: Some("https://github.com/owner/repo.git".into()),
    }]));
    repo.remotes_rev = 1;
    let target = CommitId("7337337337337337".into());
    let remote_branch = |name: &str| gitcomet_core::domain::RemoteBranch {
        remote: "origin".into(),
        name: name.into(),
        target: target.clone(),
    };
    repo.remote_branches = Loadable::Ready(Arc::new(vec![
        remote_branch("main"),
        remote_branch("dev"),
        remote_branch("feature-old"),
    ]));
    repo.remote_branches_rev = 2;
    apply_state(cx, &view, app_state_with_active_repo(repo));
    press(cx, "4 shift-n");
    assert!(popover_open(
        cx,
        &view,
        &PopoverKind::CreatePullRequest {
            repo_id: REPO,
            branch: None,
        }
    ));
    let form = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_window, app| {
            let host = view.read(app).popover_host.clone();
            host.update(app, |host, cx| host.create_pull_request_form_for_test(cx))
        })
    };
    // Focus starts in the title; `feature` isn't on GitHub, so it's pushed
    // first by default.
    press(cx, "h i");
    assert_eq!(form(cx), (String::new(), true, true));

    // Alt+B walks the remote's branches, default-like names first.
    press(cx, "alt-b");
    assert_eq!(form(cx).0, "dev");
    press(cx, "alt-b");
    assert_eq!(form(cx).0, "main");

    // Without the push, GitHub has no branch to open it from.
    press(cx, "alt-p");
    assert_eq!(form(cx), ("main".into(), false, false));
    press(cx, "alt-p");
    assert_eq!(form(cx), ("main".into(), true, true));

    // An open pull request from this very branch (same owner) blocks a
    // second one; a stranger's same-named branch doesn't.
    let seed = |cx: &mut gpui::VisualTestContext, owner: &str| {
        cx.update(|_window, app| {
            view.update(app, |this, _| {
                this.seed_pull_requests_for_test(
                    REPO,
                    vec![crate::github::PullRequestSummary {
                        number: 9,
                        title: "Earlier".into(),
                        author: owner.into(),
                        head: "feature".into(),
                        head_owner: owner.into(),
                        base: "main".into(),
                        is_draft: false,
                        is_cross_repository: false,
                        review: None,
                        checks: Default::default(),
                        review_requested: false,
                        is_mine: false,
                    }],
                    None,
                );
            })
        });
        draw_and_drain_test_window(cx);
    };
    seed(cx, "stranger");
    assert!(form(cx).2);
    seed(cx, "owner");
    assert!(!form(cx).2);
}

#[gpui::test]
fn slash_filters_the_changes_list_fuzzily_and_by_file_type(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    let mut repo = panel_repo();
    let file = |path: &str| gitcomet_core::domain::FileStatus {
        path: path.into(),
        kind: FileStatusKind::Modified,
        conflict: None,
    };
    repo.worktree_status = Loadable::Ready(Arc::new(vec![
        file("docs/notes.md"),
        file("src/pane.rs"),
        file("src/panel.rs"),
    ]));
    apply_state(cx, &view, app_state_with_active_repo(repo));
    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            this.set_change_tracking_view(crate::view::ChangeTrackingView::Unified, cx)
        })
    });
    draw_and_drain_test_window(cx);
    let shown = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_window, app| {
            view.read(app)
                .details_pane
                .read(app)
                .changes_drawn(REPO)
                .unwrap_or_default()
                .into_iter()
                .map(|(path, _)| path.to_string_lossy().replace('\\', "/"))
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(shown(cx).len(), 3);

    // `/` hands the keyboard to the filter box: letters filter, they don't
    // run panel keys. "pnl" is in order in panel.rs only.
    press(cx, "4 /");
    press(cx, "p n l");
    assert_eq!(shown(cx), ["src/panel.rs"]);
    assert_eq!(focused(cx, &view), None);

    // Enter keeps the filter and goes back to the list; Esc there clears it.
    press(cx, "enter");
    assert_eq!(focused(cx, &view), Some(Details));
    assert_eq!(shown(cx), ["src/panel.rs"]);
    press(cx, "escape");
    assert_eq!(shown(cx).len(), 3);

    // `.md` keeps a file type; Esc in the box clears it and returns too.
    press(cx, "/");
    press(cx, ". m d");
    assert_eq!(shown(cx), ["docs/notes.md"]);
    press(cx, "escape");
    assert_eq!(shown(cx).len(), 3);
    assert_eq!(focused(cx, &view), Some(Details));

    // `/` works from another panel too: the Changes list is still what it
    // filters.
    press(cx, "2 /");
    press(cx, "p a n e");
    assert_eq!(shown(cx), ["src/pane.rs", "src/panel.rs"]);
    press(cx, "escape");
    assert_eq!(shown(cx).len(), 3);
}

#[gpui::test]
fn shift_j_and_k_select_a_range_of_changes_and_esc_drops_it(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    let mut repo = panel_repo();
    let file = |path: &str| gitcomet_core::domain::FileStatus {
        path: path.into(),
        kind: FileStatusKind::Modified,
        conflict: None,
    };
    repo.worktree_status = Loadable::Ready(Arc::new(vec![
        file("a.rs"),
        file("b.rs"),
        file("c.rs"),
        file("d.rs"),
    ]));
    apply_state(cx, &view, app_state_with_active_repo(repo));
    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            this.set_change_tracking_view(crate::view::ChangeTrackingView::Unified, cx)
        })
    });
    draw_and_drain_test_window(cx);
    let selection = |cx: &mut gpui::VisualTestContext| {
        sync_store_snapshot(cx, &view);
        cx.update(|_window, app| {
            view.read(app)
                .details_pane
                .read(app)
                .changes_selection(REPO)
                .map(|files| {
                    files
                        .into_iter()
                        .map(|(path, _)| path.to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                })
        })
    };

    // One step at a time: each reads where the last one landed.
    press(cx, "3 j");
    wait_until(cx, "a.rs to open", |cx| {
        diff_path(cx, &view).as_deref() == Some(Path::new("a.rs"))
    });
    press(cx, "j");
    wait_until(cx, "b.rs to open", |cx| {
        diff_path(cx, &view).as_deref() == Some(Path::new("b.rs"))
    });
    assert_eq!(selection(cx), None);
    // Each Shift+J steps the open file on and widens the range behind it.
    for (open, range) in [
        ("c.rs", vec!["b.rs", "c.rs"]),
        ("d.rs", vec!["b.rs", "c.rs", "d.rs"]),
    ] {
        press(cx, "shift-j");
        wait_until(cx, "the range to grow", |cx| {
            diff_path(cx, &view).as_deref() == Some(Path::new(open))
        });
        assert_eq!(
            selection(cx),
            Some(range.into_iter().map(String::from).collect())
        );
    }
    // Back over the anchor: the range flips to the other side of it.
    for (open, range) in [
        ("c.rs", vec!["b.rs", "c.rs"]),
        ("b.rs", vec![]),
        ("a.rs", vec!["a.rs", "b.rs"]),
    ] {
        press(cx, "shift-k");
        wait_until(cx, "the range to move", |cx| {
            diff_path(cx, &view).as_deref() == Some(Path::new(open))
        });
        let expected = (!range.is_empty()).then(|| range.into_iter().map(String::from).collect());
        assert_eq!(selection(cx), expected);
    }
    press(cx, "escape");
    assert_eq!(selection(cx), None);
    assert_eq!(focused(cx, &view), Some(Details));

    // A plain step drops the range too.
    press(cx, "shift-j");
    wait_until(cx, "a range again", |cx| selection(cx).is_some());
    press(cx, "j");
    wait_until(cx, "the range to go", |cx| selection(cx).is_none());
}

#[gpui::test]
fn two_is_the_middle_and_esc_leaves_a_commit_for_your_changes(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    // `2` is History while no diff is open; with one open, it's the diff.
    press(cx, "2");
    assert_eq!(focused(cx, &view), Some(History));
    press(cx, "3 j");
    wait_until(cx, "a file to open", |cx| diff_path(cx, &view).is_some());
    press(cx, "2");
    assert_eq!(focused(cx, &view), Some(Diff));
    press(cx, "escape");
    wait_until(cx, "the diff to close", |cx| diff_path(cx, &view).is_none());

    // A commit selected in History fills Details; esc hands it back to your
    // changes.
    press(cx, "2 j");
    wait_until(cx, "a commit to be selected", |cx| {
        selected_commit(cx, &view).is_some()
    });
    press(cx, "escape");
    wait_until(cx, "the commit to be dropped", |cx| {
        selected_commit(cx, &view).is_none()
    });
    assert_eq!(focused(cx, &view), Some(History));
}

#[gpui::test]
fn files_past_the_first_page_append_in_order_into_the_list_and_the_review(
    cx: &mut gpui::TestAppContext,
) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    seed_three_file_pull_request(cx, &view);
    let file = |path: &str| crate::github::PullRequestFile {
        path: path.into(),
        additions: 1,
        deletions: 0,
    };
    let land = |cx: &mut gpui::VisualTestContext, page: u32, paths: &[&str]| {
        let files = paths.iter().map(|path| file(path)).collect();
        cx.update(|_window, app| {
            view.update(app, |this, cx| {
                this.land_pull_request_files_page_for_test(REPO, page, files, cx)
            })
        });
        draw_and_drain_test_window(cx);
    };
    let lists = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_window, app| {
            let this = view.read(app);
            let pull_request: Vec<String> = this
                .active_pull_requests()
                .and_then(|prs| prs.detail.ready())
                .map(|detail| detail.files.iter().map(|file| file.path.clone()).collect())
                .unwrap_or_default();
            let review = this
                .active_review()
                .map(|review| review.files.clone())
                .unwrap_or_default();
            (pull_request, review)
        })
    };

    // Reviewing starts on the first page; the rest comes after.
    press(cx, "1 r");
    assert_eq!(lists(cx).1, ["a.rs", "b.rs", "c.rs"]);
    // Page 3 before page 2: held back, so nothing shifts.
    land(cx, 3, &["e.rs"]);
    assert_eq!(lists(cx).0, ["a.rs", "b.rs", "c.rs"]);
    // Page 2 lets both in, in GitHub's order, in both lists.
    land(cx, 2, &["d.rs", "b.rs"]);
    let expected = ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"];
    assert_eq!(
        lists(cx),
        (
            expected.map(String::from).to_vec(),
            expected.map(String::from).to_vec()
        )
    );
    press(cx, "q");
}

#[gpui::test]
fn pull_request_keys_navigate_conversation_and_threads(cx: &mut gpui::TestAppContext) {
    use crate::github::{ConversationEntry, ReviewSide, ReviewThread, ThreadComment};
    use crate::view::pull_requests::PrContentTab;

    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    seed_three_file_pull_request(cx, &view);
    cx.update(|_window, app| {
        view.update(app, |this, _| {
            let mut detail =
                (**this.active_pull_requests().unwrap().detail.ready().unwrap()).clone();
            detail.body = "# Summary".into();
            detail.conversation = vec![
                ConversationEntry {
                    id: "first".into(),
                    author: "alice".into(),
                    verb: "commented",
                    at: "2025-01-01T00:00:00Z".into(),
                    body: "First".into(),
                    body_truncated: false,
                    review_id: None,
                },
                ConversationEntry {
                    id: "second".into(),
                    author: "bob".into(),
                    verb: "approved",
                    at: "2025-01-02T00:00:00Z".into(),
                    body: "Second".into(),
                    body_truncated: false,
                    review_id: None,
                },
            ];
            this.seed_pull_request_detail_for_test(REPO, detail, "c".repeat(40));
            let thread = |root_id, path: &str, line, resolved, outdated| ReviewThread {
                root_id,
                path: path.into(),
                side: ReviewSide::Right,
                line,
                original_line: Some(12),
                is_resolved: resolved,
                is_outdated: outdated,
                pull_request_review_id: None,
                diff_hunk: String::new(),
                comments: vec![ThreadComment {
                    author: "alice".into(),
                    body: "**Review this**".into(),
                    body_truncated: false,
                    at: String::new(),
                }],
            };
            this.seed_pull_request_threads_for_test(
                REPO,
                vec![
                    thread(40, "a.rs", Some(12), false, false),
                    thread(2, "b.rs", Some(12), true, false),
                    thread(3, "c.rs", None, false, true),
                    thread(4, "a.rs", Some(14), false, false),
                ],
            );
        })
    });
    draw_and_drain_test_window(cx);

    press(cx, "1 enter");
    assert_eq!(focused(cx, &view), Some(History));
    assert!(!cx.update(|_window, app| view.read(app).diff_is_open()));
    press(cx, "j j k");
    assert_eq!(
        cx.update(|_window, app| view
            .read(app)
            .active_pull_requests()
            .unwrap()
            .selected_entry),
        Some(0)
    );
    press(cx, "]");
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_pull_requests().unwrap().content_tab),
        PrContentTab::Comments
    );
    press(cx, "j j");
    assert_eq!(
        cx.update(|_window, app| {
            let prs = view.read(app).active_pull_requests().unwrap();
            (prs.selected_thread, prs.visible_thread_indexes())
        }),
        (Some(3), vec![0, 3])
    );
    press(cx, "[");
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_pull_requests().unwrap().content_tab),
        PrContentTab::Conversation
    );
    press(cx, "enter shift-c t g shift-t m escape");
    assert!(cx.update(|_window, app| view.read(app).active_review().is_none()));
    assert!(!cx.update(|_window, app| view.read(app).diff_is_open()));
    press(cx, "]");
    press(cx, "shift-v j");
    assert_eq!(
        cx.update(|_window, app| {
            let prs = view.read(app).active_pull_requests().unwrap();
            (prs.selected_thread, prs.visible_thread_indexes())
        }),
        (Some(1), vec![0, 3, 1, 2])
    );
    press(cx, "enter");
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_review().map(|review| review.file_ix)),
        Some(1)
    );
    let review_target = |cx: &mut gpui::VisualTestContext| {
        sync_store_snapshot(cx, &view);
        cx.update(|_window, app| {
            view.read(app).main_pane.read(app).state.repos[0]
                .diff_state
                .diff_target
                .clone()
        })
    };
    let target = review_target(cx).expect("review file diff is open");
    cx.update(|_window, app| {
        view.read(app).store.dispatch(Msg::SetSidebarMode {
            mode: gitcomet_state::model::SidebarMode::Files,
        });
    });
    wait_until(cx, "Files sidebar mode", |cx| {
        cx.update(|_window, app| {
            view.read(app).store.snapshot().sidebar_mode
                == gitcomet_state::model::SidebarMode::Files
        })
    });
    sync_store_snapshot(cx, &view);
    assert!(cx.update(|_window, app| view.read(app).active_review().is_none()));
    cx.update(|_window, app| {
        view.read(app).store.dispatch(Msg::SetSidebarMode {
            mode: gitcomet_state::model::SidebarMode::PullRequests,
        });
    });
    wait_until(cx, "Pull requests sidebar mode", |cx| {
        cx.update(|_window, app| {
            view.read(app).store.snapshot().sidebar_mode
                == gitcomet_state::model::SidebarMode::PullRequests
        })
    });
    sync_store_snapshot(cx, &view);
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_review().map(|review| review.file_ix)),
        Some(1)
    );
    assert_eq!(review_target(cx), Some(target));
}

fn pr_scroll_handle(cx: &mut gpui::VisualTestContext, view: &View) -> gpui::ScrollHandle {
    cx.update(|_window, app| {
        view.read(app)
            .main_pane
            .read(app)
            .pull_request_scroll
            .clone()
    })
}

fn pr_content_selection(
    cx: &mut gpui::VisualTestContext,
    view: &View,
) -> (Option<usize>, Option<usize>) {
    cx.update(|_window, app| {
        let prs = view.read(app).active_pull_requests().unwrap();
        (prs.selected_entry, prs.selected_thread)
    })
}

/// A description far taller than the window, and one short comment after it.
fn seed_long_description(cx: &mut gpui::VisualTestContext, view: &View) {
    use crate::github::ConversationEntry;
    seed_three_file_pull_request(cx, view);
    cx.update(|_window, app| {
        view.update(app, |this, _| {
            let mut detail =
                (**this.active_pull_requests().unwrap().detail.ready().unwrap()).clone();
            detail.body = (0..400)
                .map(|n| format!("Paragraph {n} of a very long pull request description."))
                .collect::<Vec<_>>()
                .join("\n\n");
            detail.conversation = vec![ConversationEntry {
                id: "only".into(),
                author: "alice".into(),
                verb: "commented",
                at: "2025-01-01T00:00:00Z".into(),
                body: "Short".into(),
                body_truncated: false,
                review_id: None,
            }];
            this.seed_pull_request_detail_for_test(REPO, detail, "c".repeat(40));
        })
    });
    draw_and_drain_test_window(cx);
}

#[gpui::test]
fn a_tall_description_scrolls_with_j_k_and_the_page_keys(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    seed_long_description(cx, &view);
    press(cx, "1 enter");
    assert_eq!(focused(cx, &view), Some(History));
    let scroll = pr_scroll_handle(cx, &view);
    let max = scroll.max_offset().y;
    assert!(max > gpui::px(0.0), "the fixture must overflow the window");
    assert_eq!(scroll.offset().y, gpui::px(0.0));

    // Nothing selected, the description taller than the window: `j` scrolls
    // it instead of jumping to the comment beneath.
    press(cx, "j");
    let after_j = scroll.offset().y;
    assert!(after_j < gpui::px(0.0), "j scrolls the tall description");
    assert_eq!(pr_content_selection(cx, &view), (None, None));
    press(cx, "down");
    assert!(scroll.offset().y < after_j, "the arrow key scrolls too");
    press(cx, "k");
    assert_eq!(
        scroll.offset().y,
        after_j,
        "k scrolls back up by the same step"
    );

    press(cx, "pagedown");
    let after_page = scroll.offset().y;
    assert!(after_page < after_j);
    press(cx, "pageup");
    assert_eq!(scroll.offset().y, after_j);

    press(cx, "ctrl-d");
    assert!(scroll.offset().y < after_j, "ctrl-d is a half page down");
    press(cx, "ctrl-u");
    assert_eq!(scroll.offset().y, after_j, "ctrl-u is a half page up");

    press(cx, "end");
    assert_eq!(scroll.offset().y, -max);
    press(cx, "home");
    assert_eq!(scroll.offset().y, gpui::px(0.0));

    // Once the description's end is showing, `j` goes on to the entry after it.
    press(cx, "end");
    press(cx, "j");
    assert_eq!(pr_content_selection(cx, &view), (Some(0), None));
}

#[gpui::test]
fn comments_open_with_the_first_thread_selected_and_scroll_with_page_keys(
    cx: &mut gpui::TestAppContext,
) {
    use crate::github::{ReviewSide, ReviewThread, ThreadComment};
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    seed_three_file_pull_request(cx, &view);
    cx.update(|_window, app| {
        view.update(app, |this, _| {
            let thread = |root_id, line| ReviewThread {
                root_id,
                path: "a.rs".into(),
                side: ReviewSide::Right,
                line: Some(line),
                original_line: Some(line),
                is_resolved: false,
                is_outdated: false,
                pull_request_review_id: None,
                diff_hunk: String::new(),
                comments: vec![ThreadComment {
                    author: "alice".into(),
                    body: "Look at this".into(),
                    body_truncated: false,
                    at: String::new(),
                }],
            };
            this.seed_pull_request_threads_for_test(
                REPO,
                (1..=60).map(|n| thread(n, n as u32)).collect(),
            );
        })
    });
    draw_and_drain_test_window(cx);
    press(cx, "1 enter");
    assert_eq!(pr_content_selection(cx, &view), (None, None));
    press(cx, "]");
    // The tab opens with one card expanded, as in the design.
    assert_eq!(pr_content_selection(cx, &view), (None, Some(0)));

    let scroll = pr_scroll_handle(cx, &view);
    assert!(scroll.max_offset().y > gpui::px(0.0));
    press(cx, "pagedown");
    assert!(scroll.offset().y < gpui::px(0.0));
    press(cx, "end");
    assert_eq!(scroll.offset().y, -scroll.max_offset().y);
    press(cx, "home");
    assert_eq!(scroll.offset().y, gpui::px(0.0));
    // A short thread is not taller than the window: `j` moves to the next one.
    press(cx, "j");
    assert_eq!(pr_content_selection(cx, &view), (None, Some(1)));
}

#[gpui::test]
fn push_and_create_waits_for_its_own_push(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (view, cx) = fixture(cx);
    let push = gitcomet_core::services::BranchPushRequest {
        remote: "origin".into(),
        local_branch: "feat".into(),
        branch: "feat".into(),
        head: CommitId("1".repeat(40).into()),
        set_upstream: true,
    };
    let outcome = |request: &gitcomet_core::services::BranchPushRequest| {
        gitcomet_state::model::BranchPushOutcome {
            request: request.clone(),
            error: Some("rejected".into()),
            auth_prompted: true,
        }
    };
    let landed = |cx: &mut gpui::VisualTestContext, outcome| {
        cx.update(|_window, app| {
            view.update(app, |this, cx| {
                this.pull_request_push_landed(REPO, &outcome, cx);
                this.pull_request_awaits_push_for_test(REPO)
            })
        })
    };
    cx.update(|_window, app| {
        view.update(app, |this, _| {
            this.await_pull_request_push_for_test(REPO, &push);
        })
    });
    // Another branch's push finishing isn't it.
    let other = gitcomet_core::services::BranchPushRequest {
        local_branch: "main".into(),
        ..push.clone()
    };
    assert!(landed(cx, outcome(&other)));
    // Its own push failing on credentials ends the wait; the store asks.
    assert!(!landed(cx, outcome(&push)));
}
