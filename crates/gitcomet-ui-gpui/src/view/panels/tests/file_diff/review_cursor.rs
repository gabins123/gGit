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
            pane.review_comment_scope = crate::view::panes::main::ReviewCommentScope::Historical;
            assert!(
                pane.review_selection_anchor(path)
                    .unwrap_err()
                    .contains("current head")
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

#[gpui::test]
fn review_markdown_block_cursor_moves_by_block_and_maps_back_to_a_source_line(
    cx: &mut gpui::TestAppContext,
) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        super::super::GitCometView::new(store, events, None, window, cx)
    });

    // A heading, then two paragraphs; only the second changed.
    let old_text = "# Title\n\nfirst\n\nsecond before\n";
    let new_text = "# Title\n\nfirst\n\nsecond after\n";
    let preview = crate::view::markdown_preview::build_markdown_diff_preview(old_text, new_text)
        .expect("fixture markdown should parse");
    let blocks = preview.inline_blocks.clone();
    assert!(
        blocks.len() >= 3,
        "fixture should parse into a heading and two paragraph blocks: {blocks:?}"
    );
    let block_rows: Vec<usize> = blocks.iter().map(|block| block.row_range().start).collect();
    // Independently derived from the same document, so this doesn't just
    // restate `markdown_preview_cursor_source_line`'s own arithmetic.
    let expected_line = |row_ix: usize| preview.inline.rows[row_ix].source_line_range.start as u32 + 1;
    let expected_side = |row_ix: usize| {
        if preview.inline_old.get(row_ix).copied().unwrap_or(false) {
            ReviewSide::Left
        } else {
            ReviewSide::Right
        }
    };
    let last_row = *block_rows.last().unwrap();
    let (last_line, last_side) = (expected_line(last_row), expected_side(last_row));

    cx.update(|_window, app| {
        let main_pane = view.read(app).main_pane.clone();
        main_pane.update(app, |pane, cx| {
            pane.diff_markdown.preview =
                gitcomet_state::model::Loadable::Ready(Arc::new(preview));
            pane.diff_view = DiffViewMode::Inline;
            pane.rendered_preview_modes
                .set(RenderedPreviewKind::Markdown, RenderedPreviewMode::Rendered);

            // With nothing focused, the first `j` goes to the first block, not
            // the first *changed* block; each `j` after lands on the next
            // block's row, not the next diff line.
            for &row in &block_rows {
                assert!(pane.review_move_markdown_block_cursor(1, cx));
                assert_eq!(pane.diff_selection_range, Some((row, row)));
            }
            let (side, line) = pane
                .markdown_preview_cursor_source_line()
                .expect("the last block has a source line");
            assert_eq!((side, line), (last_side, last_line));

            // Stops at the last block.
            assert!(!pane.review_move_markdown_block_cursor(1, cx));
            assert_eq!(pane.diff_selection_range, Some((last_row, last_row)));

            // `k` steps back by block too, not by row.
            for &row in block_rows[..block_rows.len() - 1].iter().rev() {
                assert!(pane.review_move_markdown_block_cursor(-1, cx));
                assert_eq!(pane.diff_selection_range, Some((row, row)));
            }
            assert_eq!(
                pane.diff_selection_range,
                Some((block_rows[0], block_rows[0]))
            );
            // Stops at the first block.
            assert!(!pane.review_move_markdown_block_cursor(-1, cx));
        });
    });
}

#[gpui::test]
fn review_markdown_cursor_in_split_view_falls_back_to_the_old_side_for_a_deleted_block(
    cx: &mut gpui::TestAppContext,
) {
    let (store, events) = AppStore::new_test(Arc::new(TestBackend));
    let (view, cx) = cx.add_window_view(|window, cx| {
        super::super::GitCometView::new(store, events, None, window, cx)
    });

    // A heading and a kept paragraph, then a paragraph removed entirely
    // (present only on the old side) — split view aligns it with padding on
    // the new side at the same row.
    let old_text = "# Title\n\nkept\n\ndeleted paragraph\n";
    let new_text = "# Title\n\nkept\n";
    let preview = crate::view::markdown_preview::build_markdown_diff_preview(old_text, new_text)
        .expect("fixture markdown should parse");

    let deleted_row_ix = preview
        .old
        .rows
        .iter()
        .position(|row| row.text.as_ref() == "deleted paragraph")
        .expect("the deleted paragraph should have a row on the old side");
    let expected_line = preview.old.rows[deleted_row_ix].source_line_range.start as u32 + 1;
    assert!(
        preview
            .new
            .rows
            .get(deleted_row_ix)
            .is_none_or(|row| row.is_alignment_padding()),
        "split view aligns the deleted block with padding on the new side, not a real row"
    );

    cx.update(|_window, app| {
        let main_pane = view.read(app).main_pane.clone();
        main_pane.update(app, |pane, _| {
            pane.diff_view = DiffViewMode::Split;
            pane.diff_markdown.preview =
                gitcomet_state::model::Loadable::Ready(Arc::new(preview));
            pane.diff_selection_anchor = Some(deleted_row_ix);
            pane.diff_selection_range = Some((deleted_row_ix, deleted_row_ix));

            let (side, line) = pane
                .markdown_preview_cursor_source_line()
                .expect("the deleted block has a source line on the old side");
            assert_eq!(side, ReviewSide::Left);
            assert_eq!(line, expected_line);
        });
    });
}
