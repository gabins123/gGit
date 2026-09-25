//! Review mode's keyboard line cursor over the diff, and the rows that carry a
//! pending review comment. The cursor is the ordinary row selection: the anchor
//! is its fixed end and the head the end that moves.

use super::*;
use crate::github::{ReviewAnchor, ReviewSide};
use gitcomet_core::domain::DiffLineKind;
use gitcomet_core::file_diff::{FileDiffRow, FileDiffRowKind};

/// GitHub's hunk context: it only takes comments on changed lines and this
/// many rows around them.
const REVIEW_HUNK_CONTEXT_ROWS: usize = 3;

const REVIEW_NO_LINE: &str = "Move to a line of the diff first.";
const REVIEW_OUTSIDE_HUNK: &str =
    "GitHub only takes comments on changed lines and the 3 lines around them.";
const REVIEW_ACROSS_GAP: &str = "A comment's lines have to be one unbroken run of the diff.";

/// A diff row a review comment can sit on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::view) struct ReviewRow {
    pub(in crate::view) old_line: Option<u32>,
    pub(in crate::view) new_line: Option<u32>,
    pub(in crate::view) changed: bool,
}

impl ReviewRow {
    fn from_annotated(line: AnnotatedDiffLine) -> Option<Self> {
        let changed = match line.kind {
            DiffLineKind::Add | DiffLineKind::Remove => true,
            DiffLineKind::Context => false,
            DiffLineKind::Header | DiffLineKind::Hunk => return None,
        };
        Self::new(line.old_line, line.new_line, changed)
    }

    fn from_split(row: &FileDiffRow) -> Option<Self> {
        Self::new(
            row.old_line,
            row.new_line,
            row.kind != FileDiffRowKind::Context,
        )
    }

    fn new(old_line: Option<u32>, new_line: Option<u32>, changed: bool) -> Option<Self> {
        (old_line.is_some() || new_line.is_some()).then_some(Self {
            old_line,
            new_line,
            changed,
        })
    }

    /// Removed lines anchor on the base side; everything else on the head side.
    fn side_line(&self) -> (ReviewSide, u32) {
        match (self.old_line, self.new_line) {
            (_, Some(new_line)) => (ReviewSide::Right, new_line),
            (Some(old_line), None) => (ReviewSide::Left, old_line),
            (None, None) => unreachable!("ReviewRow::new rejects rows without a line"),
        }
    }
}

impl MainPaneView {
    pub(in crate::view) fn review_row(&self, visible_ix: usize) -> Option<ReviewRow> {
        let mapped_ix = self.diff_mapped_ix_for_visible_ix(visible_ix)?;
        if self.is_collapsed_diff_projection_active() || self.is_file_diff_view_active() {
            return match self.diff_view {
                DiffViewMode::Inline => self
                    .file_diff_inline_row(mapped_ix)
                    .and_then(ReviewRow::from_annotated),
                DiffViewMode::Split => self
                    .file_diff_split_row(mapped_ix)
                    .and_then(|row| ReviewRow::from_split(&row)),
            };
        }
        match self.diff_view {
            DiffViewMode::Inline => self
                .patch_diff_row(mapped_ix)
                .and_then(ReviewRow::from_annotated),
            DiffViewMode::Split => match self.patch_diff_split_row(mapped_ix)? {
                PatchSplitRow::Aligned { row, .. } => ReviewRow::from_split(&row),
                PatchSplitRow::Raw {
                    src_ix,
                    click_kind: DiffClickKind::Line,
                } => self
                    .patch_diff_row(src_ix)
                    .and_then(ReviewRow::from_annotated),
                PatchSplitRow::Raw { .. } => None,
            },
        }
    }

    /// The moving end of the selection.
    fn review_head(&self) -> Option<usize> {
        match (self.diff_selection_anchor, self.diff_selection_range) {
            (Some(anchor), Some((lo, hi))) => Some(if anchor == lo { hi } else { lo }),
            (None, Some((lo, _))) => Some(lo),
            (anchor, None) => anchor,
        }
    }

