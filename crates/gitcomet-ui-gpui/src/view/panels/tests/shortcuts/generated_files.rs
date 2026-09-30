use super::*;
use crate::view::pull_requests::PrLoad;

/// `fetch_pull_request_generated_files` opens the repository through
/// `AppStore::backend()` — the same backend the store itself was built with —
/// rather than a hardcoded concrete backend, so it also runs (and fails
/// gracefully) against a test's fake `GitBackend`, with no `cfg!(test)`
/// bypass needed.
#[gpui::test]
fn fetch_generated_files_uses_the_stores_own_backend_and_fails_gracefully_without_one(
    cx: &mut gpui::TestAppContext,
) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        crate::view::GitCometView::new(store, events, None, window, cx)
    });

    let repo_id = gitcomet_state::model::RepoId(9503);
    let workdir = std::env::temp_dir().join(format!(
        "gitcomet_ui_test_{}_fetch_generated_files_backend",
        std::process::id()
    ));
    let file = |path: &str| crate::github::PullRequestFile {
        path: path.into(),
        additions: 1,
        deletions: 0,
    };

    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            let mut repo = opening_repo_state(repo_id, &workdir);
            // `fetch_pull_request_generated_files` bails out early without a
            // GitHub remote (`github_target_for`).
            repo.remotes = gitcomet_state::model::Loadable::Ready(Arc::new(vec![
                gitcomet_core::domain::Remote {
                    name: "origin".into(),
                    url: Some("https://github.com/owner/repo.git".into()),
                },
            ]));
            let next_state = Arc::new(AppState {
                repos: vec![repo],
                active_repo: Some(repo_id),
                sidebar_mode: gitcomet_state::model::SidebarMode::PullRequests,
                ..AppState::test_default()
            });
            push_test_state(this, next_state, cx);
            this.pull_requests.repo_mut(repo_id).detail = PrLoad::Ready(Arc::new(
                crate::github::PullRequestDetail {
                    number: 1,
                    title: String::new(),
                    body: String::new(),
                    url: String::new(),
                    author: String::new(),
                    created_at: String::new(),
                    head: "feat".into(),
                    head_oid: "h1".to_string(),
                    base: "main".into(),
                    base_oid: "base".to_string(),
                    is_draft: false,
                    is_cross_repository: false,
                    state: "OPEN".into(),
                    review: None,
                    mergeable: None,
                    additions: 1,
                    deletions: 0,
                    changed_files: 1,
                    files: vec![file("Cargo.lock")],
                    checks: Default::default(),
                    check_runs: vec![],
                    conversation: vec![],
                    reviewers: vec![],
                    commits: vec![],
                },
            ));
        });
    });
    // `github_target_for` reads `self.state`, which only catches up with the
    // state just pushed once a render round-trips through the subscription.
    draw_and_drain_test_window(cx);
    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            this.fetch_pull_request_generated_files(repo_id, cx);
            assert!(
                matches!(
                    this.pull_requests.repo(repo_id).map(|prs| &prs.generated_files),
                    Some(PrLoad::Loading)
                ),
                "should start loading rather than short-circuit on cfg!(test)"
            );
        });
    });

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        draw_and_drain_test_window(cx);
        let settled = cx.update(|_window, app| {
            matches!(
                view.read(app)
                    .pull_requests
                    .repo(repo_id)
                    .map(|prs| &prs.generated_files),
                Some(PrLoad::Idle)
            )
        });
        if settled {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for the generated-files fetch to settle"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let _ = std::fs::remove_dir_all(&workdir);
}

