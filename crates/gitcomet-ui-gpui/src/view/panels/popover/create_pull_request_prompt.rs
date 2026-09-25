use super::*;

/// Why the checked-out branch can't head a new pull request yet.
pub(super) enum HeadProblem {
    Detached,
    /// The branch has no upstream, so GitHub has never seen it.
    NotPushed(String),
}

/// The branch a new pull request comes from, as `gh pr create --head` takes
/// it: the checked-out branch's upstream, which must exist on the remote.
/// Creating never pushes, so a branch without one blocks the dialog. When the
/// upstream is a different GitHub repository than the pull request's (a
/// fork), it is `owner:branch`.
pub(super) fn pull_request_head(repo: &RepoState) -> Result<String, HeadProblem> {
    let Loadable::Ready(head) = &repo.head_branch else {
        return Err(HeadProblem::Detached);
    };
    if head == "HEAD" {
        return Err(HeadProblem::Detached);
    }
    if !crate::view::mod_helpers::head_branch_has_live_upstream(repo) {
        return Err(HeadProblem::NotPushed(head.clone()));
    }
    let upstream = match &repo.branches {
        Loadable::Ready(branches) => branches
            .iter()
            .find(|branch| branch.name == *head)
            .and_then(|branch| branch.upstream.clone()),
        _ => None,
    };
    let Some(upstream) = upstream else {
        return Err(HeadProblem::NotPushed(head.clone()));
    };
    let remotes = repo
        .remotes
        .ready()
        .map(|remotes| remotes.as_slice())
        .unwrap_or(&[]);
    let target = crate::view::permalink::github_remote(remotes).map(|(name, _)| name);
    if target.as_deref() == Some(upstream.remote.as_str()) {
        return Ok(upstream.branch);
    }
    let owner = remotes
        .iter()
        .find(|remote| remote.name == upstream.remote)
        .and_then(|remote| crate::view::permalink::github_slug(remote.url.as_deref()?))
        .and_then(|slug| slug.split('/').next().map(str::to_string));
    Ok(match owner {
        Some(owner) => format!("{owner}:{}", upstream.branch),
        None => upstream.branch,
    })
}

/// Local commits on the head branch that its upstream doesn't have yet.
fn unpushed_commits(repo: &RepoState) -> usize {
    let (Loadable::Ready(head), Loadable::Ready(branches)) = (&repo.head_branch, &repo.branches)
    else {
        return 0;
    };
    branches
        .iter()
        .find(|branch| branch.name == *head)
        .and_then(|branch| branch.divergence.as_ref())
        .map_or(0, |divergence| divergence.ahead)
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
    cx: &mut gpui::Context<PopoverHost>,
) -> gpui::Div {
    let theme = this.theme;
    let scaled_px = super::popover_scaled_px_fn(cx);
    let can_submit = this.can_submit_create_pull_request(cx);
    let submitting = this.pull_request_submitting(cx);
    let error = this.pull_request_submit_error(cx);
    let draft = this.pull_request_draft;
    let repo = this.state.repos.iter().find(|repo| repo.id == repo_id);
    let head = repo.map(pull_request_head);
    let unpushed = repo.map_or(0, unpushed_commits);

    let head_notice = match &head {
        Some(Err(HeadProblem::Detached)) | None => Some(notice(
            theme,
            true,
            "Check out a branch first: a detached HEAD can't open a pull request.".to_string(),
        )),
        Some(Err(HeadProblem::NotPushed(name))) => Some(notice(
            theme,
            true,
            format!(
                "{name} isn't on GitHub yet. Push it first (Alt+P): creating a pull request never pushes."
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
    let from = match &head {
        Some(Ok(branch)) => format!("From {branch}"),
        _ => "From the checked-out branch".to_string(),
    };

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
        .on_key_down(cx.listener(|this, e: &gpui::KeyDownEvent, window, cx| {
            let mods = e.keystroke.modifiers;
            if !mods.alt || mods.control || mods.platform || mods.shift {
                return;
            }
            match e.keystroke.key.as_str() {
                "d" => this.toggle_pull_request_draft(cx),
                // The normal push flow, run only because the user asked.
                "p" => {
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
        }))
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
        .child(div().px_2().py_1().child(draft_toggle))
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