    fn review_set_cursor(
        &mut self,
        anchor: usize,
        head: usize,
        strategy: gpui::ScrollStrategy,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let range = (anchor.min(head), anchor.max(head));
        self.scroll_diff_to_item_strict(head, strategy);
        if self.diff_selection_anchor == Some(anchor) && self.diff_selection_range == Some(range) {
            return false;
        }
        self.diff_selection_anchor = Some(anchor);
        self.diff_selection_range = Some(range);
        cx.notify();
        true
    }

    /// Visual rows in source order, one per source row so word wrap is
    /// invisible, with the review row each carries.
    fn review_rows(&self) -> impl Iterator<Item = (usize, ReviewRow)> + '_ {
        (0..self.diff_source_visible_len()).filter_map(|source_ix| {
            let visible_ix = self.diff_visual_ix_for_source_visible_ix(source_ix);
            Some((visible_ix, self.review_row(visible_ix)?))
        })
    }

    pub(in crate::view) fn review_cursor_to_start(&mut self, cx: &mut gpui::Context<Self>) -> bool {
        let mut first = None;
        for (visible_ix, row) in self.review_rows() {
            if row.changed {
                first = Some(visible_ix);
                break;
            }
            first.get_or_insert(visible_ix);
        }
        let Some(visible_ix) = first else {
            return false;
        };
        self.review_set_cursor(visible_ix, visible_ix, gpui::ScrollStrategy::Nearest, cx)
    }

    pub(in crate::view) fn review_move_cursor(
        &mut self,
        delta: i32,
        extend: bool,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let Some(head) = self.review_head() else {
            return self.review_cursor_to_start(cx);
        };
        let Some(mut source_ix) = self.diff_source_visible_ix_for_visible_ix(head) else {
            return false;
        };
        let source_len = self.diff_source_visible_len();
        let mut target = None;
        let mut remaining = delta.unsigned_abs();
        while remaining > 0 {
            source_ix = if delta < 0 {
                match source_ix.checked_sub(1) {
                    Some(ix) => ix,
                    None => break,
                }
            } else {
                source_ix + 1
            };
            if source_ix >= source_len {
                break;
            }
            let visible_ix = self.diff_visual_ix_for_source_visible_ix(source_ix);
            if self.review_row(visible_ix).is_some() {
                target = Some(visible_ix);
                remaining -= 1;
            }
        }
        let Some(target) = target else {
            return false;
        };
        let anchor = if extend {
            self.diff_selection_anchor.unwrap_or(head)
        } else {
            target
        };
        self.review_set_cursor(anchor, target, gpui::ScrollStrategy::Nearest, cx)
    }

    pub(in crate::view) fn review_collapse_selection(
        &mut self,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let Some(head) = self.review_head() else {
            return false;
        };
        self.review_set_cursor(head, head, gpui::ScrollStrategy::Nearest, cx)
    }

    /// Whether GitHub would take a comment on this row: it is changed or
    /// within the hunk context of a changed row. A row that is not a line
    /// (hunk or file header) ends the hunk, like the hidden gap it stands for.
    fn review_row_commentable(&self, visible_ix: usize) -> bool {
        let Some(row) = self.review_row(visible_ix) else {
            return false;
        };
        if row.changed {
            return true;
        }
        let Some(source_ix) = self.diff_source_visible_ix_for_visible_ix(visible_ix) else {
            return false;
        };
        let changed_at = |source_ix: usize| {
            self.review_row(self.diff_visual_ix_for_source_visible_ix(source_ix))
                .map(|row| row.changed)
        };
        let near_change = |rows: &mut dyn Iterator<Item = usize>| {
            for source_ix in rows {
                match changed_at(source_ix) {
                    Some(true) => return true,
                    Some(false) => {}
                    None => return false,
                }
            }
            false
        };
        let source_len = self.diff_source_visible_len();
        near_change(&mut (source_ix + 1..source_len).take(REVIEW_HUNK_CONTEXT_ROWS))
            || near_change(&mut (0..source_ix).rev().take(REVIEW_HUNK_CONTEXT_ROWS))
    }

    pub(in crate::view) fn review_selection_anchor(
        &self,
        path: &str,
    ) -> Result<ReviewAnchor, &'static str> {
        // After a change jump the range is gone, but the anchor is the row it
        // landed on.
        let (lo, hi) = self
            .diff_selection_range
            .or(self.diff_selection_anchor.map(|ix| (ix, ix)))
            .ok_or(REVIEW_NO_LINE)?;
        let start_row = self.review_row(lo).ok_or(REVIEW_NO_LINE)?;
        let end_row = self.review_row(hi).ok_or(REVIEW_NO_LINE)?;
        // GitHub refuses a range whole if any row of it is outside a hunk, or
        // if it spans the hidden gap behind a hunk header.
        let (Some(first), Some(last)) = (
            self.diff_source_visible_ix_for_visible_ix(lo),
            self.diff_source_visible_ix_for_visible_ix(hi),
        ) else {
            return Err(REVIEW_NO_LINE);
        };
        for source_ix in first..=last {
            let visible_ix = self.diff_visual_ix_for_source_visible_ix(source_ix);
            if self.review_row(visible_ix).is_none() {
                return Err(REVIEW_ACROSS_GAP);
            }
            if !self.review_row_commentable(visible_ix) {
                return Err(REVIEW_OUTSIDE_HUNK);
            }
        }
        let end = end_row.side_line();
        // A range ending on a removed line counts in old line numbers, so it
        // starts on the old side too wherever its first row has one.
        let start = match (end.0, start_row.old_line) {
            (ReviewSide::Left, Some(old_line)) => (ReviewSide::Left, old_line),
            _ => start_row.side_line(),
        };
        Ok(ReviewAnchor {
            path: path.to_owned(),
            side: end.0,
            line: end.1,
            start: (start != end).then_some(start),
        })
    }

    /// Whether the diff on screen has any row a cursor can sit on yet.
    pub(in crate::view) fn review_has_rows(&self) -> bool {
        self.review_rows().next().is_some()
    }

    pub(in crate::view) fn review_jump_to(
        &mut self,
        side: ReviewSide,
        line: u32,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        // A base-side line prefers its removed row; in split view it may only
        // exist paired with an added line, so any row with that old line will do.
        let mut fallback = None;
        let mut found = None;
        for (visible_ix, row) in self.review_rows() {
            match side {
                ReviewSide::Right if row.new_line == Some(line) => {
                    found = Some(visible_ix);
                    break;
                }
                ReviewSide::Left if row.old_line == Some(line) => {
                    if row.new_line.is_none() {
                        found = Some(visible_ix);
                        break;
                    }
                    fallback.get_or_insert(visible_ix);
                }
                _ => {}
            }
        }
        let Some(visible_ix) = found.or(fallback) else {
            return false;
        };
        self.review_set_cursor(visible_ix, visible_ix, gpui::ScrollStrategy::Center, cx);
        true
    }

    /// Which side of this row has a pending review comment; read per painted row.
    pub(in crate::view) fn review_mark_side(&self, visible_ix: usize) -> Option<ReviewSide> {
        if !self.review_active || self.review_marks.is_empty() {
            return None;
        }
        let row = self.review_row(visible_ix)?;
        if let Some(new_line) = row.new_line
            && self.review_marks.contains(&(ReviewSide::Right, new_line))
        {
            return Some(ReviewSide::Right);
        }
        row.old_line
            .filter(|old_line| self.review_marks.contains(&(ReviewSide::Left, *old_line)))
            .map(|_| ReviewSide::Left)
    }
}
