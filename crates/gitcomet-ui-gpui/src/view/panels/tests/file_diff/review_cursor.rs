use super::*;
use crate::github::ReviewSide;

#[gpui::test]
fn review_cursor_selects_comments_and_marks_lines(cx: &mut gpui::TestAppContext) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        super::super::GitCometView::new(store, events, None, window, cx)
    });

    let old_text: String = (1..=12).map(|n| format!("line {n}\n")).collect();
    let new_text = old_text.replace("line 6\n", "line six\n");
    let path = PathBuf::from("src/review.rs");
    push_regular_diff_content_mode_state(
        cx,
        &view,
        gitcomet_state::model::RepoId(9310),
        "review_cursor",
        path,
        "\
diff --git a/src/review.rs b/src/review.rs
index 1111111..2222222 100644
--- a/src/review.rs
+++ b/src/review.rs
@@ -3,7 +3,7 @@
 line 3
 line 4
 line 5
-line 6
+line six
 line 7
 line 8
 line 9
"
        .to_string(),
        old_text,
        new_text,
    );
    cx.update(|_window, app| {
        view.update(app, |this, cx| {
            this.set_diff_content_mode(DiffContentMode::Full, cx);
            this.main_pane.update(cx, |pane, _| {
                pane.diff_view = DiffViewMode::Inline;
            });
            this.set_diff_word_wrap(false, cx);
        });
    });
    wait_for_main_pane_condition(
        cx,
        &view,
        "full inline file diff ready for review cursor",
        |pane| {
            pane.file_diff_cache_inflight.is_none()
                && pane.is_file_diff_view_active()
                && pane.diff_visible_len() == 13
        },
        |pane| {
            format!(
                "cache_inflight={:?} file_active={} visible_len={}",
                pane.file_diff_cache_inflight,
                pane.is_file_diff_view_active(),
                pane.diff_visible_len(),
            )
        },
    );
    draw_and_drain_test_window(cx);

    // Rows: 0..=4 context 1..=5, 5 removes old 6, 6 adds new 6, 7..=12 context 7..=12.
    cx.update(|_window, app| {
        let main_pane = view.read(app).main_pane.clone();
        main_pane.update(app, |pane, cx| {
            let path = "src/review.rs";
            pane.review_active = true;

            assert!(pane.review_cursor_to_start(cx));
            assert_eq!(pane.diff_selection_range, Some((5, 5)));
            assert!(pane.review_move_cursor(1, true, cx));
            assert!(pane.review_move_cursor(1, true, cx));
            assert_eq!(pane.diff_selection_anchor, Some(5));
            assert_eq!(pane.diff_selection_range, Some((5, 7)));
            let anchor = pane
                .review_selection_anchor(path)
                .expect("range is commentable");
            assert_eq!(anchor.path, path);
            assert_eq!((anchor.side, anchor.line), (ReviewSide::Right, 7));
            assert_eq!(anchor.start, Some((ReviewSide::Left, 6)));

            // Plain moves collapse the range onto the head; Esc keeps the head.
            assert!(pane.review_move_cursor(-1, false, cx));
            assert_eq!(pane.diff_selection_range, Some((6, 6)));
            assert!(pane.review_move_cursor(-1, true, cx));
            assert!(pane.review_collapse_selection(cx));
            assert_eq!(pane.diff_selection_range, Some((5, 5)));

            // New line 9 is the last of the 3 context lines after the change.
            assert!(pane.review_jump_to(ReviewSide::Right, 9, cx));
            assert_eq!(pane.diff_selection_range, Some((9, 9)));
            let anchor = pane.review_selection_anchor(path).expect("hunk context");
            assert_eq!(
                (anchor.side, anchor.line, anchor.start),
                (ReviewSide::Right, 9, None)
            );

            assert!(pane.review_jump_to(ReviewSide::Right, 12, cx));
            assert_eq!(pane.diff_selection_range, Some((12, 12)));
            assert!(pane.review_selection_anchor(path).is_err());
            assert!(
                !pane.review_move_cursor(1, false, cx),
                "stops at the last row"
            );

            assert!(pane.review_jump_to(ReviewSide::Left, 6, cx));
            assert_eq!(pane.diff_selection_range, Some((5, 5)));

            // Ending on a removed line counts in old numbers from the start.
            assert!(pane.review_jump_to(ReviewSide::Right, 5, cx));
            assert!(pane.review_move_cursor(1, true, cx));
            let anchor = pane
                .review_selection_anchor(path)
                .expect("context into removed");
            assert_eq!(
                (anchor.side, anchor.line, anchor.start),
                (ReviewSide::Left, 6, Some((ReviewSide::Left, 5)))
            );
            // One row past the hunk context refuses the whole range.
            assert!(pane.review_jump_to(ReviewSide::Right, 9, cx));
            assert!(pane.review_move_cursor(1, true, cx));
            assert!(pane.review_selection_anchor(path).is_err());
            // After a change jump only the anchor is left; it is still the cursor.
            pane.diff_selection_anchor = Some(6);
            pane.diff_selection_range = None;
            let anchor = pane.review_selection_anchor(path).expect("anchor only");
            assert_eq!(
                (anchor.side, anchor.line, anchor.start),
                (ReviewSide::Right, 6, None)
            );

            // t/T: step between thread lines, either side.
            let threads = [(ReviewSide::Right, 9), (ReviewSide::Left, 6)];
            assert!(pane.review_jump_to(ReviewSide::Right, 7, cx));
            assert!(pane.review_step_to(&threads, 1, cx));
            assert_eq!(pane.diff_selection_range, Some((9, 9)));
            assert!(!pane.review_step_to(&threads, 1, cx), "none after the last");
            assert!(pane.review_step_to(&threads, -1, cx));
            assert_eq!(pane.diff_selection_range, Some((5, 5)));
            assert!(
                !pane.review_step_to(&threads, -1, cx),
                "none before the first"
            );
            let row = pane.review_cursor_row().expect("a row");
            assert_eq!((row.old_line, row.new_line), (Some(6), None));

            // alt+s: the new side of the selected lines, removed ones skipped.
            assert!(pane.review_jump_to(ReviewSide::Right, 5, cx));
            assert!(pane.review_move_cursor(2, true, cx));
            assert_eq!(
                pane.review_selection_new_text().as_deref(),
                Some(
                    "line 5
line six"
                )
            );
            // Ending on a removed line has no new text to suggest over.
            assert!(pane.review_jump_to(ReviewSide::Left, 6, cx));
            assert_eq!(pane.review_selection_new_text(), None);

            assert_eq!(pane.review_mark_side(7), None);
            pane.review_marks.insert((ReviewSide::Right, 7));
            pane.review_marks.insert((ReviewSide::Left, 6));
            assert_eq!(pane.review_mark_side(7), Some(ReviewSide::Right));
            assert_eq!(pane.review_mark_side(5), Some(ReviewSide::Left));
            assert_eq!(pane.review_mark_side(6), None);
            // One line, all three kinds: yours, then a thread, then Codex.
            use crate::view::panes::main::ReviewMark;
            pane.review_thread_marks.insert((ReviewSide::Right, 7));
            pane.review_suggestion_marks.insert((ReviewSide::Right, 7));
            assert_eq!(
                pane.review_mark(7),
                Some((ReviewSide::Right, ReviewMark::Pending))
            );
            pane.review_marks.remove(&(ReviewSide::Right, 7));
            assert_eq!(
                pane.review_mark(7),
                Some((ReviewSide::Right, ReviewMark::Thread))
            );
            pane.review_thread_marks.clear();
            assert_eq!(
                pane.review_mark(7),
                Some((ReviewSide::Right, ReviewMark::Suggestion))
            );

            pane.review_active = false;
            assert_eq!(pane.review_mark_side(7), None);
        });
    });
}