/// `Shift+G` shows and hides a review's generated files, the way `Shift+V`
/// does for viewed ones — and a generated file's visibility never depends on
/// being viewed: it stays hidden until `Shift+G` shows it, viewed or not.
#[gpui::test]
fn shift_g_shows_and_hides_generated_files(cx: &mut gpui::TestAppContext) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        crate::view::GitCometView::new(store, events, None, window, cx)
    });
    bind_app_keys_and_global_diff_fallback_for_test(cx);

    let repo_id = gitcomet_state::model::RepoId(9501);
    let workdir = std::env::temp_dir().join(format!(
        "gitcomet_ui_test_{}_shift_g_generated_files",
        std::process::id()
    ));

    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            let repo = opening_repo_state(repo_id, &workdir);
            let next_state = Arc::new(AppState {
                repos: vec![repo],
                active_repo: Some(repo_id),
                // `active_review` (and so `handle_review_key`) only sees the
                // review while the Pull requests tab is showing.
                sidebar_mode: gitcomet_state::model::SidebarMode::PullRequests,
                ..AppState::test_default()
            });
            push_test_state(this, next_state, cx);
            this.open_review_for_test(
                repo_id,
                77,
                vec![
                    "a.rs".to_string(),
                    "Cargo.lock".to_string(),
                    "b.rs".to_string(),
                ],
                "h1",
                cx,
            );
            this.seed_review_generated_files_for_test(["Cargo.lock".to_string()], cx);
        });
    });

    draw_and_drain_test_window(cx);

    let listed = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_window, app| {
            view.read(app)
                .active_review()
                .map(|review| {
                    (0..review.files.len())
                        .filter(|ix| review.file_listed(*ix))
                        .collect::<Vec<_>>()
                })
                .expect("reviewing")
        })
    };

    assert_eq!(
        listed(cx),
        vec![0, 2],
        "Cargo.lock is generated and hidden by default"
    );

    cx.simulate_keystrokes("shift-g");
    draw_and_drain_test_window(cx);
    assert_eq!(listed(cx), vec![0, 1, 2], "shift-g shows generated files");

    cx.simulate_keystrokes("shift-g");
    draw_and_drain_test_window(cx);
    assert_eq!(
        listed(cx),
        vec![0, 2],
        "shift-g again hides them"
    );

    let _ = std::fs::remove_dir_all(&workdir);
}

