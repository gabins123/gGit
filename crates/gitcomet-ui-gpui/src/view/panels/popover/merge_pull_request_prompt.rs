use super::*;
use crate::github::MergeMethod;

/// The three merge methods, each on an Alt chord.
const METHODS: [(MergeMethod, &str, &str, &str); 3] = [
    (MergeMethod::Merge, "Merge commit", "m", "Alt+M"),
    (MergeMethod::Squash, "Squash", "s", "Alt+S"),
    (MergeMethod::Rebase, "Rebase", "r", "Alt+R"),
];

fn submit_label(method: MergeMethod) -> &'static str {
    match method {
        MergeMethod::Merge => "Merge",
        MergeMethod::Squash => "Squash and merge",
        MergeMethod::Rebase => "Rebase and merge",
    }
}

pub(super) fn panel(
    this: &mut PopoverHost,
    number: u64,
    method: MergeMethod,
    cx: &mut gpui::Context<PopoverHost>,
) -> gpui::Div {
    let theme = this.theme;
    let submitting = this.pull_request_submitting(cx);
    let error = this.pull_request_submit_error(cx);
    let delete_branch = this.pull_request_delete_branch;
    let detail = this.pull_request_merge_detail(number, cx);
    // gh only deletes a branch that lives in this repository.
    let can_delete_branch = detail
        .as_ref()
        .is_some_and(|detail| !detail.is_cross_repository);

    let method_row =
        div()
            .px_2()
            .py_1()
            .flex()
            .gap_1()
            .children(METHODS.map(|(option, label, key, chord)| {
                components::Button::new(format!("pull_request_merge_method_{key}"), label)
                    .end_slot(super::hotkey_hint(
                        theme,
                        "pull_request_merge_method_hint",
                        chord,
                    ))
                    .style(if option == method {
                        components::ButtonStyle::Filled
                    } else {
                        components::ButtonStyle::Subtle
                    })
                    .on_click(theme, cx, move |this, _e, _window, cx| {
                        this.set_pull_request_merge_method(option, cx);
                    })
            }));
    let delete_row = div().px_2().py_1().child(
        components::Button::new(
            "pull_request_merge_delete_branch",
            if delete_branch {
                "✓ Delete the branch on GitHub"
            } else {
                "Delete the branch on GitHub"
            },
        )
        .end_slot(super::hotkey_hint(
            theme,
            "pull_request_merge_delete_branch_hint",
            "Alt+D",
        ))
        .style(if delete_branch {
            components::ButtonStyle::Filled
        } else {
            components::ButtonStyle::Subtle
        })
        .on_click(theme, cx, |this, _e, _window, cx| {
            this.toggle_pull_request_delete_branch(cx);
        }),
    );

    // The title is the author's text: kept off the line that names the
    // branches, so it can't pose as a different target.
    let mut dialog = ConfirmDialog::new(format!("Merge #{number}"), DIALOG_540_WIDTH);
    dialog = match &detail {
        Some(detail) => dialog
            .text(theme, detail.title.clone())
            .mono_value(theme, format!("{} → {}", detail.head, detail.base)),
        None => dialog.text(theme, format!("Loading #{number}…")),
    };
    dialog = dialog.section(method_row);
    if can_delete_branch {
        dialog = dialog.section(delete_row);
    }
    if let Some(error) = error {
        dialog = dialog.section(
            div()
                .mx_2()
                .my_1()
                .px_2()
                .py_1()
                .rounded(px(theme.radii.control))
                .border_1()
                .border_color(theme.colors.status.danger.border)
                .bg(theme.colors.status.danger.background)
                .text_size(theme.ui_text(12.0))
                .text_color(theme.colors.foreground.primary)
                .child(error),
        );
    }
    dialog
        .note(
            theme,
            if submitting {
                "Merging through gh…"
            } else {
                "Merged as you through gh. Your local branches are left alone."
            },
        )
        .render(
            theme,
            dialog_cancel_button(
                "pull_request_merge_cancel",
                "pull_request_merge_cancel_hint",
                theme,
                cx,
            ),
            components::Button::new("pull_request_merge_go", submit_label(method))
                .disabled(submitting)
                .focus_handle(this.pull_request_merge_focus_handle.clone())
                .separated_end_slot(super::hotkey_hint(
                    theme,
                    "pull_request_merge_go_hint",
                    "Enter",
                ))
                .style(components::ButtonStyle::Filled)
                .on_click(theme, cx, |this, _e, _window, cx| {
                    this.submit_pull_request_prompt(cx);
                }),
            cx,
        )
        // Only Enter merges: Space would click the focused button too, and
        // Space is this tab's checkout key.
        .capture_key_down(cx.listener(|_, e: &gpui::KeyDownEvent, window, cx| {
            if e.keystroke.key == "space" && !e.keystroke.modifiers.modified() {
                window.prevent_default();
                cx.stop_propagation();
            }
        }))
        .on_key_down(
            cx.listener(move |this, e: &gpui::KeyDownEvent, _window, cx| {
                let mods = e.keystroke.modifiers;
                if !mods.alt || mods.control || mods.platform || mods.shift {
                    return;
                }
                let key = e.keystroke.key.as_str();
                if key == "d" && can_delete_branch {
                    this.toggle_pull_request_delete_branch(cx);
                } else if let Some(&(method, ..)) = METHODS.iter().find(|(_, _, k, _)| *k == key) {
                    this.set_pull_request_merge_method(method, cx);
                } else {
                    return;
                }
                cx.stop_propagation();
            }),
        )
}
