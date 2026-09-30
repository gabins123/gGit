use super::*;

type View = gpui::Entity<crate::view::GitCometView>;

/// The review Files list's flat/tree toggle (`` ` ``), folder rows, and how
/// `j`/`k`, `]`/`[` and `space` walk them — mirroring the Changes list's
/// flat/tree layout (`view/rows/file_list`), but review mode also lets
/// `j`/`k` rest a cursor on a folder row for `enter` to toggle, which the
/// other changed-file lists don't need (their folders only toggle by click).
///
/// Files: `z.md`, `src/a.rs`, `src/b.rs`, `m.md`. Path-ascending tree order
/// groups directories first, so the tree lists `src` (with `a.rs`, `b.rs`)
/// before the two root files, `z.md` then `m.md` — genuinely different from
/// the flat, file-list order (`z.md`, `a.rs`, `b.rs`, `m.md`), which is what
/// lets these tests tell "follows tree order" apart from "follows raw index".
fn review_files() -> Vec<String> {
    vec![
        "z.md".to_string(),
        "src/a.rs".to_string(),
        "src/b.rs".to_string(),
        "m.md".to_string(),
    ]
}

fn fixture(cx: &mut gpui::TestAppContext) -> (View, &mut gpui::VisualTestContext) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        crate::view::GitCometView::new(store, events, None, window, cx)
    });
    bind_app_keys_and_global_diff_fallback_for_test(cx);

    let repo_id = gitcomet_state::model::RepoId(9601);
    let workdir = std::env::temp_dir().join(format!(
        "gitcomet_ui_test_{}_review_files_tree",
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
            this.open_review_for_test(repo_id, 91, review_files(), "h1", cx);
        });
    });
    draw_and_drain_test_window(cx);

    cx.update(|window, app| {
        let sidebar_pane = view.read(app).sidebar_pane.clone();
        let focus = sidebar_pane.read(app).panel_focus_handle.clone();
        window.focus(&focus, app);
        let _ = window.draw(app);
    });
    (view, cx)
}

fn file_ix(cx: &mut gpui::VisualTestContext, view: &View) -> usize {
    cx.update(|_window, app| {
        view.read(app)
            .active_review()
            .map(|review| review.file_ix)
            .expect("reviewing")
    })
}

#[gpui::test]
fn backtick_toggles_tree_and_shows_a_folder_row(cx: &mut gpui::TestAppContext) {
    let (_view, cx) = fixture(cx);

    // Flat by default: every file is its own row, no folder row.
    assert!(cx.debug_bounds("review_file_0").is_some(), "z.md (flat)");
    assert!(
        cx.debug_bounds("review_file_dir_0").is_none(),
        "no folder in flat layout"
    );

    cx.simulate_keystrokes("`");
    draw_and_drain_test_window(cx);
    assert!(
        cx.debug_bounds("review_file_dir_0").is_some(),
        "tree layout should show the src folder as the first row"
    );
    // Every file is still there, `src`'s children included.
    for ix in 0..4 {
        let selector: &'static str = Box::leak(format!("review_file_{ix}").into_boxed_str());
        assert!(
            cx.debug_bounds(selector).is_some(),
            "file {ix} should still be listed in tree layout"
        );
    }

    cx.simulate_keystrokes("`");
    draw_and_drain_test_window(cx);
    assert!(
        cx.debug_bounds("review_file_dir_0").is_none(),
        "` again should return to flat layout"
    );
}

