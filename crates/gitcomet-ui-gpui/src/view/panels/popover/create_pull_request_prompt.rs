use super::*;
use crate::view::pull_requests::{HeadProblem, pull_request_head};

/// Local commits on `branch` (or the checked-out one) its upstream doesn't
/// have yet.
fn unpushed_commits(repo: &RepoState, branch: Option<&str>) -> usize {
    let name = match (branch, &repo.head_branch) {
        (Some(name), _) => name,
        (None, Loadable::Ready(head)) => head.as_str(),
        _ => return 0,
    };
    repo.branches
        .ready()
        .and_then(|branches| branches.iter().find(|candidate| candidate.name == name))
        .and_then(|branch| branch.divergence.as_ref())
        .map_or(0, |divergence| divergence.ahead)
}

/// Whether `branch` is the checked-out one, which Alt+P can push.
fn is_checked_out(repo: &RepoState, branch: Option<&str>) -> bool {
    match (branch, &repo.head_branch) {
        (None, _) => true,
        (Some(name), Loadable::Ready(head)) => name == head,
        _ => false,
    }
}

fn notice(theme: AppTheme, warning: bool, text: String) -> gpui::Div {
    let colors = if warning {
        theme.colors.status.warning
    } else {
        theme.colors.status.info
    };
    div()
        .mx_2()
        .my_1()
        .px_2()
        .py_1()
        .rounded(px(theme.radii.control))
        .border_1()
        .border_color(colors.border)
        .bg(colors.background)
        .text_size(theme.ui_text(12.0))
        .text_color(theme.colors.foreground.primary)
        .child(text)
}

pub(super) fn panel(
    this: &mut PopoverHost,
    repo_id: RepoId,
    branch: Option<String>,
    cx: &mut gpui::Context<PopoverHost>,
) -> gpui::Div {
    let theme = this.theme;
    let scaled_px = super::popover_scaled_px_fn(cx);
    let can_submit = this.can_submit_create_pull_request(cx);
    let submitting = this.pull_request_submitting(cx);
    let error = this.pull_request_submit_error(cx);
    let draft = this.pull_request_draft;
    let repo = this.state.repos.iter().find(|repo| repo.id == repo_id);
    let head = repo.map(|repo| pull_request_head(repo, branch.as_deref()));
    let unpushed = repo.map_or(0, |repo| unpushed_commits(repo, branch.as_deref()));
    let can_push = repo.is_some_and(|repo| is_checked_out(repo, branch.as_deref()));

    let head_notice = match &head {
        Some(Err(HeadProblem::Detached)) | None => Some(notice(
            theme,
            true,
            "Check out a branch first: a detached HEAD can't open a pull request.".to_string(),
        )),
        Some(Err(HeadProblem::NotPushed(name))) if can_push => Some(notice(
            theme,
            true,
            format!(
                "{name} isn't on GitHub yet. Push it first (Alt+P): creating a pull request never pushes."
            ),
        )),
        Some(Err(HeadProblem::NotPushed(name))) => Some(notice(
            theme,
            true,
            format!(
                "{name} isn't on GitHub yet. Check it out and push it first: creating a pull request never pushes."
            ),
        )),
        Some(Ok(_)) if unpushed > 0 => Some(notice(
            theme,
            false,
            format!(
                "{unpushed} local commit{} aren't pushed. The pull request opens with what's on GitHub.",
                if unpushed == 1 { "" } else { "s" }
            ),
        )),
        Some(Ok(_)) => None,
    };
    let from = match (&head, &branch) {
        (Some(Ok(head)), _) => format!("From {head}"),
        (_, Some(branch)) => format!("From {branch}"),
        _ => "From the checked-out branch".to_string(),
    };
    let open_on_github =
        components::Button::new("create_pull_request_on_github", "Open on GitHub instead")
            .end_slot(super::hotkey_hint(
                theme,
                "create_pull_request_on_github_hint",
                "Alt+O",
            ))
            .style(components::ButtonStyle::Subtle)
            .on_click(theme, cx, |this, _e, window, cx| {
                this.open_pull_request_compare_from_dialog(window, cx);
            });

    let draft_toggle = components::Button::new(
        "create_pull_request_draft",
        if draft { "Draft: yes" } else { "Draft: no" },
    )
    .end_slot(super::hotkey_hint(
        theme,
        "create_pull_request_draft_hint",
        "Alt+D",
    ))
    .style(if draft {
        components::ButtonStyle::Filled
    } else {
        components::ButtonStyle::Subtle
    })
    .on_click(theme, cx, |this, _e, _window, cx| {
        this.toggle_pull_request_draft(cx);
    });

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
                match e.keystroke.key.as_str() {
                    "d" => this.toggle_pull_request_draft(cx),
                    "o" => this.open_pull_request_compare_from_dialog(window, cx),
                    // The normal push flow, run only because the user asked; it
                    // pushes the checked-out branch, so only for that one.
                    "p" if can_push => {
                        let root = this.root_view.clone();
                        window.defer(cx, move |window, cx| {
                            let _ = root.update(cx, |root, cx| {
                                root.execute_command("push", Some(window), cx);
                            });
                        });
                    }
                    _ => return,
                }
                cx.stop_propagation();
            }),
        )
        .child(popover_title(theme, "New pull request"))
        .child(super::popover_rule(theme))
        .child(super::popover_detail(theme, from))
        .children(head_notice)
        .child(input_label(theme, "Base branch"))
        .child(
            div()
                .px_2()
                .pb_1()
                .w_full()
                .min_w(px(0.0))
                .child(this.pull_request_base_input.clone()),
        )
        .child(input_label(theme, "Title"))
        .child(
            div()
                .px_2()
                .pb_1()
                .w_full()
                .min_w(px(0.0))
                .child(this.pull_request_title_input.clone()),
        )
        .child(input_label(theme, "Description"))
        .child(
            div().px_2().pb_1().w_full().min_w(px(0.0)).child(
                components::ScrollContainer::vertical(
                    "create_pull_request_body_scroll_surface",
                    "create_pull_request_body_scrollbar",
                    this.pull_request_body_scroll.clone(),
                    px(180.0),
                )
                .render(theme, this.pull_request_body_input.clone()),
            ),
        )
        .child(
            div()
                .px_2()
                .py_1()
                .flex()
                .gap_1()
                .child(draft_toggle)
                .child(div().flex_1())
                .child(open_on_github),
        )
        .when_some(error, |panel, error| {
            panel.child(notice(
                theme,
                true,
                format!("Couldn't create the pull request: {error}"),
            ))
        })
        .child(super::popover_rule(theme))
        .child(
            super::prompt_footer_row()
                .child(
                    div()
                        .text_size(theme.ui_text(12.0))
                        .text_color(theme.colors.foreground.secondary)
                        .child(if submitting {
                            "Creating through gh…"
                        } else {
                            "Runs gh pr create. Nothing is pushed."
                        }),
                )
                .child(
                    div()
                        .flex()
                        .gap_1()
                        .child(
                            cancel_button(
                                "create_pull_request_cancel",
                                "create_pull_request_cancel_hint",
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
                                "create_pull_request_submit",
                                if draft {
                                    "Create draft pull request"
                                } else {
                                    "Create pull request"
                                },
                            )
                            .separated_end_slot(super::hotkey_hint(
                                theme,
                                "create_pull_request_submit_hint",
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
