//! GitHub pull requests for the active repository: the open list, the selected
//! PR's details and diff, and the gh calls behind review and create.
//!
//! This lives in the view rather than the store: nothing in the reducer reads
//! it, and gh runs on background threads the way the signing-tools probe does.
//! A per-repo sequence number drops any result that arrives after a newer
//! request for the same thing.

use super::*;
use crate::github::{
    self, MergeRequest, NewPullRequest, PrError, PullRequestDetail, PullRequestSummary, ReviewKind,
};
use gitcomet_state::model::SidebarMode;

/// A value gh is fetching or has fetched.
#[derive(Clone, Debug, Default)]
pub(super) enum PrLoad<T> {
    #[default]
    Idle,
    Loading,
    Ready(T),
    Failed(PrError),
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
    /// gh's refusal of the last review, create or merge, shown in its dialog.
    pub(super) submit_error: Option<String>,
    /// The pull request `gh pr checkout` is working on.
    checking_out: Option<u64>,
    list_seq: u64,
    detail_seq: u64,
    diff_seq: u64,
}

/// Why a branch can't head a new pull request yet.
pub(super) enum HeadProblem {
    Detached,
    /// The branch has no upstream on GitHub: GitHub has never seen it.
    NotPushed(String),
}

/// The branch a new pull request comes from, as `gh pr create --head` and
/// GitHub's compare page take it: `branch` (default: the checked-out one) by
/// its upstream, which must exist on the remote. Creating never pushes, so a
/// branch without one can't head a pull request yet.
pub(super) fn pull_request_head(
    repo: &RepoState,
    branch: Option<&str>,
) -> Result<String, HeadProblem> {
    let name = match (branch, &repo.head_branch) {
        (Some(name), _) => name.to_string(),
        (None, Loadable::Ready(head)) if head != "HEAD" => head.clone(),
        _ => return Err(HeadProblem::Detached),
    };
    let upstream = repo
        .branches
        .ready()
        .and_then(|branches| branches.iter().find(|candidate| candidate.name == name))
        .and_then(|branch| branch.upstream.clone());
    // Configured isn't pushed: the remote-tracking ref has to exist.
    let live = upstream.as_ref().is_some_and(|upstream| {
        repo.remote_branches.ready().is_some_and(|remote_branches| {
            remote_branches.iter().any(|candidate| {
                candidate.remote == upstream.remote && candidate.name == upstream.branch
            })
        })
    });
    match upstream {
        Some(upstream) if live => Ok(remote_branch_head(repo, &upstream.remote, &upstream.branch)),
        _ => Err(HeadProblem::NotPushed(name)),
    }
}

/// `branch` on `remote` as a pull request head: bare on the pull requests' own
/// GitHub remote, `owner:branch` on another GitHub repository (a fork).
pub(super) fn remote_branch_head(repo: &RepoState, remote: &str, branch: &str) -> String {
    let remotes = repo
        .remotes
        .ready()
        .map(|remotes| remotes.as_slice())
        .unwrap_or(&[]);
    let target = super::permalink::github_remote(remotes).map(|(name, _)| name);
    if target.as_deref() == Some(remote) {
        return branch.to_string();
    }
    let owner = remotes
        .iter()
        .find(|candidate| candidate.name == remote)
        .and_then(|candidate| super::permalink::github_slug(candidate.url.as_deref()?))
        .and_then(|slug| slug.split('/').next().map(str::to_string));
    match owner {
        Some(owner) => format!("{owner}:{branch}"),
        None => branch.to_string(),
    }
}

/// The dialog a gh submit came from, so its outcome reaches that dialog and
/// no other.
#[derive(Clone, Copy)]
enum PrDialog {
    Review(u64),
    Create,
    Merge(u64),
}

impl PrDialog {
    fn matches(self, repo_id: RepoId, kind: &PopoverKind) -> bool {
        match (self, kind) {
            (
                Self::Review(number),
                PopoverKind::PullRequestReview {
                    repo_id: open,
                    number: open_number,
                    ..
                },
            )
            | (
                Self::Merge(number),
                PopoverKind::MergePullRequest {
                    repo_id: open,
                    number: open_number,
                    ..
                },
            ) => *open == repo_id && *open_number == number,
            (Self::Create, PopoverKind::CreatePullRequest { repo_id: open, .. }) => {
                *open == repo_id
            }
            _ => false,
        }
    }
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
    pub(super) repo_id: RepoId,
    workdir: std::path::PathBuf,
    remote: String,
    pub(super) slug: String,
}