#[gpui::test]
fn enter_on_a_folder_row_collapses_and_expands_it(cx: &mut gpui::TestAppContext) {
    let (view, cx) = fixture(cx);

    cx.simulate_keystrokes("`");
    draw_and_drain_test_window(cx);

    // z.md (file 0) opens at row 3, under the src folder (row 0) and its two
    // children (rows 1-2); `k` three times walks the cursor up to the folder,
    // opening b.rs then a.rs along the way (a file row's `j`/`k` still opens
    // it, exactly as flat layout always has).
    cx.simulate_keystrokes("k k k");
    draw_and_drain_test_window(cx);
    assert_eq!(
        file_ix(cx, &view),
        1,
        "k over file rows should open them, ending on a.rs"
    );
    // The third `k` should now rest on the folder row rather than opening
    // another file — this is exactly the state `directory_row`'s `selected`
    // paints with the same background a file row uses for the open file
    // (AGENTS.md keyboard-first: the cursor must be visible, not just live).
    let dir_cursor = cx.update(|_window, app| {
        view.read(app)
            .active_review()
            .and_then(|review| review.sidebar_dir_cursor.clone())
    });
    assert_eq!(
        dir_cursor.as_deref(),
        Some(std::path::Path::new("src")),
        "k should rest the keyboard cursor on the src folder row"
    );

    cx.simulate_keystrokes("enter");
    draw_and_drain_test_window(cx);
    assert!(
        cx.debug_bounds("review_file_1").is_none(),
        "a.rs should be hidden once its folder collapses"
    );
    assert!(
        cx.debug_bounds("review_file_2").is_none(),
        "b.rs should be hidden once its folder collapses"
    );
    assert!(
        cx.debug_bounds("review_file_0").is_some(),
        "z.md, outside the folder, should stay visible"
    );

    cx.simulate_keystrokes("enter");
    draw_and_drain_test_window(cx);
    assert!(
        cx.debug_bounds("review_file_1").is_some(),
        "enter again should expand the folder back open"
    );
}

#[gpui::test]
fn next_previous_file_follow_tree_order_and_skip_folders(cx: &mut gpui::TestAppContext) {
    let (view, cx) = fixture(cx);

    cx.simulate_keystrokes("`");
    draw_and_drain_test_window(cx);

    // Tree order: a.rs (1), b.rs (2), z.md (0), m.md (3) — `src`'s files
    // before the root files. Open a.rs and step forward with `]` twice.
    cx.update(|_window, app| {
        view.update(app, |this, cx| this.review_open_file(1, cx));
    });
    draw_and_drain_test_window(cx);

    cx.simulate_keystrokes("]");
    draw_and_drain_test_window(cx);
    assert_eq!(file_ix(cx, &view), 2, "] from a.rs should reach b.rs");

    cx.simulate_keystrokes("]");
    draw_and_drain_test_window(cx);
    assert_eq!(
        file_ix(cx, &view),
        0,
        "] from b.rs should continue into z.md, past the folder, in tree order"
    );

    cx.simulate_keystrokes("[");
    draw_and_drain_test_window(cx);
    assert_eq!(file_ix(cx, &view), 2, "[ should step back the same way");

    // The same last step, in flat layout, does not wrap: raw file order ends
    // at m.md (index 3), so `]` from b.rs (index 2) only reaches m.md, never
    // z.md (index 0) — the previous assertion is therefore about tree order
    // specifically, not just "] always finds something".
    cx.simulate_keystrokes("`");
    draw_and_drain_test_window(cx);
    cx.update(|_window, app| {
        view.update(app, |this, cx| this.review_open_file(2, cx));
    });
    draw_and_drain_test_window(cx);
    cx.simulate_keystrokes("]");
    draw_and_drain_test_window(cx);
    assert_eq!(
        file_ix(cx, &view),
        3,
        "] from b.rs in flat layout reaches m.md, not z.md"
    );
}

#[gpui::test]
fn space_marks_viewed_and_jumps_to_next_unviewed_in_tree_order(cx: &mut gpui::TestAppContext) {
    let (view, cx) = fixture(cx);

    cx.simulate_keystrokes("`");
    draw_and_drain_test_window(cx);

    // z.md is open (file 0). In tree order (a.rs, b.rs, z.md, m.md), the
    // next unviewed file after z.md is m.md — not a.rs, which raw file-index
    // order (used in flat layout) would pick instead.
    cx.simulate_keystrokes("space");
    draw_and_drain_test_window(cx);
    let (viewed, next) = cx.update(|_window, app| {
        let review = view.read(app).active_review().expect("reviewing");
        (review.draft.viewed.contains("z.md"), review.file_ix)
    });
    assert!(viewed, "space should mark the open file viewed");
    assert_eq!(
        next, 3,
        "space should open the next unviewed file in tree order (m.md), not raw index order (a.rs)"
    );
}

#[gpui::test]
fn backtick_is_inert_while_the_filter_input_has_focus(cx: &mut gpui::TestAppContext) {
    let (_view, cx) = fixture(cx);

    cx.simulate_keystrokes("/");
    draw_and_drain_test_window(cx);
    cx.simulate_keystrokes("`");
    draw_and_drain_test_window(cx);

    assert!(
        cx.debug_bounds("review_file_dir_0").is_none(),
        "` while the filter box has focus must not toggle the layout"
    );
}
