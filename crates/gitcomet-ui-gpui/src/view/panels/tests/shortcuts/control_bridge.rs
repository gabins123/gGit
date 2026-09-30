//! The dev control bridge's command executor against a real `GitCometView`:
//! `keys` drives the panel keys, `state` reads the result back.

use super::*;

fn state_json(result: &str) -> serde_json::Value {
    let mut lines = result.lines();
    assert_eq!(lines.next(), Some("ok"), "{result}");
    serde_json::from_str(lines.next().expect("payload line")).expect("state is JSON")
}

#[gpui::test]
fn bridge_keys_change_what_state_reports(cx: &mut gpui::TestAppContext) {
    let _guard = lock_visual_test();
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        crate::view::GitCometView::new(store, events, None, window, cx)
    });
    let workdir = std::env::temp_dir().join("gitcomet_ui_test_control_bridge");
    apply_state(
        cx,
        &view,
        app_state_with_active_repo(opening_repo_state(RepoId(9701), &workdir)),
    );
    bind_app_keys_and_global_diff_fallback_for_test(cx);
    cx.update(|_window, app| crate::app::bind_text_input_keys_for_test(app));
    draw_and_drain_test_window(cx);
    let window = cx.windows()[0];

    let before = state_json(&crate::control_bridge::run_for_test(window, cx, "state"));
    assert_eq!(before["sidebar_mode"], "branches");
    assert_eq!(before["review_active"], false);
    assert_eq!(before["file_list_layout"], "flat");
    assert_eq!(before["selected_pr"], serde_json::Value::Null);
    assert_eq!(before["repo_workdir"], workdir.display().to_string());

    // `1` focuses the sidebar, `]` steps to the next sidebar tab.
    let keys = crate::control_bridge::run_for_test(window, cx, "keys 1 ]");
    assert_eq!(keys, "ok dispatched 2\n");
    // The reducer runs on its own thread; the live bridge waits between keys.
    wait_until(cx, "Files sidebar mode", |cx| {
        cx.update(|_window, app| {
            view.read(app).store.snapshot().sidebar_mode
                == gitcomet_state::model::SidebarMode::Files
        })
    });
    sync_store_snapshot(cx, &view);

    let after = state_json(&crate::control_bridge::run_for_test(window, cx, "state"));
    assert_eq!(after["focused_panel"], "sidebar");
    assert_eq!(after["sidebar_mode"], "files");

    // Bad input is an error result, never a panic or a dispatch.
    for (line, prefix) in [
        ("keys a-b", "error: invalid keystroke 'a-b'"),
        ("keys 1 a-b", "error: invalid keystroke 'a-b'"),
        ("dance", "error: unknown command 'dance'"),
        ("", "error: empty command"),
    ] {
        let result = crate::control_bridge::run_for_test(window, cx, line);
        assert!(result.starts_with(prefix), "{line:?} -> {result}");
    }
    sync_store_snapshot(cx, &view);
    let unchanged = state_json(&crate::control_bridge::run_for_test(window, cx, "state"));
    assert_eq!(unchanged["sidebar_mode"], "files");

    // The test platform has no native window to capture; that must surface as
    // an error result, not a panic.
    let shot = crate::control_bridge::run_for_test(window, cx, "screenshot never-written.png");
    assert!(shot.starts_with("error: "), "{shot}");
    assert!(
        shot.contains("Win32 handle") || shot.contains("only implemented on Windows"),
        "{shot}"
    );
    assert!(!Path::new("never-written.png").exists());
}
