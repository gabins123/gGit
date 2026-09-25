//! GitHub pull requests for the active repository: the open list, the selected
//! PR's details and diff, and the gh calls behind review and create.
//!
//! This lives in the view rather than the store: nothing in the reducer reads
//! it, and gh runs on background threads the way the signing-tools probe does.
//! A per-repo sequence number drops any result that arrives after a newer
//! request for the same thing.

use super::*;
use crate::github::{
    self, NewPullRequest, PrError, PullRequestDetail, PullRequestSummary, ReviewKind,
};
use gitcomet_state::model::SidebarMode;

/// A value gh is fetching or has fetched.
#[derive(Clone, Debug)]
pub(super) enum PrLoad<T> {
    Idle,
    Loading,
    Ready(T),
    Failed(PrError),
}

impl<T> Default for PrLoad<T> {
    fn default() -> Self {
        Self::Idle
    }
}

impl<T> PrLoad<T> {
    pub(super) fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(value) => Some(value),
            _ => None,
        }
    }
}

#[derive(Default)]
pub(super) struct RepoPullRequests {
    pub(super) list: PrLoad<Arc<Vec<PullRequestSummary>>>,
    pub(super) selected: Option<u64>,
    pub(super) detail: PrLoad<Arc<PullRequestDetail>>,
    /// The selected PR's merge base, once its commits are local.
    pub(super) diff_base: PrLoad<String>,
    /// Index into the selected PR's files.
    pub(super) selected_file: Option<usize>,
    /// Set while a review or create is with gh.
    pub(super) submitting: bool,
    /// gh's refusal of the last review or create, shown in its dialog.
    pub(super) submit_error: Option<String>,
    list_seq: u64,
    detail_seq: u64,
    diff_seq: u64,
}

#[derive(Default)]
pub(super) struct PullRequestsState {
    repos: FxHashMap<RepoId, RepoPullRequests>,
}

impl PullRequestsState {
    pub(super) fn repo(&self, repo_id: RepoId) -> Option<&RepoPullRequests> {
        self.repos.get(&repo_id)
    }

    fn repo_mut(&mut self, repo_id: RepoId) -> &mut RepoPullRequests {
        self.repos.entry(repo_id).or_default()
    }
}

/// The repository gh talks to for the active tab.
#[derive(Clone)]
pub(super) struct GitHubTarget {
    repo_id: RepoId,
    workdir: std::path::PathBuf,
    remote: String,
    pub(super) slug: String,
}

impl GitCometView {
    /// The active repository's github.com remote, or `None` when it has none.
    pub(super) fn github_target(&self) -> Option<GitHubTarget> {
        self.github_target_for(self.active_repo_id()?)
    }

    fn github_target_for(&self, repo_id: RepoId) -> Option<GitHubTarget> {
        let repo = self.state.repos.iter().find(|repo| repo.id == repo_id)?;
        let Loadable::Ready(remotes) = &repo.remotes else {
            return None;
        };
        let (remote, slug) = super::permalink::github_remote(remotes)?;
        Some(GitHubTarget {
            repo_id: repo.id,
            workdir: repo.spec.workdir.clone(),
            remote,
            slug,
        })
    }

    pub(super) fn active_pull_requests(&self) -> Option<&RepoPullRequests> {
        self.pull_requests.repo(self.active_repo_id()?)
    }

    /// Whether the Details panel is showing a pull request.
    pub(super) fn pull_request_details_active(&self) -> bool {
        self.state.sidebar_mode == SidebarMode::PullRequests
            && self
                .active_pull_requests()
                .is_some_and(|prs| prs.selected.is_some())
    }

