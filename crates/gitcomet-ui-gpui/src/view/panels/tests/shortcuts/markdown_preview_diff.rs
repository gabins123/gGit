use super::*;

/// Alt+P flips the Preview/Text switch for a commit-range diff (the shape the
/// pull request "enter" diff, review mode, and a commit-range Details scope
/// all show a file's diff as), which `diff_target_rendered_preview_kind`
/// didn't recognize before this change.
#[gpui::test]
fn alt_p_toggles_preview_and_text_for_a_commit_range_markdown_diff(cx: &mut gpui::TestAppContext) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        crate::view::GitCometView::new(store, events, None, window, cx)
    });

    let repo_id = gitcomet_state::model::RepoId(9302);
    let workdir = std::env::temp_dir().join(format!(
        "gitcomet_ui_test_{}_alt_p_commit_range_markdown",
        std::process::id()
    ));
    let path = std::path::PathBuf::from("README.md");
    let old_text = "# Title\n\nbefore\n";
    let new_text = "# Title\n\nafter\n";

    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            let mut repo = opening_repo_state(repo_id, &workdir);
            repo.diff_state.diff_target = Some(gitcomet_core::domain::DiffTarget::CommitRange {
                from_commit_id: gitcomet_core::domain::CommitId("deadbeef".into()),
                to_commit_id: Some(gitcomet_core::domain::CommitId("f00dcafe".into())),
                path: Some(path.clone()),
            });
            repo.diff_state.diff_file = gitcomet_state::model::Loadable::Ready(Some(Arc::new(
                gitcomet_core::domain::FileDiffText::new(
                    path.clone(),
                    Some(old_text.to_string()),
                    Some(new_text.to_string()),
                ),
            )));
            let next_state = app_state_with_repo(repo, repo_id);
            push_test_state(this, next_state, cx);
        });
    });

    for _ in 0..3 {
        cx.update(|window, app| {
            let _ = window.draw(app);
        });
    }
    cx.run_until_parked();
    cx.update(|window, app| {
        let _ = window.draw(app);
    });

    wait_for_main_pane_condition(
        cx,
        &view,
        "commit-range markdown diff preview activation",
        |pane| pane.is_markdown_preview_active(),
        |pane| {
            format!(
                "diff_target={:?} markdown_preview_active={}",
                pane.active_repo()
                    .and_then(|repo| repo.diff_state.diff_target.clone()),
                pane.is_markdown_preview_active(),
            )
        },
    );

    cx.update(|_window, app| {
        let pane = view.read(app).main_pane.read(app);
        assert_eq!(
            pane.rendered_preview_modes
                .get(RenderedPreviewKind::Markdown),
            RenderedPreviewMode::Rendered,
            "expected a commit-range markdown diff to default to Preview mode"
        );
    });
    assert!(
        cx.debug_bounds("markdown_diff_view_toggle").is_some(),
        "expected the Preview/Text switch for a commit-range markdown diff"
    );

    cx.update(|_window, app| {
        crate::app::install_global_diff_shortcut_fallback_for_test(app);
    });
    focus_diff_panel(cx, &view);

    assert!(
        crate::view::is_diff_shortcut_candidate(
            &gpui::Keystroke::parse("alt-p").expect("valid chord")
        ),
        "alt-p must reach the diff shortcut table"
    );

    cx.simulate_keystrokes("alt-p");
    cx.update(|window, app| {
        let _ = window.draw(app);
    });
    cx.update(|_window, app| {
        let pane = view.read(app).main_pane.read(app);
        assert_eq!(
            pane.rendered_preview_modes
                .get(RenderedPreviewKind::Markdown),
            RenderedPreviewMode::Source,
            "alt-p should switch a commit-range markdown diff to Text"
        );
    });

    cx.simulate_keystrokes("alt-p");
    cx.update(|window, app| {
        let _ = window.draw(app);
    });
    cx.update(|_window, app| {
        let pane = view.read(app).main_pane.read(app);
        assert_eq!(
            pane.rendered_preview_modes
                .get(RenderedPreviewKind::Markdown),
            RenderedPreviewMode::Rendered,
            "alt-p should switch back to Preview"
        );
    });

    let _ = std::fs::remove_dir_all(&workdir);
}

