use super::*;
use crate::github::ReviewAnchor;

/// A line comment for the review in progress: it joins the pending review,
/// and nothing is posted until the review is submitted.
pub(super) fn panel(
    this: &mut PopoverHost,
    anchor: ReviewAnchor,
    edit: Option<usize>,
    cx: &mut gpui::Context<PopoverHost>,
) -> gpui::Div {
    let theme = this.theme;
    let scaled_px = super::popover_scaled_px_fn(cx);
    let empty = this
        .review_comment_input
        .read_with(cx, |input, _| input.text().trim().is_empty());
    let title = match edit {
        Some(_) => format!("Edit comment on {}", anchor.lines_label()),
        None => format!("Comment on {}", anchor.lines_label()),
    };

    div()
        .flex()
        .flex_col()
        .w(scaled_px(540.0))
        .on_action(
            cx.listener(|this, _: &crate::view::TextInputCommitSubmit, window, cx| {
                if this.submit_review_comment(window, cx) {
                    cx.stop_propagation();
                }
            }),
        )
        .child(popover_title(theme, title))
        .child(super::popover_detail(theme, anchor.path.clone()))
        .child(super::popover_rule(theme))
        .child(
            div().px_2().py_1().w_full().min_w(px(0.0)).child(
                components::ScrollContainer::vertical(
                    "review_comment_scroll_surface",
                    "review_comment_scrollbar",
                    this.review_comment_scroll.clone(),
                    px(200.0),
                )
                .render(theme, this.review_comment_input.clone()),
            ),
        )
        .child(super::popover_rule(theme))
        .child(
            super::prompt_footer_row()
                .child(
                    div()
                        .text_size(theme.ui_text(12.0))
                        .text_color(theme.colors.foreground.secondary)
                        .child("Pending until you submit the review. esc keeps the text."),
                )
                .child(
                    div()
                        .flex()
                        .gap_1()
                        .child(
                            cancel_button(
                                "review_comment_cancel",
                                "review_comment_cancel_hint",
                                theme,
                            )
                            .on_click(
                                theme,
                                cx,
                                |this, _e, window, cx| {
                                    this.dismiss_prompt_popover(window, cx);
                                },
                            ),
                        )
                        .child(
                            components::Button::new(
                                "review_comment_submit",
                                if edit.is_some() {
                                    "Save comment"
                                } else {
                                    "Add to review"
                                },
                            )
                            .separated_end_slot(super::hotkey_hint(
                                theme,
                                "review_comment_submit_hint",
                                crate::view::shortcut_labels::secondary_shortcut("Enter"),
                            ))
                            .style(components::ButtonStyle::Filled)
                            .disabled(empty)
                            .on_click(
                                theme,
                                cx,
                                |this, _e, window, cx| {
                                    this.submit_review_comment(window, cx);
                                },
                            ),
                        ),
                ),
        )
}