    /// The sidebar and details panes are cached views, so a change to pull
    /// request state has to reach them explicitly.
    fn notify_pull_request_panes(&mut self, cx: &mut gpui::Context<Self>) {
        self.sidebar_pane.update(cx, |_, cx| cx.notify());
        self.details_pane.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    /// Loads the list the first time the tab shows it.
    pub(super) fn ensure_pull_requests_loaded(&mut self, cx: &mut gpui::Context<Self>) {
        let idle = self
            .active_pull_requests()
            .is_none_or(|prs| matches!(prs.list, PrLoad::Idle));
        if idle {
            self.refresh_pull_requests(cx);
        }
    }

    pub(super) fn refresh_pull_requests(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(target) = self.github_target() else {
            return;
        };
        let repo_id = target.repo_id;
        let entry = self.pull_requests.repo_mut(repo_id);
        entry.list_seq += 1;
        let seq = entry.list_seq;
        // A refresh keeps the current list on screen until the new one lands.
        if entry.list.ready().is_none() {
            entry.list = PrLoad::Loading;
        }
        let task =
            cx.background_spawn(async move { github::list_open(&target.workdir, &target.slug) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                let entry = this.pull_requests.repo_mut(repo_id);
                if entry.list_seq != seq {
                    return;
                }
                entry.list = match result {
                    Ok(list) => PrLoad::Ready(Arc::new(list)),
                    Err(err) => PrLoad::Failed(err),
                };
                this.notify_pull_request_panes(cx);
            });
        })
        .detach();
        self.notify_pull_request_panes(cx);
    }

    pub(super) fn select_pull_request(&mut self, number: u64, cx: &mut gpui::Context<Self>) {
        let Some(repo_id) = self.active_repo_id() else {
            return;
        };
        let entry = self.pull_requests.repo_mut(repo_id);
        if entry.selected == Some(number) && !matches!(entry.detail, PrLoad::Failed(_)) {
            return;
        }
        entry.selected = Some(number);
        entry.detail = PrLoad::Loading;
        entry.diff_base = PrLoad::Idle;
        entry.diff_seq += 1;
        // A diff asked for on the previous pull request must not steal focus.
        self.focus_diff_when_open = false;
        let entry = self.pull_requests.repo_mut(repo_id);
        entry.selected_file = None;
        entry.submit_error = None;
        self.load_pull_request_detail(number, cx);
    }

    /// Fetches `number`'s details, keeping whatever is on screen until they
    /// land; a review reloads this way so the panel does not flash empty.
    fn load_pull_request_detail(&mut self, number: u64, cx: &mut gpui::Context<Self>) {
        let Some(target) = self.github_target() else {
            return;
        };
        let repo_id = target.repo_id;
        let entry = self.pull_requests.repo_mut(repo_id);
        entry.detail_seq += 1;
        let seq = entry.detail_seq;
        let task =
            cx.background_spawn(async move { github::view(&target.workdir, &target.slug, number) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                let entry = this.pull_requests.repo_mut(repo_id);
                if entry.detail_seq != seq {
                    return;
                }
                entry.detail = match result {
                    Ok(detail) => PrLoad::Ready(Arc::new(detail)),
                    Err(err) => PrLoad::Failed(err),
                };
                this.notify_pull_request_panes(cx);
            });
        })
        .detach();
        self.notify_pull_request_panes(cx);
    }

    /// `j`/`k` in the list. With nothing selected it starts at either end.
    pub(super) fn select_adjacent_pull_request(
        &mut self,
        direction: i8,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let Some(prs) = self.active_pull_requests() else {
            return false;
        };
        let Some(list) = prs.list.ready() else {
            return false;
        };
        let current = prs
            .selected
            .and_then(|number| list.iter().position(|pr| pr.number == number));
        let next = match (current, direction < 0) {
            (Some(ix), false) => Some(ix + 1).filter(|ix| *ix < list.len()),
            (Some(ix), true) => ix.checked_sub(1),
            (None, false) => (!list.is_empty()).then_some(0),
            (None, true) => list.len().checked_sub(1),
        };
        let Some(number) = next.map(|ix| list[ix].number) else {
            return false;
        };
        self.select_pull_request(number, cx);
        true
    }

    /// Shows the selected PR's current file as a merge-base..head range diff.
    /// Its commits must already be local (`diff_base` ready). Only for the
    /// active repository: a fetch that lands after a tab switch just waits.
    fn show_pull_request_file(&mut self, repo_id: RepoId) -> bool {
        if self.active_repo_id() != Some(repo_id) {
            return false;
        }
        let Some(prs) = self.pull_requests.repo(repo_id) else {
            return false;
        };
        let (Some(detail), Some(merge_base), Some(file_ix)) =
            (prs.detail.ready(), prs.diff_base.ready(), prs.selected_file)
        else {
            return false;
        };
        let Some(file) = detail.files.get(file_ix) else {
            return false;
        };
        self.store.dispatch(Msg::SelectDiff {
            repo_id,
            target: DiffTarget::CommitRange {
                from_commit_id: CommitId(merge_base.as_str().into()),
                to_commit_id: Some(CommitId(detail.head_oid.as_str().into())),
                path: Some(std::path::PathBuf::from(&file.path)),
            },
        });
        true
    }

    /// Opens the selected PR's diff at `file_ix` (default: the current or first
    /// file). The first time, its commits are fetched by id — no ref or file in
    /// the repository changes — and the diff shows once they arrive. Returns
    /// whether a diff is on its way.
    pub(super) fn open_pull_request_diff(
        &mut self,
        file_ix: Option<usize>,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let Some(target) = self.github_target() else {
            return false;
        };
        let repo_id = target.repo_id;
        let entry = self.pull_requests.repo_mut(repo_id);
        let Some(detail) = entry.detail.ready().cloned() else {
            return false;
        };
        if detail.too_large_for_app() {
            self.push_toast(
                components::ToastKind::Warning,
                format!(
                    "#{} is too large to review here ({} files, +{} −{}). Press o to open it on GitHub.",
                    detail.number, detail.changed_files, detail.additions, detail.deletions
                ),
                cx,
            );
            return false;
        }
        if detail.files.is_empty() {
            return false;
        }
        let last = detail.files.len() - 1;
        entry.selected_file = Some(file_ix.or(entry.selected_file).unwrap_or(0).min(last));
        match entry.diff_base {
            PrLoad::Ready(_) => {
                self.show_pull_request_file(repo_id);
                self.notify_pull_request_panes(cx);
                return true;
            }
            // The file just picked opens when the fetch lands.
            PrLoad::Loading => {
                self.notify_pull_request_panes(cx);
                return true;
            }
            PrLoad::Idle | PrLoad::Failed(_) => {}
        }
        entry.diff_base = PrLoad::Loading;
        entry.diff_seq += 1;
        let seq = entry.diff_seq;
        let number = detail.number;
        let task = cx.background_spawn(async move {
            github::prepare_diff_range(
                &target.workdir,
                &target.remote,
                &detail.base_oid,
                &detail.head_oid,
            )
        });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                let entry = this.pull_requests.repo_mut(repo_id);
                if entry.diff_seq != seq {
                    return;
                }
                match result {
                    Ok(merge_base) => {
                        entry.diff_base = PrLoad::Ready(merge_base);
                        this.show_pull_request_file(repo_id);
                    }
                    Err(err) => {
                        let message = format!("Couldn't load the diff of #{number}: {err}");
                        entry.diff_base = PrLoad::Failed(err);
                        this.focus_diff_when_open = false;
                        this.push_toast(components::ToastKind::Error, message, cx);
                    }
                }
                this.notify_pull_request_panes(cx);
            });
        })
        .detach();
        self.notify_pull_request_panes(cx);
        true
    }

    /// `j`/`k` over the selected PR's files. Moves the diff along once one is
    /// open; before that it only moves the highlight.
    pub(super) fn select_adjacent_pull_request_file(
        &mut self,
        direction: i8,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let Some(repo_id) = self.active_repo_id() else {
            return false;
        };
        let entry = self.pull_requests.repo_mut(repo_id);
        let Some(len) = entry.detail.ready().map(|detail| detail.files.len()) else {
            return false;
        };
        let next = match (entry.selected_file, direction < 0) {
            (Some(ix), false) => Some(ix + 1).filter(|ix| *ix < len),
            (Some(ix), true) => ix.checked_sub(1),
            (None, false) => (len > 0).then_some(0),
            (None, true) => len.checked_sub(1),
        };
        let Some(next) = next else {
            return false;
        };
        entry.selected_file = Some(next);
        if entry.diff_base.ready().is_some() {
            self.show_pull_request_file(repo_id);
        }
        self.notify_pull_request_panes(cx);
        true
    }

    /// Opens the selected PR (or the repository's PR list) on GitHub, reusing
    /// the browser launch every other forge link goes through.
    pub(super) fn open_pull_request_on_github(&mut self, cx: &mut gpui::Context<Self>) -> bool {
        let Some(target) = self.github_target() else {
            return false;
        };
        let prs = self.pull_requests.repo(target.repo_id);
        let url = match (
            prs.and_then(|prs| prs.detail.ready()),
            prs.and_then(|prs| prs.selected),
        ) {
            (Some(detail), Some(number)) if detail.number == number => detail.url.clone(),
            (_, Some(number)) => format!("https://github.com/{}/pull/{number}", target.slug),
            (_, None) => format!("https://github.com/{}/pulls", target.slug),
        };
        platform_open::spawn_launch(
            cx,
            move || platform_open::open_url_blocking(&url),
            |this, result, cx| {
                if let Err(err) = result {
                    this.push_toast(
                        components::ToastKind::Error,
                        format!("Failed to open link: {err}"),
                        cx,
                    );
                    cx.notify();
                }
            },
        );
        true
    }

    /// Posts a review. Runs only from the review dialog's explicit submit.
    pub(super) fn submit_pull_request_review(
        &mut self,
        repo_id: RepoId,
        number: u64,
        kind: ReviewKind,
        body: String,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(target) = self.github_target_for(repo_id) else {
            return;
        };
        let repo_id = target.repo_id;
        let entry = self.pull_requests.repo_mut(repo_id);
        if entry.submitting {
            return;
        }
        entry.submitting = true;
        entry.submit_error = None;
        let slug = target.slug.clone();
        let task = cx.background_spawn(async move {
            github::review(&target.workdir, &target.slug, number, kind, &body)
        });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                let entry = this.pull_requests.repo_mut(repo_id);
                entry.submitting = false;
                match result {
                    Ok(()) => {
                        this.close_pull_request_prompt(cx);
                        let verb = match kind {
                            ReviewKind::Comment => "Commented on",
                            ReviewKind::Approve => "Approved",
                            ReviewKind::RequestChanges => "Requested changes on",
                        };
                        this.push_toast_with_link(
                            components::ToastKind::Success,
                            format!("{verb} #{number}"),
                            format!("https://github.com/{slug}/pull/{number}"),
                            "View on GitHub".to_string(),
                            cx,
                        );
                        // Pick up the new review decision, unless the user has
                        // moved on to another pull request meanwhile.
                        if this.pull_requests.repo_mut(repo_id).selected == Some(number)
                            && this.active_repo_id() == Some(repo_id)
                        {
                            this.load_pull_request_detail(number, cx);
                        }
                        this.refresh_pull_requests(cx);
                    }
                    Err(err) => entry.submit_error = Some(err.to_string()),
                }
                this.notify_pull_request_panes(cx);
                this.popover_host.update(cx, |_, cx| cx.notify());
            });
        })
        .detach();
    }

    /// Opens a pull request. Runs only from the create dialog's explicit
    /// submit, and never pushes.
    pub(super) fn submit_new_pull_request(
        &mut self,
        repo_id: RepoId,
        pr: NewPullRequest,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(target) = self.github_target_for(repo_id) else {
            return;
        };
        let repo_id = target.repo_id;
        let entry = self.pull_requests.repo_mut(repo_id);
        if entry.submitting {
            return;
        }
        entry.submitting = true;
        entry.submit_error = None;
        let task =
            cx.background_spawn(async move { github::create(&target.workdir, &target.slug, &pr) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                let entry = this.pull_requests.repo_mut(repo_id);
                entry.submitting = false;
                match result {
                    Ok(url) => {
                        this.close_pull_request_prompt(cx);
                        this.push_toast_with_link(
                            components::ToastKind::Success,
                            "Pull request created".to_string(),
                            url,
                            "View on GitHub".to_string(),
                            cx,
                        );
                        this.refresh_pull_requests(cx);
                    }
                    Err(err) => entry.submit_error = Some(err.to_string()),
                }
                this.notify_pull_request_panes(cx);
                this.popover_host.update(cx, |_, cx| cx.notify());
            });
        })
        .detach();
    }

    /// Closes the review or create dialog after gh accepted it, handing focus
    /// back where the dialog came from. Leaves any other dialog alone.
    fn close_pull_request_prompt(&mut self, cx: &mut gpui::Context<Self>) {
        let host = self.popover_host.clone();
        let window_handle = self.window_handle;
        cx.defer(move |cx| {
            let _ = window_handle.update(cx, |_, window, cx| {
                host.update(cx, |host, cx| {
                    if host.pull_request_prompt_open() {
                        host.close_popover_and_restore_focus(window, cx);
                    }
                });
            });
        });
    }

    #[cfg(test)]
    pub(super) fn seed_pull_requests_for_test(
        &mut self,
        repo_id: RepoId,
        list: Vec<PullRequestSummary>,
        selected: Option<u64>,
    ) {
        let entry = self.pull_requests.repo_mut(repo_id);
        entry.list = PrLoad::Ready(Arc::new(list));
        entry.selected = selected;
    }

    /// Clears a stale gh refusal when a review or create dialog opens.
    pub(super) fn clear_pull_request_submit_error(&mut self) {
        if let Some(repo_id) = self.active_repo_id() {
            self.pull_requests.repo_mut(repo_id).submit_error = None;
        }
    }
}