/// SECURITY: a commit-range diff is how review mode and the pull request
/// "enter" diff show a file — content that can come from an untrusted fork's
/// pull request — so its rendered Markdown preview must never load remote
/// images automatically, regardless of the user's general preference.
#[gpui::test]
fn commit_range_markdown_diff_never_loads_remote_images_even_when_the_policy_allows_it(
    cx: &mut gpui::TestAppContext,
) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        crate::view::GitCometView::new(store, events, None, window, cx)
    });

    let repo_id = gitcomet_state::model::RepoId(9303);
    let workdir = std::env::temp_dir().join(format!(
        "gitcomet_ui_test_{}_commit_range_never_loads_remote_images",
        std::process::id()
    ));
    let path = std::path::PathBuf::from("README.md");
    let old_text = "# Title\n\n![pixel](https://evil.example/pixel.png)\n";
    let new_text = "# Title\n\n![pixel](https://evil.example/pixel.png)\n\nafter\n";

    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            let mut repo = opening_repo_state(repo_id, &workdir);
            repo.diff_state.diff_target = Some(gitcomet_core::domain::DiffTarget::CommitRange {
                from_commit_id: gitcomet_core::domain::CommitId("deadbeef".into()),
                to_commit_id: Some(gitcomet_core::domain::CommitId("f00dcafe".into())),
                path: Some(path.clone()),
            });
            repo.diff_state.diff_file = gitcomet_state::model::Loadable::Ready(Some(Arc::new(
                gitcomet_core::domain::FileDiffText::new(
                    path.clone(),
                    Some(old_text.to_string()),
                    Some(new_text.to_string()),
                ),
            )));
            let next_state = app_state_with_repo(repo, repo_id);
            push_test_state(this, next_state, cx);
            // The user's general preference says to load remote images
            // automatically; a commit-range diff must refuse anyway.
            this.main_pane.update(cx, |pane, _| {
                pane.remote_markdown_images.policy = RemoteMarkdownImagePolicy::AlwaysLoad;
            });
        });
    });

    for _ in 0..3 {
        cx.update(|window, app| {
            let _ = window.draw(app);
        });
    }
    cx.run_until_parked();
    cx.update(|window, app| {
        let _ = window.draw(app);
    });

    wait_for_main_pane_condition(
        cx,
        &view,
        "commit-range markdown diff preview activation",
        |pane| pane.is_markdown_preview_active(),
        |pane| format!("markdown_preview_active={}", pane.is_markdown_preview_active()),
    );

    cx.update(|_window, app| {
        let pane = view.read(app).main_pane.read(app);
        assert_eq!(
            pane.markdown_remote_image_access(None).policy,
            RemoteMarkdownImagePolicy::NeverLoad,
            "a commit-range diff must never auto-load remote images, whatever the general preference says"
        );
    });

    // A plain working-tree target still honors the general preference: this
    // isn't a blanket override, only a commit-range one.
    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            this.main_pane.update(cx, |pane, cx| {
                pane.remote_markdown_images.policy = RemoteMarkdownImagePolicy::AlwaysLoad;
                cx.notify();
            });
            let mut repo = opening_repo_state(repo_id, &workdir);
            repo.diff_state.diff_target = Some(gitcomet_core::domain::DiffTarget::WorkingTree {
                path: path.clone(),
                area: gitcomet_core::domain::DiffArea::Unstaged,
            });
            repo.diff_state.diff_file = gitcomet_state::model::Loadable::Ready(Some(Arc::new(
                gitcomet_core::domain::FileDiffText::new(
                    path.clone(),
                    Some(old_text.to_string()),
                    Some(new_text.to_string()),
                ),
            )));
            push_test_state(this, app_state_with_repo(repo, repo_id), cx);
        });
    });
    draw_and_drain_test_window(cx);
    cx.update(|_window, app| {
        let pane = view.read(app).main_pane.read(app);
        assert_eq!(
            pane.markdown_remote_image_access(None).policy,
            RemoteMarkdownImagePolicy::AlwaysLoad,
            "a working-tree diff should still honor the general preference"
        );
    });

    let _ = std::fs::remove_dir_all(&workdir);
}

