use super::*;
use crate::view::pull_requests::{
    HeadProblem, own_upstream, pull_request_head_into, remote_branch_head,
};
use gitcomet_core::services::BranchPushRequest;

/// Where the pull request's branch stands on GitHub, and what "push first"
/// would do about it.
pub(super) struct HeadState {
    /// The local branch.
    pub(super) branch: String,
    /// The branch as `gh pr create --head` takes it, once pushed.
    pub(super) head: String,
    /// The branch's name on the remote it's pushed to.
    pub(super) remote_branch: String,
    /// The push that brings GitHub up to date; `None` when it is.
    pub(super) push: Option<BranchPushRequest>,
    /// Local commits GitHub doesn't have; `None` when it has never seen the
    /// branch.
    pub(super) unpushed: Option<usize>,
}

impl HeadState {
    /// Whether `base` names the branch itself. Only within one repository:
    /// a fork's `feature` into the parent's `feature` is a real pull request.
    pub(super) fn base_is_head(&self, base: &str) -> bool {
        !base.is_empty() && !self.head.contains(':') && base == self.remote_branch
    }
}

/// `None` for a detached HEAD, which can't head a pull request. `base` is the
/// base typed so far: an upstream by that name is where the pull request
/// goes, never the branch's own.
pub(super) fn head_state(repo: &RepoState, branch: Option<&str>, base: &str) -> Option<HeadState> {
    let name = match (branch, &repo.head_branch) {
        (Some(name), _) => name.to_string(),
        (None, Loadable::Ready(head)) if head != "HEAD" => head.clone(),
        _ => return None,
    };
    let base = Some(base).filter(|base| !base.is_empty());
    let local = repo
        .branches
        .ready()
        .and_then(|branches| branches.iter().find(|candidate| candidate.name == name));
    let upstream = own_upstream(repo, &name, base);
    // Pushed as it stands when asked: a branch that moves meanwhile isn't.
    let push = |remote: String, branch: String, set_upstream: bool| {
        local.map(|local| BranchPushRequest {
            remote,
            local_branch: name.clone(),
            branch,
            head: local.target.clone(),
            set_upstream,
        })
    };
    match pull_request_head_into(repo, Some(&name), base) {
        Ok(head) => {
            // Its own upstream is on the remote, or there'd be no head.
            let upstream = upstream?;
            let ahead = local
                .and_then(|branch| branch.divergence.as_ref())
                .map_or(0, |divergence| divergence.ahead);
            Some(HeadState {
                push: if ahead > 0 {
                    push(upstream.remote.clone(), upstream.branch.clone(), false)
                } else {
                    None
                },
                remote_branch: upstream.branch,
                branch: name,
                head,
                unpushed: Some(ahead),
            })
        }
        Err(HeadProblem::NotPushed) => {
            // Back to its own upstream when that branch is gone; a new branch
            // goes to origin when origin is on GitHub (a fork's usual name),
            // else to the pull requests' own remote.
            let (remote, remote_branch) = match upstream {
                Some(upstream) => (upstream.remote, upstream.branch),
                None => {
                    let remotes = repo.remotes.ready()?;
                    let remote = remotes
                        .iter()
                        .find(|remote| {
                            remote.name == "origin"
                                && remote
                                    .url
                                    .as_deref()
                                    .and_then(crate::view::permalink::github_slug)
                                    .is_some()
                        })
                        .map(|remote| remote.name.clone())
                        .or_else(|| {
                            crate::view::permalink::github_remote(remotes).map(|(name, _)| name)
                        })?;
                    (remote, name.clone())
                }
            };
            Some(HeadState {
                head: remote_branch_head(repo, &remote, &remote_branch),
                push: push(remote, remote_branch.clone(), true),
                remote_branch,
                branch: name,
                unpushed: None,
            })
        }
        Err(HeadProblem::Detached) => None,
    }
}

/// Whether `pr` comes from `head` (`branch`, or `owner:branch` from a fork).
/// The list holds every fork's pull requests, so a bare name like `main` is
/// not enough; gh versions that don't report the owner fall back to the name.
pub(super) fn opened_from(
    pr: &crate::github::PullRequestSummary,
    head: &str,
    target_owner: &str,
) -> bool {
    let (owner, branch) = head.split_once(':').unwrap_or((target_owner, head));
    pr.head == branch && (pr.head_owner.is_empty() || pr.head_owner.eq_ignore_ascii_case(owner))
}

