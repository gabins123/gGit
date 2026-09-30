use super::*;

/// Alt+V cycles Side by side -> Swipe -> Onion skin -> Side by side, and
/// `,`/`.` move the Swipe divider or step the Onion opacity while that mode
/// is active. In Side by side neither key has anything to move.
#[gpui::test]
fn alt_v_cycles_image_diff_mode_and_comma_dot_adjust_the_active_mode(
    cx: &mut gpui::TestAppContext,
) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        crate::view::GitCometView::new(store, events, None, window, cx)
    });
    bind_app_keys_and_global_diff_fallback_for_test(cx);

    let repo_id = gitcomet_state::model::RepoId(9501);
    let workdir = std::env::temp_dir().join(format!(
        "gitcomet_ui_test_{}_image_diff_modes",
        std::process::id()
    ));
    let path = std::path::PathBuf::from("assets/gitcomet.png");
    let old_bytes =
        include_bytes!("../../../../../../../assets/linux/hicolor/32x32/apps/gitcomet.png");
    let new_bytes =
        include_bytes!("../../../../../../../assets/linux/hicolor/48x48/apps/gitcomet.png");

    seed_file_image_diff_state_with_rev(
        cx,
        &view,
        repo_id,
        &workdir,
        &path,
        1,
        Some(old_bytes),
        Some(new_bytes),
    );
    wait_for_file_image_diff_cache(cx, &view, "image diff cache ready", |pane| {
        pane.file_image_diff_cache_old.is_some() && pane.file_image_diff_cache_new.is_some()
    });

    focus_diff_panel(cx, &view);

    let press = |cx: &mut gpui::VisualTestContext, keys: &str| {
        cx.simulate_keystrokes(keys);
        draw_and_drain_test_window(cx);
    };

    cx.update(|_window, app| {
        assert_eq!(
            view.read(app).main_pane.read(app).image_diff_mode,
            ImageDiffMode::SideBySide,
            "Side by side is the default mode"
        );
    });
    // In Side by side, `,`/`.` have no divider or opacity to move.
    let (swipe_before, onion_before) = cx.update(|_window, app| {
        let pane = view.read(app).main_pane.read(app);
        (pane.image_diff_swipe_position, pane.image_diff_onion_opacity)
    });
    press(cx, ".");
    press(cx, ",");
    cx.update(|_window, app| {
        let pane = view.read(app).main_pane.read(app);
        assert_eq!(pane.image_diff_swipe_position, swipe_before);
        assert_eq!(pane.image_diff_onion_opacity, onion_before);
    });

    press(cx, "alt-v");
    cx.update(|_window, app| {
        assert_eq!(
            view.read(app).main_pane.read(app).image_diff_mode,
            ImageDiffMode::Swipe,
            "alt-v should move to Swipe"
        );
    });
    assert!(
        cx.debug_bounds("diff_image_swipe").is_some(),
        "expected the Swipe overlay to render"
    );

    let before = cx.update(|_window, app| {
        view.read(app).main_pane.read(app).image_diff_swipe_position
    });
    press(cx, ".");
    cx.update(|_window, app| {
        let after = view.read(app).main_pane.read(app).image_diff_swipe_position;
        assert!(after > before, "'.' should move the swipe divider right");
    });
    let before = cx.update(|_window, app| {
        view.read(app).main_pane.read(app).image_diff_swipe_position
    });
    press(cx, ",");
    cx.update(|_window, app| {
        let after = view.read(app).main_pane.read(app).image_diff_swipe_position;
        assert!(after < before, "',' should move the swipe divider left");
    });

    press(cx, "alt-v");
    cx.update(|_window, app| {
        assert_eq!(
            view.read(app).main_pane.read(app).image_diff_mode,
            ImageDiffMode::OnionSkin,
            "alt-v should move to Onion skin"
        );
    });
    assert!(
        cx.debug_bounds("diff_image_onion").is_some(),
        "expected the Onion skin overlay to render"
    );

    let before = cx.update(|_window, app| {
        view.read(app).main_pane.read(app).image_diff_onion_opacity
    });
    press(cx, ".");
    cx.update(|_window, app| {
        let after = view.read(app).main_pane.read(app).image_diff_onion_opacity;
        assert!(after > before, "'.' should raise the onion opacity");
    });
    let before = cx.update(|_window, app| {
        view.read(app).main_pane.read(app).image_diff_onion_opacity
    });
    press(cx, ",");
    cx.update(|_window, app| {
        let after = view.read(app).main_pane.read(app).image_diff_onion_opacity;
        assert!(after < before, "',' should lower the onion opacity");
    });

    press(cx, "alt-v");
    cx.update(|_window, app| {
        assert_eq!(
            view.read(app).main_pane.read(app).image_diff_mode,
            ImageDiffMode::SideBySide,
            "alt-v should cycle back to Side by side"
        );
    });

    let _ = std::fs::remove_dir_all(&workdir);
}