/// In review mode's rendered Markdown preview, `j`/`k` step by rendered block
/// rather than by diff line, and `c` — which has no single line to comment on
/// in the preview — switches to Text instead of opening the comment box.
#[gpui::test]
fn review_mode_moves_by_block_in_markdown_preview_and_c_switches_to_text(
    cx: &mut gpui::TestAppContext,
) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        crate::view::GitCometView::new(store, events, None, window, cx)
    });
    bind_app_keys_and_global_diff_fallback_for_test(cx);
    cx.update(|_window, app| crate::app::bind_text_input_keys_for_test(app));

    let repo_id = gitcomet_state::model::RepoId(9401);
    let workdir = std::env::temp_dir().join(format!(
        "gitcomet_ui_test_{}_review_markdown_preview_block_nav",
        std::process::id()
    ));
    let path_str = "docs/preview.md";
    let path = std::path::PathBuf::from(path_str);
    let old_text = "# Title\n\nfirst\n\nsecond before\n";
    let new_text = "# Title\n\nfirst\n\nsecond after\n";

    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            let mut repo = opening_repo_state(repo_id, &workdir);
            repo.diff_state.diff_target = Some(gitcomet_core::domain::DiffTarget::CommitRange {
                from_commit_id: gitcomet_core::domain::CommitId("base".into()),
                to_commit_id: Some(gitcomet_core::domain::CommitId("h1".into())),
                path: Some(path.clone()),
            });
            repo.diff_state.diff_file = gitcomet_state::model::Loadable::Ready(Some(Arc::new(
                gitcomet_core::domain::FileDiffText::new(
                    path.clone(),
                    Some(old_text.to_string()),
                    Some(new_text.to_string()),
                ),
            )));
            let next_state = Arc::new(AppState {
                repos: vec![repo],
                active_repo: Some(repo_id),
                // `active_review` (and so `handle_review_key`) only sees the
                // review while the Pull requests tab is showing.
                sidebar_mode: gitcomet_state::model::SidebarMode::PullRequests,
                ..AppState::test_default()
            });
            push_test_state(this, next_state, cx);
            this.main_pane
                .update(cx, |pane, _| pane.diff_view = DiffViewMode::Inline);
            this.open_review_for_test(repo_id, 42, vec![path_str.to_string()], "h1", cx);
        });
    });

    for _ in 0..3 {
        cx.update(|window, app| {
            let _ = window.draw(app);
        });
    }
    cx.run_until_parked();

    wait_for_main_pane_condition(
        cx,
        &view,
        "review's markdown diff preview activation",
        |pane| pane.is_markdown_preview_active(),
        |pane| {
            format!(
                "diff_target={:?} markdown_preview_active={}",
                pane.active_repo()
                    .and_then(|repo| repo.diff_state.diff_target.clone()),
                pane.is_markdown_preview_active(),
            )
        },
    );

    // Autoscroll-to-first-change parks the cursor on the first *changed*
    // block while the fixture settles (an intentional, unrelated feature —
    // review mode opens a file with the cursor already on its first change).
    // Start from a clean slate so the assertions below test "the first `j`
    // reaches the first block", not "the first `j` after autoscroll".
    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            this.main_pane.update(cx, |pane, cx| {
                pane.diff_selection_anchor = None;
                pane.diff_selection_range = None;
                cx.notify();
            });
        });
    });

    focus_diff_panel(cx, &view);

    let press = |cx: &mut gpui::VisualTestContext, keys: &str| {
        cx.simulate_keystrokes(keys);
        draw_and_drain_test_window(cx);
    };

    // The expected stops are read from the same preview the pane renders,
    // rather than hardcoded, so a change to how the fixture parses doesn't
    // make this test lie about what it's checking.
    let (first_block_row, second_block_row) = cx.update(|_window, app| {
        let pane = view.read(app).main_pane.read(app);
        let gitcomet_state::model::Loadable::Ready(preview) = &pane.diff_markdown.preview else {
            panic!("markdown diff preview should be ready by now");
        };
        let blocks = &preview.inline_blocks;
        assert!(
            blocks.len() >= 2,
            "fixture should parse into at least 2 rendered blocks: {blocks:?}"
        );
        (blocks[0].row_range().start, blocks[1].row_range().start)
    });
    assert_ne!(
        first_block_row, second_block_row,
        "the fixture should have at least two distinct blocks to move between"
    );

    press(cx, "j");
    cx.update(|_window, app| {
        let pane = view.read(app).main_pane.read(app);
        assert_eq!(
            pane.diff_selection_range,
            Some((first_block_row, first_block_row)),
            "the first j should land on the first block"
        );
    });

    press(cx, "j");
    cx.update(|_window, app| {
        let pane = view.read(app).main_pane.read(app);
        assert_eq!(
            pane.diff_selection_range,
            Some((second_block_row, second_block_row)),
            "the second j should land on the next block, not the next row"
        );
    });

    press(cx, "c");
    cx.update(|_window, app| {
        let pane = view.read(app).main_pane.read(app);
        assert_eq!(
            pane.rendered_preview_modes
                .get(RenderedPreviewKind::Markdown),
            RenderedPreviewMode::Source,
            "c in the rendered preview should switch to Text instead of opening a comment"
        );
    });

    let _ = std::fs::remove_dir_all(&workdir);
}