/// The pull requests' remote's branches, its default first: what the base can
/// be.
pub(super) fn base_candidates(repo: &RepoState) -> Vec<String> {
    let Some(remote) = repo
        .remotes
        .ready()
        .and_then(|remotes| crate::view::permalink::github_remote(remotes))
        .map(|(name, _)| name)
    else {
        return Vec::new();
    };
    let mut names: Vec<String> = repo
        .remote_branches
        .ready()
        .map(|branches| {
            branches
                .iter()
                .filter(|branch| branch.remote == remote && branch.name != "HEAD")
                .map(|branch| branch.name.clone())
                .collect()
        })
        .unwrap_or_default();
    names.sort_by_key(|name| {
        (
            !matches!(name.as_str(), "main" | "master" | "dev" | "develop"),
            name.clone(),
        )
    });
    names
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
    let push_first = this.pull_request_push_first;
    let existing = this.existing_pull_request(cx);
    let repo = this.state.repos.iter().find(|repo| repo.id == repo_id);
    let base = this
        .pull_request_base_input
        .read_with(cx, |input, _| input.text().trim().to_string());
    let state = repo.and_then(|repo| head_state(repo, branch.as_deref(), &base));
    let candidates = repo.map(base_candidates).unwrap_or_default();

    let mut notices = Vec::new();
    match &state {
        None => notices.push(notice(
            theme,
            true,
            "Check out a branch first: a detached HEAD can't open a pull request.".to_string(),
        )),
        Some(state) => {
            let name = &state.branch;
            match (&state.push, state.unpushed, push_first) {
                (Some(push), None, true) => notices.push(notice(
                    theme,
                    false,
                    format!(
                        "{name} isn't on GitHub yet: creating pushes it to {}/{} first (Alt+P: don't).",
                        push.remote, push.branch
                    ),
                )),
                (Some(_), None, false) => notices.push(notice(
                    theme,
                    true,
                    format!("{name} isn't on GitHub yet. Alt+P pushes it first."),
                )),
                (Some(push), Some(ahead), true) => notices.push(notice(
                    theme,
                    false,
                    format!(
                        "{ahead} local commit{} {} pushed to {}/{} first (Alt+P: don't).",
                        if ahead == 1 { "" } else { "s" },
                        if ahead == 1 { "is" } else { "are" },
                        push.remote,
                        push.branch,
                    ),
                )),
                (Some(_), Some(ahead), false) => notices.push(notice(
                    theme,
                    true,
                    format!(
                        "{ahead} local commit{} aren't pushed. The pull request opens with what's on GitHub; Alt+P pushes them first.",
                        if ahead == 1 { "" } else { "s" }
                    ),
                )),
                _ => {}
            }
        }
    }
    if let Some(number) = existing {
        notices.push(notice(
            theme,
            true,
            format!("#{number} is already open from this branch. Alt+O opens it."),
        ));
    }
    if state
        .as_ref()
        .is_some_and(|state| state.base_is_head(&base))
    {
        notices.push(notice(
            theme,
            true,
            format!("{base} is the branch itself: pick another base (Alt+B)."),
        ));
    }
    if !base.is_empty() && !candidates.is_empty() && !candidates.contains(&base) {
        notices.push(notice(
            theme,
            true,
            format!("GitHub has no branch named {base}."),
        ));
    }

    // Branches matching what's typed, to pick instead of spelling out.
    let lowered = base.to_lowercase();
    let suggestions: Vec<String> = candidates
        .iter()
        .filter(|name| **name != base && name.to_lowercase().contains(&lowered))
        .take(6)
        .cloned()
        .collect();

    let from = match &state {
        Some(state) if state.head != state.branch => {
            format!("From {} as {}", state.branch, state.head)
        }
        Some(state) => format!("From {}", state.branch),
        None => "From the checked-out branch".to_string(),
    };
    let open_on_github = components::Button::new(
        "create_pull_request_on_github",
        if existing.is_some() {
            "Open the existing one"
        } else {
            "Open on GitHub instead"
        },
    )
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

    let needs_push = state.as_ref().is_some_and(|state| state.push.is_some());
    let push_toggle = needs_push.then(|| {
        components::Button::new(
            "create_pull_request_push_first",
            if push_first {
                "Push first: yes"
            } else {
                "Push first: no"
            },
        )
        .end_slot(super::hotkey_hint(
            theme,
            "create_pull_request_push_first_hint",
            "Alt+P",
        ))
        .style(if push_first {
            components::ButtonStyle::Filled
        } else {
            components::ButtonStyle::Subtle
        })
        .on_click(theme, cx, |this, _e, _window, cx| {
            this.toggle_pull_request_push_first(cx);
        })
    });
    let pushes = needs_push && push_first;

    let suggestion_row = (!suggestions.is_empty()).then(|| {
        div()
            .px_2()
            .pb_1()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .text_size(theme.ui_text(12.0))
            .text_color(theme.colors.foreground.secondary)
            .child("Alt+B:")
            .children(suggestions.into_iter().enumerate().map(|(ix, name)| {
                let pick = name.clone();
                components::Button::new(format!("create_pull_request_base_pick_{ix}"), name)
                    .style(components::ButtonStyle::Subtle)
                    .on_click(theme, cx, move |this, _e, _window, cx| {
                        this.set_pull_request_base(pick.clone(), cx);
                    })
            }))
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
                    "p" => this.toggle_pull_request_push_first(cx),
                    "b" => this.next_pull_request_base(cx),
                    _ => return,
                }
                cx.stop_propagation();
            }),
        )
        .child(popover_title(theme, "New pull request"))
        .child(super::popover_rule(theme))
        .child(super::popover_detail(theme, from))
        .children(notices)
        .child(input_label(theme, "Base branch"))
        .child(
            div()
                .px_2()
                .pb_1()
                .w_full()
                .min_w(px(0.0))
                .child(this.pull_request_base_input.clone()),
        )
        .children(suggestion_row)
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
                .children(push_toggle)
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
                        .child(match (submitting, pushes) {
                            (true, true) => "Pushing, then creating through gh…",
                            (true, false) => "Creating through gh…",
                            (false, true) => "Pushes the branch, then runs gh pr create.",
                            (false, false) => "Runs gh pr create. Nothing is pushed.",
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
                                match (pushes, draft) {
                                    (true, true) => "Push and create draft",
                                    (true, false) => "Push and create",
                                    (false, true) => "Create draft pull request",
                                    (false, false) => "Create pull request",
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

#[cfg(test)]
mod tests {
    use super::*;
    use gitcomet_core::domain::{Branch, Remote, RemoteBranch, Upstream, UpstreamDivergence};

    fn repo(branches: Vec<Branch>, remote_branches: Vec<RemoteBranch>) -> RepoState {
        let mut repo = RepoState::new_opening(
            RepoId(1),
            gitcomet_core::domain::RepoSpec {
                workdir: "/tmp/create-pr".into(),
            },
        );
        let target = CommitId("1".repeat(40).into());
        repo.head_branch = Loadable::Ready("feature".into());
        repo.remotes = Loadable::Ready(Arc::new(vec![Remote {
            name: "origin".into(),
            url: Some("https://github.com/owner/repo.git".into()),
        }]));
        repo.branches = Loadable::Ready(Arc::new(
            branches
                .into_iter()
                .map(|mut branch| {
                    branch.target = target.clone();
                    branch
                })
                .collect(),
        ));
        repo.remote_branches = Loadable::Ready(Arc::new(
            remote_branches
                .into_iter()
                .map(|mut branch| {
                    branch.target = target.clone();
                    branch
                })
                .collect(),
        ));
        repo
    }

    fn branch(upstream: Option<(&str, &str)>, ahead: usize) -> Branch {
        Branch {
            name: "feature".into(),
            target: CommitId(String::new().into()),
            upstream: upstream.map(|(remote, branch)| Upstream {
                remote: remote.into(),
                branch: branch.into(),
            }),
            divergence: Some(UpstreamDivergence { ahead, behind: 0 }),
        }
    }

    fn remote_branch(name: &str) -> RemoteBranch {
        RemoteBranch {
            remote: "origin".into(),
            name: name.into(),
            target: CommitId(String::new().into()),
        }
    }

    /// Where the push goes: remote, remote branch, and whether it becomes
    /// the upstream. Always the local branch's tip.
    fn push_of(state: &HeadState) -> Option<(String, String, bool)> {
        state.push.as_ref().map(|push| {
            assert_eq!(push.local_branch, "feature");
            assert_eq!(push.head, CommitId("1".repeat(40).into()));
            (push.remote.clone(), push.branch.clone(), push.set_upstream)
        })
    }

    #[test]
    fn a_new_branch_is_pushed_with_its_upstream_set() {
        let repo = repo(vec![branch(None, 0)], vec![remote_branch("main")]);
        let state = head_state(&repo, None, "main").expect("a branch is checked out");
        assert_eq!(state.head, "feature");
        assert_eq!(state.unpushed, None);
        assert_eq!(
            push_of(&state),
            Some(("origin".into(), "feature".into(), true))
        );
    }

    #[test]
    fn a_branch_tracking_main_is_pushed_as_itself_never_onto_main() {
        let repo = repo(
            vec![branch(Some(("origin", "main")), 2)],
            vec![remote_branch("main")],
        );
        let state = head_state(&repo, None, "").expect("checked out");
        assert_eq!(state.head, "feature");
        assert_eq!(
            push_of(&state),
            Some(("origin".into(), "feature".into(), true))
        );
    }

    #[test]
    fn a_branch_started_from_a_shared_branch_never_pushes_onto_it() {
        let repo = repo(
            vec![branch(Some(("origin", "release")), 3)],
            vec![remote_branch("release"), remote_branch("main")],
        );
        let state = head_state(&repo, None, "main").expect("checked out");
        assert_eq!(state.head, "feature");
        assert_eq!(
            push_of(&state),
            Some(("origin".into(), "feature".into(), true))
        );
    }

    #[test]
    fn a_differently_named_upstream_of_its_own_heads_it_unless_it_is_the_base() {
        let repo = repo(
            vec![branch(Some(("origin", "me/feature")), 1)],
            vec![remote_branch("me/feature"), remote_branch("main")],
        );
        let state = head_state(&repo, None, "main").expect("checked out");
        assert_eq!(state.head, "me/feature");
        assert_eq!(state.remote_branch, "me/feature");
        assert_eq!(
            push_of(&state),
            Some(("origin".into(), "me/feature".into(), false))
        );
        // Opened into that branch, it's the base: the branch goes up as itself.
        let state = head_state(&repo, None, "me/feature").expect("checked out");
        assert_eq!(state.head, "feature");
        assert_eq!(
            push_of(&state),
            Some(("origin".into(), "feature".into(), true))
        );
    }

    #[test]
    fn an_existing_pull_request_is_matched_by_owner_and_branch() {
        let pr = |head: &str, owner: &str| crate::github::PullRequestSummary {
            number: 1,
            title: String::new(),
            author: String::new(),
            head: head.into(),
            head_owner: owner.into(),
            base: "main".into(),
            is_draft: false,
            is_cross_repository: false,
            review: None,
            checks: Default::default(),
            review_requested: false,
            is_mine: false,
            updated_at: String::new(),
        };
        assert!(opened_from(&pr("feature", "owner"), "feature", "owner"));
        assert!(opened_from(&pr("feature", "Me"), "me:feature", "owner"));
        // Someone else's fork with the same branch name.
        assert!(!opened_from(&pr("main", "stranger"), "main", "owner"));
        assert!(!opened_from(&pr("feature", "owner"), "me:feature", "owner"));
        // gh without the owner: the name is all there is.
        assert!(opened_from(&pr("feature", ""), "feature", "owner"));
    }

    #[test]
    fn a_pushed_branch_pushes_only_what_github_lacks() {
        let ahead = repo(
            vec![branch(Some(("origin", "feature")), 2)],
            vec![remote_branch("feature"), remote_branch("main")],
        );
        let state = head_state(&ahead, None, "main").expect("checked out");
        assert_eq!(state.unpushed, Some(2));
        assert_eq!(state.push.map(|push| push.set_upstream), Some(false));

        let even = repo(
            vec![branch(Some(("origin", "feature")), 0)],
            vec![remote_branch("feature")],
        );
        let state = head_state(&even, None, "main").expect("checked out");
        assert_eq!(state.push, None);
        assert_eq!(state.remote_branch, "feature");
    }

    #[test]
    fn a_detached_head_has_no_head_state_and_bases_lead_with_the_default() {
        let mut detached = repo(vec![], vec![]);
        detached.head_branch = Loadable::Ready("HEAD".into());
        assert!(head_state(&detached, None, "main").is_none());

        let repo = repo(
            vec![],
            vec![
                remote_branch("HEAD"),
                remote_branch("zeta"),
                remote_branch("dev"),
                remote_branch("alpha"),
            ],
        );
        assert_eq!(base_candidates(&repo), vec!["dev", "alpha", "zeta"]);
    }
}