impl GitCometView {
    /// The active repository's github.com remote, or `None` when it has none.
    pub(super) fn github_target(&self) -> Option<GitHubTarget> {
        self.github_target_for(self.active_repo_id()?)
    }

    pub(super) fn github_target_for(&self, repo_id: RepoId) -> Option<GitHubTarget> {
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
    /// request state has to reach them explicitly; so does an open dialog
    /// that shows it.
    pub(super) fn notify_pull_request_panes(&mut self, cx: &mut gpui::Context<Self>) {
        self.sidebar_pane.update(cx, |_, cx| cx.notify());
        self.details_pane.update(cx, |_, cx| cx.notify());
        // Deferred: this also runs inside the host's own submit handler.
        let host = self.popover_host.clone();
        cx.defer(move |cx| host.update(cx, |_, cx| cx.notify()));
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
        // `j`/`k` can pass many pull requests a second, and each load is
        // several GitHub requests: gh runs for the one the selection settles on.
        entry.detail_seq += 1;
        let seq = entry.detail_seq;
        cx.spawn(async move |view, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(200))
                .await;
            let _ = view.update(cx, |this, cx| {
                let settled = this
                    .pull_requests
                    .repo(repo_id)
                    .is_some_and(|prs| prs.detail_seq == seq && prs.selected == Some(number));
                if settled {
                    this.load_pull_request_detail(repo_id, number, cx);
                }
            });
        })
        .detach();
    }

    /// Fetches `number`'s details, keeping whatever is on screen until they
    /// land; a review reloads this way so the panel does not flash empty.
    fn load_pull_request_detail(
        &mut self,
        repo_id: RepoId,
        number: u64,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(target) = self.github_target_for(repo_id) else {
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

    /// `o` on a branch, as in lazygit: GitHub's page for opening a pull request
    /// from it, in the browser.
    pub(super) fn open_pull_request_compare(
        &mut self,
        target: &super::branch_sidebar::BranchMenuTarget,
        cx: &mut gpui::Context<Self>,
    ) {
        use super::branch_sidebar::BranchMenuTarget;
        let Some(github) = self.github_target() else {
            self.push_toast(
                components::ToastKind::Warning,
                "Pull requests need a github.com remote.".to_string(),
                cx,
            );
            return;
        };
        let Some(repo) = self.active_repo() else {
            return;
        };
        let head = match target {
            BranchMenuTarget::Local { name } => pull_request_head(repo, Some(name))
                .map_err(|_| format!("{name} isn't on GitHub yet; push it first.")),
            BranchMenuTarget::Remote { remote, branch } => {
                Ok(remote_branch_head(repo, remote, branch))
            }
        };
        match head {
            Ok(head) => self.open_in_browser(
                super::permalink::github_compare_url(&github.slug, None, &head),
                cx,
            ),
            Err(message) => self.push_toast(components::ToastKind::Warning, message, cx),
        }
    }

    /// The browser launch every other forge link goes through.
    pub(super) fn open_in_browser(&mut self, url: String, cx: &mut gpui::Context<Self>) {
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
        self.open_in_browser(url, cx);
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
        // A review in progress goes up with its line comments, pinned to the
        // commit they were written against.
        let pending = self
            .review_of(repo_id, number)
            .map(|review| (review.draft.head_oid.clone(), review.draft.comments.clone()));
        let in_review = pending.is_some();
        let submitted: Vec<crate::github::ReviewComment> = pending
            .as_ref()
            .map(|(_, comments)| comments.clone())
            .unwrap_or_default();
        let comment_count = submitted.len();
        let task = cx.background_spawn(async move {
            match pending {
                Some((head_oid, comments)) if !comments.is_empty() => github::create_review(
                    &target.workdir,
                    &target.slug,
                    number,
                    &head_oid,
                    kind,
                    &body,
                    &comments,
                ),
                _ => github::review(&target.workdir, &target.slug, number, kind, &body),
            }
        });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                let entry = this.pull_requests.repo_mut(repo_id);
                entry.submitting = false;
                match result {
                    Ok(()) => {
                        this.close_pull_request_prompt(repo_id, PrDialog::Review(number), cx);
                        let verb = match kind {
                            ReviewKind::Comment => "Commented on",
                            ReviewKind::Approve => "Approved",
                            ReviewKind::RequestChanges => "Requested changes on",
                        };
                        let with = match comment_count {
                            0 => String::new(),
                            1 => " · 1 comment".to_string(),
                            n => format!(" · {n} comments"),
                        };
                        if in_review {
                            this.finish_review(repo_id, number, &submitted, cx);
                        }
                        this.push_toast_with_link(
                            components::ToastKind::Success,
                            format!("{verb} #{number}{with}"),
                            format!("https://github.com/{slug}/pull/{number}"),
                            "View on GitHub".to_string(),
                            cx,
                        );
                        // Pick up the new review decision, unless the user has
                        // moved on to another pull request meanwhile.
                        this.reload_pull_request(repo_id, number, cx);
                    }
                    Err(err) => this.report_pull_request_error(
                        repo_id,
                        PrDialog::Review(number),
                        format!("Couldn't post the review on #{number}: {err}"),
                        err.to_string(),
                        cx,
                    ),
                }
                this.notify_pull_request_panes(cx);
                this.popover_host.update(cx, |_, cx| cx.notify());
            });
        })
        .detach();
    }

    /// Merges on GitHub. Runs only from the merge dialog's explicit confirm,
    /// and only onto the head commit the details showed.
    pub(super) fn submit_pull_request_merge(
        &mut self,
        repo_id: RepoId,
        number: u64,
        method: github::MergeMethod,
        delete_branch: bool,
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
        let Some(head_oid) = entry
            .detail
            .ready()
            .filter(|detail| detail.number == number)
            .map(|detail| detail.head_oid.clone())
        else {
            entry.submit_error = Some(format!(
                "#{number} is still loading; merge once its details show."
            ));
            self.notify_pull_request_panes(cx);
            return;
        };
        entry.submitting = true;
        entry.submit_error = None;
        let slug = target.slug.clone();
        let request = MergeRequest {
            method,
            delete_branch,
            head_oid,
        };
        let task = cx.background_spawn(async move {
            github::merge(&target.workdir, &target.slug, number, &request)
        });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                this.pull_requests.repo_mut(repo_id).submitting = false;
                match result {
                    Ok(()) => {
                        this.close_pull_request_prompt(repo_id, PrDialog::Merge(number), cx);
                        this.push_toast_with_link(
                            components::ToastKind::Success,
                            format!("Merged #{number}"),
                            format!("https://github.com/{slug}/pull/{number}"),
                            "View on GitHub".to_string(),
                            cx,
                        );
                    }
                    // gh also fails when the merge went through but deleting
                    // the branch didn't, so the state is reloaded either way.
                    Err(err) => this.report_pull_request_error(
                        repo_id,
                        PrDialog::Merge(number),
                        format!("Merging #{number}: gh reported: {err}"),
                        format!("gh reported: {err}"),
                        cx,
                    ),
                }
                this.reload_pull_request(repo_id, number, cx);
            });
        })
        .detach();
    }

    /// After a review or merge: the list, and the details if the user is
    /// still on that pull request.
    fn reload_pull_request(&mut self, repo_id: RepoId, number: u64, cx: &mut gpui::Context<Self>) {
        if self.active_repo_id() != Some(repo_id) {
            return;
        }
        if self.pull_requests.repo_mut(repo_id).selected == Some(number) {
            self.load_pull_request_detail(repo_id, number, cx);
        }
        self.refresh_pull_requests(cx);
    }

    /// gh's refusal goes into the dialog it came from, or to a toast once that
    /// dialog is gone.
    fn report_pull_request_error(
        &mut self,
        repo_id: RepoId,
        dialog: PrDialog,
        toast: String,
        in_dialog: String,
        cx: &mut gpui::Context<Self>,
    ) {
        let open = self
            .popover_host
            .read(cx)
            .open_popover_kind()
            .is_some_and(|kind| dialog.matches(repo_id, kind));
        if open {
            self.pull_requests.repo_mut(repo_id).submit_error = Some(in_dialog);
        } else {
            self.push_toast(components::ToastKind::Error, toast, cx);
        }
        self.notify_pull_request_panes(cx);
    }

    /// `space`: checks the selected pull request out into a local branch.
    /// Local only; git refuses it over conflicting uncommitted changes.
    pub(super) fn checkout_pull_request(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(target) = self.github_target() else {
            return;
        };
        let repo_id = target.repo_id;
        let entry = self.pull_requests.repo_mut(repo_id);
        let Some(number) = entry.selected else {
            return;
        };
        if let Some(busy) = entry.checking_out {
            self.push_toast(
                components::ToastKind::Warning,
                format!("Still checking out #{busy}…"),
                cx,
            );
            return;
        }
        // A fork's branch name means nothing here: under it gh would
        // fast-forward a same-named local branch (say `develop`) to the
        // contributor's commits. Unless the details show the pull request is
        // from this repository, it gets a branch of its own.
        let same_repo = entry
            .detail
            .ready()
            .is_some_and(|detail| detail.number == number && !detail.is_cross_repository);
        let branch = (!same_repo).then(|| format!("pr/{number}"));
        entry.checking_out = Some(number);
        let task = cx.background_spawn(async move {
            github::checkout(&target.workdir, &target.slug, number, branch.as_deref())
        });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                this.pull_requests.repo_mut(repo_id).checking_out = None;
                match result {
                    Ok(()) => {
                        this.push_toast(
                            components::ToastKind::Success,
                            format!("Checked out #{number}"),
                            cx,
                        );
                        this.store.dispatch(Msg::ReloadRepo { repo_id });
                    }
                    Err(err) => this.push_toast(
                        components::ToastKind::Error,
                        format!("Couldn't check out #{number}: {err}"),
                        cx,
                    ),
                }
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
                        this.close_pull_request_prompt(repo_id, PrDialog::Create, cx);
                        this.push_toast_with_link(
                            components::ToastKind::Success,
                            "Pull request created".to_string(),
                            url,
                            "View on GitHub".to_string(),
                            cx,
                        );
                        this.refresh_pull_requests(cx);
                    }
                    Err(err) => this.report_pull_request_error(
                        repo_id,
                        PrDialog::Create,
                        format!("Couldn't create the pull request: {err}"),
                        err.to_string(),
                        cx,
                    ),
                }
                this.notify_pull_request_panes(cx);
            });
        })
        .detach();
    }

    /// Closes the dialog a gh submit came from after gh accepted it, handing
    /// focus back where the dialog came from. Leaves any other dialog alone.
    fn close_pull_request_prompt(
        &mut self,
        repo_id: RepoId,
        dialog: PrDialog,
        cx: &mut gpui::Context<Self>,
    ) {
        let host = self.popover_host.clone();
        let window_handle = self.window_handle;
        cx.defer(move |cx| {
            let _ = window_handle.update(cx, |_, window, cx| {
                host.update(cx, |host, cx| {
                    let open = host
                        .open_popover_kind()
                        .is_some_and(|kind| dialog.matches(repo_id, kind));
                    if open {
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

    /// The selected pull request's details, and its commits as already local
    /// at `merge_base`, so tests never run gh or git.
    #[cfg(test)]
    pub(super) fn seed_pull_request_detail_for_test(
        &mut self,
        repo_id: RepoId,
        detail: PullRequestDetail,
        merge_base: String,
    ) {
        let entry = self.pull_requests.repo_mut(repo_id);
        entry.detail = PrLoad::Ready(Arc::new(detail));
        entry.diff_base = PrLoad::Ready(merge_base);
    }

    /// Clears a stale gh refusal when a review or create dialog opens.
    pub(super) fn clear_pull_request_submit_error(&mut self) {
        if let Some(repo_id) = self.active_repo_id() {
            self.pull_requests.repo_mut(repo_id).submit_error = None;
        }
    }
}