/// Alt+V, `,` and `.` are diff-view chords like Alt+P: inert while a text
/// input owns the keyboard (the diff search box here), per AGENTS.md's
/// keyboard-first rule that a shortcut stays out of the way while typing.
#[gpui::test]
fn image_diff_mode_keys_are_inert_while_the_diff_search_input_is_focused(
    cx: &mut gpui::TestAppContext,
) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        crate::view::GitCometView::new(store, events, None, window, cx)
    });
    bind_app_keys_and_global_diff_fallback_for_test(cx);
    cx.update(|_window, app| crate::app::bind_text_input_keys_for_test(app));

    let repo_id = gitcomet_state::model::RepoId(9503);
    let workdir = std::env::temp_dir().join(format!(
        "gitcomet_ui_test_{}_image_diff_modes_inert_while_typing",
        std::process::id()
    ));
    let path = std::path::PathBuf::from("assets/gitcomet.png");
    let old_bytes =
        include_bytes!("../../../../../../../assets/linux/hicolor/32x32/apps/gitcomet.png");
    let new_bytes =
        include_bytes!("../../../../../../../assets/linux/hicolor/48x48/apps/gitcomet.png");

    seed_file_image_diff_state_with_rev(
        cx,
        &view,
        repo_id,
        &workdir,
        &path,
        1,
        Some(old_bytes),
        Some(new_bytes),
    );
    wait_for_file_image_diff_cache(cx, &view, "image diff cache ready", |pane| {
        pane.file_image_diff_cache_old.is_some() && pane.file_image_diff_cache_new.is_some()
    });

    // Focus the diff search input directly, the same field the Alt+E/Alt+P
    // text-input carve-out checks.
    cx.update(|window, app| {
        let main_pane = view.read(app).main_pane.clone();
        let focus = main_pane
            .read(app)
            .diff_search_input
            .read(app)
            .focus_handle();
        window.focus(&focus, app);
        let _ = window.draw(app);
    });

    let mode_before = cx.update(|_window, app| view.read(app).main_pane.read(app).image_diff_mode);
    cx.simulate_keystrokes("alt-v");
    draw_and_drain_test_window(cx);
    cx.simulate_keystrokes(".");
    draw_and_drain_test_window(cx);
    cx.update(|_window, app| {
        assert_eq!(
            view.read(app).main_pane.read(app).image_diff_mode,
            mode_before,
            "alt-v must not fire while the diff search input is focused"
        );
    });

    let _ = std::fs::remove_dir_all(&workdir);
}

/// The image diff already loads for `CommitRange` targets at the state layer
/// (`selected_diff_load_plan`); this confirms it also *renders* through the
/// pull request "enter" diff / review-mode shape rather than only for a plain
/// working-tree or commit target.
#[gpui::test]
fn image_diff_renders_for_a_commit_range_target_and_in_review_mode(cx: &mut gpui::TestAppContext) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        crate::view::GitCometView::new(store, events, None, window, cx)
    });

    let repo_id = gitcomet_state::model::RepoId(9502);
    let workdir = std::env::temp_dir().join(format!(
        "gitcomet_ui_test_{}_image_diff_review_mode",
        std::process::id()
    ));
    let path_str = "assets/gitcomet.png";
    let path = std::path::PathBuf::from(path_str);
    let old_bytes =
        include_bytes!("../../../../../../../assets/linux/hicolor/32x32/apps/gitcomet.png");
    let new_bytes =
        include_bytes!("../../../../../../../assets/linux/hicolor/48x48/apps/gitcomet.png");

    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            let mut repo = opening_repo_state(repo_id, &workdir);
            repo.diff_state.diff_target = Some(gitcomet_core::domain::DiffTarget::CommitRange {
                from_commit_id: gitcomet_core::domain::CommitId("base".into()),
                to_commit_id: Some(gitcomet_core::domain::CommitId("h1".into())),
                path: Some(path.clone()),
            });
            repo.diff_state.diff_file_rev = 1;
            repo.diff_state.diff_file_image = gitcomet_state::model::Loadable::Ready(Some(
                Arc::new(gitcomet_core::domain::FileDiffImage {
                    path: path.clone(),
                    old: Some(old_bytes.to_vec()),
                    new: Some(new_bytes.to_vec()),
                }),
            ));
            let next_state = Arc::new(AppState {
                repos: vec![repo],
                active_repo: Some(repo_id),
                // `active_review` (and so review mode) only sees the review
                // while the Pull requests tab is showing.
                sidebar_mode: gitcomet_state::model::SidebarMode::PullRequests,
                ..AppState::test_default()
            });
            push_test_state(this, next_state, cx);
            this.open_review_for_test(repo_id, 77, vec![path_str.to_string()], "h1", cx);
        });
    });

    for _ in 0..3 {
        cx.update(|window, app| {
            let _ = window.draw(app);
        });
    }
    cx.run_until_parked();

    wait_for_file_image_diff_cache(cx, &view, "review-mode image diff cache", |pane| {
        pane.file_image_diff_cache_old.is_some() && pane.file_image_diff_cache_new.is_some()
    });

    assert!(
        cx.debug_bounds("diff_image_left").is_some(),
        "expected the old side to render for a CommitRange image diff in review mode"
    );
    assert!(
        cx.debug_bounds("diff_image_right").is_some(),
        "expected the new side to render for a CommitRange image diff in review mode"
    );

    let _ = std::fs::remove_dir_all(&workdir);
}