/// `enter` on a generated file's "Generated file" placeholder loads its diff
/// — from the Sidebar, and from the Diff panel itself once focus moves there.
///
/// `Msg::SelectDiff`'s effects need a real, opened repository (the store's
/// worker thread holds its own `Arc<dyn GitRepository>` per repo, separate
/// from `AppState`'s data), so — like `status_staging.rs` and
/// `diff_marker_refresh.rs` — this uses a real temporary git repository and
/// `AppStore::insert_repo_for_test` rather than `TestBackend`, which never
/// opens one.
#[gpui::test]
fn enter_loads_a_generated_files_diff_from_its_placeholder(cx: &mut gpui::TestAppContext) {
    let repo_id = gitcomet_state::model::RepoId(9502);
    let workdir = std::env::temp_dir().join(format!(
        "gitcomet_ui_test_{}_enter_loads_generated_diff",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir).expect("create workdir");
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&workdir)
            .args(args)
            .output()
            .expect("git command to run");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.name", "Test"]);
    git(&["config", "user.email", "test@example.com"]);
    std::fs::write(workdir.join("Cargo.lock"), "# lockfile\n").expect("write Cargo.lock");
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "initial"]);
    let head_oid = {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(&workdir)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("rev-parse");
        String::from_utf8(output.stdout)
            .expect("utf8")
            .trim()
            .to_string()
    };
    let backend = {
        use gitcomet_core::services::GitBackend;
        gitcomet_git_gix::GixBackend
            .open(&workdir)
            .expect("open repo")
    };

    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let store_for_view = store.clone();
    let (view, cx) = cx.add_window_view(|window, cx| {
        crate::view::GitCometView::new(store_for_view, events, None, window, cx)
    });
    bind_app_keys_and_global_diff_fallback_for_test(cx);
    // Synchronize with the worker thread before relying on its state, the
    // same way `status_staging.rs`/`diff_marker_refresh.rs` do.
    crate::view::test_support::drain_store_worker(&view, cx);
    store.insert_repo_for_test(repo_id, backend);

    let file = |path: &str| crate::github::PullRequestFile {
        path: path.into(),
        additions: 1,
        deletions: 0,
    };
    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            let mut repo = opening_repo_state(repo_id, &workdir);
            repo.open = gitcomet_state::model::Loadable::Ready(());
            let next_state = Arc::new(AppState {
                repos: vec![repo],
                active_repo: Some(repo_id),
                sidebar_mode: gitcomet_state::model::SidebarMode::PullRequests,
                ..AppState::test_default()
            });
            push_test_state(this, next_state, cx);
            // `review_open_file`'s "by path" branch needs the PR's detail and
            // merge base already local, the same way `gh pr view` and the
            // commits fetch would leave them.
            this.pull_requests.repo_mut(repo_id).detail = PrLoad::Ready(Arc::new(
                crate::github::PullRequestDetail {
                    number: 88,
                    title: "Add a lockfile".into(),
                    body: String::new(),
                    url: String::new(),
                    author: "someone".into(),
                    created_at: String::new(),
                    head: "feat".into(),
                    head_oid: head_oid.clone(),
                    base: "main".into(),
                    base_oid: head_oid.clone(),
                    is_draft: false,
                    is_cross_repository: false,
                    state: "OPEN".into(),
                    review: None,
                    mergeable: None,
                    additions: 1,
                    deletions: 0,
                    changed_files: 1,
                    files: vec![file("Cargo.lock")],
                    checks: Default::default(),
                    check_runs: vec![],
                    conversation: vec![],
                    reviewers: vec![],
                    commits: vec![],
                },
            ));
            this.pull_requests.repo_mut(repo_id).diff_base = PrLoad::Ready(head_oid.clone());
            this.open_review_for_test(
                repo_id,
                88,
                vec!["Cargo.lock".to_string()],
                head_oid.clone(),
                cx,
            );
            this.seed_review_generated_files_for_test(["Cargo.lock".to_string()], cx);
        });
    });

    draw_and_drain_test_window(cx);

    let diff_target = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_window, app| {
            view.read(app)
                .main_pane
                .read(app)
                .active_repo()
                .and_then(|repo| repo.diff_state.diff_target.clone())
        })
    };
    let placeholder_shown = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_window, app| view.read(app).main_pane.read(app).review_generated_placeholder)
    };

    assert!(
        placeholder_shown(cx),
        "a generated file's placeholder should show before enter dismisses it"
    );
    assert_eq!(
        diff_target(cx),
        None,
        "the diff must not load while the placeholder is up"
    );

    cx.update(|window, app| {
        let sidebar_pane = view.read(app).sidebar_pane.clone();
        let focus = sidebar_pane.read(app).panel_focus_handle.clone();
        window.focus(&focus, app);
        let _ = window.draw(app);
    });
    cx.simulate_keystrokes("enter");
    draw_and_drain_test_window(cx);

    assert!(
        !placeholder_shown(cx),
        "enter should dismiss the placeholder"
    );
    // `Msg::SelectDiff` (dispatched by `enter`, above) reaches the store's own
    // worker thread, which processes its queue in order; round-tripping a
    // second, unrelated message through it (as `drain_store_worker` does) is
    // this codebase's way to know a prior dispatch already landed, the same
    // way `status_staging.rs` uses it after every interaction.
    crate::view::test_support::drain_store_worker(&view, cx);
    assert_eq!(
        store.snapshot().repos[0].diff_state.diff_target,
        Some(gitcomet_core::domain::DiffTarget::CommitRange {
            from_commit_id: gitcomet_core::domain::CommitId(head_oid.clone().into()),
            to_commit_id: Some(gitcomet_core::domain::CommitId(head_oid.into())),
            path: Some(std::path::PathBuf::from("Cargo.lock")),
        }),
        "enter should load the generated file's diff"
    );

    let _ = std::fs::remove_dir_all(&workdir);
}
