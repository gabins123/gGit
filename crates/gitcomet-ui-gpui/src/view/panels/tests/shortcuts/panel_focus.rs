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
    // No diff is open, so there is nothing for 3 to focus.
    press(cx, "3");
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
                    base: "main".into(),
                    is_draft: false,
                    review: None,
                    checks: Default::default(),
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
    press(cx, "r");
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
        &PopoverKind::CreatePullRequest { repo_id: REPO }
    ));
    press(cx, "escape");
    assert_eq!(focused(cx, &view), Some(Sidebar));
}
