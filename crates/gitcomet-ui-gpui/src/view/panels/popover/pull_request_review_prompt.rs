use super::*;
use crate::github::ReviewKind;

/// The three review types, each on an Alt chord so they switch without leaving
/// the text box.
const KINDS: [(ReviewKind, &str, &str, &str); 3] = [
    (ReviewKind::Comment, "Comment", "c", "Alt+C"),
    (ReviewKind::Approve, "Approve", "a", "Alt+A"),
    (ReviewKind::RequestChanges, "Request changes", "x", "Alt+X"),
];

fn submit_label(kind: ReviewKind, number: u64) -> String {
    match kind {
        ReviewKind::Comment => "Post comment".to_string(),
        ReviewKind::Approve => format!("Approve #{number}"),
        ReviewKind::RequestChanges => "Request changes".to_string(),
    }
}

pub(super) fn panel(
    this: &mut PopoverHost,
    repo_id: RepoId,
    number: u64,
    kind: ReviewKind,
    cx: &mut gpui::Context<PopoverHost>,
) -> gpui::Div {
    let theme = this.theme;
    let scaled_px = super::popover_scaled_px_fn(cx);
    let can_submit = this.can_submit_pull_request_review(cx);
    let submitting = this.pull_request_submitting(cx);
    let error = this.pull_request_submit_error(cx);
    let body_empty = this
        .pull_request_review_input
        .read_with(cx, |input, _| input.text().trim().is_empty());

    let kind_row =
        div()
            .px_2()
            .py_1()
            .flex()
            .gap_1()
            .children(KINDS.map(|(option, label, key, chord)| {
                components::Button::new(format!("pull_request_review_kind_{key}"), label)
                    .end_slot(super::hotkey_hint(
                        theme,
                        "pull_request_review_kind_hint",
                        chord,
                    ))
                    .style(if option == kind {
                        components::ButtonStyle::Filled
                    } else {
                        components::ButtonStyle::Subtle
                    })
                    .on_click(theme, cx, move |this, _e, _window, cx| {
                        this.set_pull_request_review_kind(option, cx);
                    })
            }));

    div()
        .flex()
        .flex_col()
        .w(scaled_px(540.0))
        .on_action(cx.listener(
            |this, _: &crate::view::TextInputCommitSubmit, _window, cx| {
                if this.submit_pull_request_prompt(cx) {
                    cx.stop_propagation();
                }
            },
        ))
        .on_key_down(
            cx.listener(move |this, e: &gpui::KeyDownEvent, window, cx| {
                let mods = e.keystroke.modifiers;
                if !mods.alt || mods.control || mods.platform || mods.shift {
                    return;
                }
                if e.keystroke.key == "g" {
                    this.draft_pull_request_review_with_codex(repo_id, number, window, cx);
                    cx.stop_propagation();
                    return;
                }
                let Some(&(kind, ..)) = KINDS
                    .iter()
                    .find(|(_, _, key, _)| *key == e.keystroke.key.as_str())
                else {
                    return;
                };
                this.set_pull_request_review_kind(kind, cx);
                cx.stop_propagation();
            }),
        )
        .child(popover_title(theme, format!("Review #{number}")))
        .child(super::popover_rule(theme))
        .child(
            kind_row.child(div().flex_1()).child(
                components::Button::new("pull_request_review_codex", "Draft with Codex")
                    .end_slot(super::hotkey_hint(
                        theme,
                        "pull_request_review_codex_hint",
                        "Alt+G",
                    ))
                    .style(components::ButtonStyle::Subtle)
                    .on_click(theme, cx, move |this, _e, window, cx| {
                        this.draft_pull_request_review_with_codex(repo_id, number, window, cx);
                    }),
            ),
        )
        .child(
            div().px_2().py_1().w_full().min_w(px(0.0)).child(
                components::ScrollContainer::vertical(
                    "pull_request_review_scroll_surface",
                    "pull_request_review_scrollbar",
                    this.pull_request_review_scroll.clone(),
                    px(220.0),
                )
                .render(theme, this.pull_request_review_input.clone()),
            ),
        )
        .when(kind.needs_body() && body_empty, |panel| {
            panel.child(
                div()
                    .px_2()
                    .text_size(theme.ui_text(12.0))
                    .text_color(theme.colors.foreground.secondary)
                    .child("This review type needs a comment."),
            )
        })
        .when_some(error, |panel, error| {
            panel.child(
                div()
                    .mx_2()
                    .my_1()
                    .px_2()
                    .py_1()
                    .rounded(scaled_px(theme.radii.control))
                    .border_1()
                    .border_color(theme.colors.status.danger.border)
                    .bg(theme.colors.status.danger.background)
                    .text_size(theme.ui_text(12.0))
                    .text_color(theme.colors.foreground.primary)
                    .child(format!("Couldn't post the review: {error}")),
            )
        })
        .child(super::popover_rule(theme))
        .child(
            super::prompt_footer_row()
                .child(
                    div()
                        .text_size(theme.ui_text(12.0))
                        .text_color(theme.colors.foreground.secondary)
                        .child(if submitting {
                            "Posting through gh…"
                        } else {
                            "Posted as you through gh."
                        }),
                )
                .child(
                    div()
                        .flex()
                        .gap_1()
                        .child(
                            cancel_button(
                                "pull_request_review_cancel",
                                "pull_request_review_cancel_hint",
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
                                "pull_request_review_submit",
                                submit_label(kind, number),
                            )
                            .separated_end_slot(super::hotkey_hint(
                                theme,
                                "pull_request_review_submit_hint",
                                crate::view::shortcut_labels::secondary_shortcut("Enter"),
                            ))
                            .style(components::ButtonStyle::Filled)
                            .disabled(!can_submit)
                            .on_click(
                                theme,
                                cx,
                                |this, _e, _window, cx| {
                                    this.submit_pull_request_prompt(cx);
                                },
                            ),
                        ),
                ),
        )
}
