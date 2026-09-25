//! Review mode: a pull request read file by file, with line comments that
//! wait as a pending review until one submit posts them all.
//!
//! The pending review lives on this computer, one JSON file per repository
//! and pull request, pinned to the head commit the diff shows. Nothing reaches
//! GitHub before the submit dialog's explicit confirm.

use super::panel_focus::FocusPanel;
use super::*;
use crate::github::{ReviewAnchor, ReviewComment, ReviewSide};
use gitcomet_state::model::SidebarMode;

/// A pending review as saved on disk.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct ReviewDraft {
    pub(super) repo: String,
    pub(super) number: u64,
    /// The head commit the comments are anchored to.
    pub(super) head_oid: String,
    pub(super) comments: Vec<ReviewComment>,
    /// Paths of the files marked viewed.
    #[serde(default)]
    pub(super) viewed: std::collections::BTreeSet<String>,
}

impl ReviewDraft {
    fn file(repo: &str, number: u64) -> Option<std::path::PathBuf> {
        let dir = gitcomet_state::session::review_drafts_dir()?;
        // `owner/name` becomes `owner~name`: one flat file per pull request.
        let name: String = repo
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                    c
                } else {
                    '~'
                }
            })
            .collect();
        Some(dir.join(format!("{name}~{number}.json")))
    }

    /// The saved draft, if any. A file that can't be read back is set aside
    /// (never overwritten) and reported, so its comments aren't lost quietly.
    fn load(repo: &str, number: u64) -> Result<Option<Self>, String> {
        let Some(file) = Self::file(repo, number) else {
            return Ok(None);
        };
        let text = match std::fs::read_to_string(&file) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(format!(
                    "Couldn't read the pending review of #{number}: {err}"
                ));
            }
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(draft) if draft.repo == repo && draft.number == number => Ok(Some(draft)),
            parsed => {
                let aside = file.with_extension("json.unreadable");
                let _ = std::fs::rename(&file, &aside);
                let why = match parsed {
                    Err(err) => err.to_string(),
                    Ok(_) => "it belongs to another pull request".to_string(),
                };
                Err(format!(
                    "The pending review of #{number} couldn't be read ({why}); it was kept as {}.",
                    aside.display()
                ))
            }
        }
    }

    /// The file's next contents: `None` when there's nothing left to keep.
    fn contents(&self) -> Result<Option<Vec<u8>>, String> {
        if self.comments.is_empty() && self.viewed.is_empty() {
            return Ok(None);
        }
        serde_json::to_vec_pretty(self)
            .map(Some)
            .map_err(|err| err.to_string())
    }
}

/// Writes or removes a draft file.
fn write_draft(file: &std::path::Path, contents: Option<&[u8]>) -> std::io::Result<()> {
    match contents {
        Some(bytes) => gitcomet_core::fs_utils::write_private_file(file, bytes),
        None => match std::fs::remove_file(file) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        },
    }
}

/// The pull request being reviewed and where the review is.
pub(super) struct ReviewMode {
    pub(super) repo_id: RepoId,
    pub(super) number: u64,
    pub(super) title: String,
    /// The pull request's changed files, in GitHub's order.
    pub(super) files: Vec<String>,
    pub(super) file_ix: usize,
    pub(super) draft: ReviewDraft,
    /// The row picked in the Your review panel.
    pub(super) selected_comment: Option<usize>,
    /// Pending comments were written on an older head than the one shown.
    pub(super) head_moved: bool,
    /// A line to put the cursor on once its file's diff is on screen.
    pending_jump: Option<(ReviewSide, u32)>,
    /// The shown file hasn't had its cursor placed yet.
    needs_cursor: bool,
    /// `d` pressed once on a comment in Your review, waiting for the second.
    armed_delete: Option<usize>,
    /// Draft writes run off the UI thread; each carries a sequence number and
    /// only a newer one than the last written lands, so the file always ends
    /// on the latest draft.
    write_seq: u64,
    written_seq: std::sync::Arc<std::sync::Mutex<u64>>,
}

impl ReviewMode {
    pub(super) fn current_path(&self) -> Option<&str> {
        self.files.get(self.file_ix).map(String::as_str)
    }

    pub(super) fn comments_on(&self, path: &str) -> usize {
        self.draft
            .comments
            .iter()
            .filter(|comment| comment.anchor.path == path)
            .count()
    }

    pub(super) fn files_commented(&self) -> usize {
        let paths: std::collections::BTreeSet<&str> = self
            .draft
            .comments
            .iter()
            .map(|comment| comment.anchor.path.as_str())
            .collect();
        paths.len()
    }
}

impl GitCometView {
    /// Review mode, while it is on for the active repository and its Pull
    /// requests tab is showing. Another tab has its own keys back.
    pub(super) fn active_review(&self) -> Option<&ReviewMode> {
        self.review.as_ref().filter(|review| {
            Some(review.repo_id) == self.active_repo_id()
                && self.state.sidebar_mode == SidebarMode::PullRequests
        })
    }

    /// The review of `number` in `repo_id`, if it's the one in progress.
    pub(super) fn review_of(&self, repo_id: RepoId, number: u64) -> Option<&ReviewMode> {
        self.review
            .as_ref()
            .filter(|review| review.repo_id == repo_id && review.number == number)
    }

    /// Whether the diff on screen is the review's current file at the head
    /// its comments are pinned to: only then do cursor rows match a comment's
    /// file and lines.
    fn review_diff_shown(&self) -> bool {
        let Some(review) = self.active_review() else {
            return false;
        };
        let Some(path) = review.current_path() else {
            return false;
        };
        self.active_repo()
            .and_then(|repo| repo.diff_state.diff_target.as_ref())
            .is_some_and(|target| match target {
                DiffTarget::CommitRange {
                    to_commit_id: Some(head),
                    path: Some(shown),
                    ..
                } => head.as_ref() == review.draft.head_oid && shown == std::path::Path::new(path),
                _ => false,
            })
    }

    /// `r` on the Pull requests tab: reviews the selected pull request,
    /// picking up a pending review of it where it was left.
    pub(super) fn start_review(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(target) = self.github_target() else {
            return;
        };
        let repo_id = target.repo_id;
        let Some(prs) = self.pull_requests.repo(repo_id) else {
            return;
        };
        let Some(number) = prs.selected else {
            return;
        };
        let Some(detail) = prs
            .detail
            .ready()
            .filter(|detail| detail.number == number)
            .cloned()
        else {
            self.push_toast(
                components::ToastKind::Warning,
                format!("#{number} is still loading."),
                cx,
            );
            return;
        };
        if detail.too_large_for_app() || detail.files.is_empty() {
            self.push_toast(
                components::ToastKind::Warning,
                format!("#{number} is too large to review here. Press o to open it on GitHub."),
                cx,
            );
            return;
        }
        let draft = match ReviewDraft::load(&target.slug, number) {
            Ok(draft) => draft,
            Err(message) => {
                self.push_toast(components::ToastKind::Error, message, cx);
                None
            }
        };
        let draft = draft.unwrap_or_else(|| ReviewDraft {
            repo: target.slug.clone(),
            number,
            head_oid: detail.head_oid.clone(),
            ..ReviewDraft::default()
        });
        let files: Vec<String> = detail.files.iter().map(|file| file.path.clone()).collect();
        let file_ix = files
            .iter()
            .position(|path| !draft.viewed.contains(path))
            .unwrap_or(0);
        self.review = Some(ReviewMode {
            repo_id,
            number,
            title: detail.title.clone(),
            files,
            file_ix,
            draft,
            selected_comment: None,
            head_moved: false,
            pending_jump: None,
            needs_cursor: true,
            armed_delete: None,
            write_seq: 0,
            written_seq: Default::default(),
        });
        self.main_pane.update(cx, |pane, cx| {
            pane.review_active = true;
            cx.notify();
        });
        self.review_open_file(file_ix, cx);
        self.diff_return_panel = FocusPanel::Sidebar;
        self.focus_diff_when_open = true;
    }

    /// The diff shows the pull request's current head; a draft started on an
    /// older one moves to it, so what's on screen and what's posted agree. Its
    /// comments keep their line numbers, which may now point elsewhere: the
    /// user is told to check them.
    fn review_follow_head(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(review) = self.review.as_ref() else {
            return;
        };
        let head = self
            .pull_requests
            .repo(review.repo_id)
            .and_then(|prs| prs.detail.ready())
            .filter(|detail| detail.number == review.number)
            .map(|detail| detail.head_oid.clone());
        let Some(head) = head.filter(|head| *head != review.draft.head_oid) else {
            return;
        };
        let number = review.number;
        let had_comments = !review.draft.comments.is_empty();
        if let Some(review) = self.review.as_mut() {
            review.draft.head_oid = head;
            review.head_moved |= had_comments;
        }
        self.save_review(cx);
        if had_comments {
            self.push_toast(
                components::ToastKind::Warning,
                format!(
                    "#{number} has new commits since your pending comments were written. Check their lines (Your review, enter) before submitting."
                ),
                cx,
            );
        }
    }

    /// `q`: back to the pull request list. The pending review stays saved.
    pub(super) fn leave_review(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let Some(review) = self.review.take() else {
            return;
        };
        self.main_pane.update(cx, |pane, cx| {
            pane.review_active = false;
            pane.review_marks.clear();
            cx.notify();
        });
        if self.diff_is_open() {
            self.store.dispatch(Msg::ClearDiffSelection {
                repo_id: review.repo_id,
            });
        }
        self.focus_panel(FocusPanel::Sidebar, window, cx);
        self.notify_pull_request_panes(cx);
    }

    /// After gh accepted the review: exactly the comments that went up leave
    /// the draft. Any added while it was posting stay pending, and review mode
    /// stays for them; otherwise the draft goes and review mode closes.
    pub(super) fn finish_review(
        &mut self,
        repo_id: RepoId,
        number: u64,
        submitted: &[ReviewComment],
        cx: &mut gpui::Context<Self>,
    ) {
        let is_submitted = |comment: &ReviewComment| submitted.contains(comment);
        let Some(review) = self
            .review
            .as_mut()
            .filter(|review| review.repo_id == repo_id && review.number == number)
        else {
            // Left while it was posting: the draft on disk still has them.
            let repo = self.github_target_for(repo_id).map(|target| target.slug);
            if let Some(repo) = repo
                && let Ok(Some(mut draft)) = ReviewDraft::load(&repo, number)
            {
                draft.comments.retain(|comment| !is_submitted(comment));
                if draft.comments.is_empty() {
                    draft.viewed.clear();
                }
                if let (Some(file), Ok(contents)) =
                    (ReviewDraft::file(&repo, number), draft.contents())
                {
                    let _ = write_draft(&file, contents.as_deref());
                }
            }
            return;
        };
        review
            .draft
            .comments
            .retain(|comment| !is_submitted(comment));
        review.selected_comment = None;
        let remaining = review.draft.comments.len();
        if remaining > 0 {
            self.save_review(cx);
            self.sync_review_marks(cx);
            self.notify_pull_request_panes(cx);
            self.push_toast(
                components::ToastKind::Warning,
                format!(
                    "{remaining} comment{} added while it was posting {} still pending.",
                    if remaining == 1 { "" } else { "s" },
                    if remaining == 1 { "is" } else { "are" }
                ),
                cx,
            );
            return;
        }
        review.draft.viewed.clear();
        self.save_review(cx);
        self.review = None;
        self.main_pane.update(cx, |pane, cx| {
            pane.review_active = false;
            pane.review_marks.clear();
            cx.notify();
        });
        if self.active_repo_id() == Some(repo_id) {
            if self.diff_is_open() {
                self.store.dispatch(Msg::ClearDiffSelection { repo_id });
            }
            let window_handle = self.window_handle;
            let view = cx.entity();
            cx.defer(move |cx| {
                let _ = window_handle.update(cx, |_, window, cx| {
                    view.update(cx, |this, cx| {
                        this.focus_panel(FocusPanel::Sidebar, window, cx)
                    });
                });
            });
        }
        self.notify_pull_request_panes(cx);
    }

    /// Shows file `ix` of the review, fetching the pull request's commits the
    /// first time. Focus stays where it is.
    pub(super) fn review_open_file(&mut self, ix: usize, cx: &mut gpui::Context<Self>) {
        let Some(review) = self.review.as_mut() else {
            return;
        };
        if ix >= review.files.len() {
            return;
        }
        review.file_ix = ix;
        review.pending_jump = None;
        review.needs_cursor = true;
        self.review_follow_head(cx);
        self.sync_review_marks(cx);
        self.open_pull_request_diff(Some(ix), cx);
        self.notify_pull_request_panes(cx);
    }

    fn review_step_file(&mut self, direction: i8, cx: &mut gpui::Context<Self>) {
        let Some(review) = self.review.as_ref() else {
            return;
        };
        let next = if direction < 0 {
            review.file_ix.checked_sub(1)
        } else {
            Some(review.file_ix + 1).filter(|ix| *ix < review.files.len())
        };
        if let Some(next) = next {
            self.review_open_file(next, cx);
        }
    }

    /// `space`: marks the shown file viewed (or not) and, when marking, goes on
    /// to the next file not yet viewed.
    fn review_toggle_viewed(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(review) = self.review.as_mut() else {
            return;
        };
        let Some(path) = review.current_path().map(str::to_string) else {
            return;
        };
        let now_viewed = review.draft.viewed.insert(path.clone());
        if !now_viewed {
            review.draft.viewed.remove(&path);
        }
        let next = now_viewed
            .then(|| {
                let from = review.file_ix;
                (1..review.files.len())
                    .map(|step| (from + step) % review.files.len())
                    .find(|ix| !review.draft.viewed.contains(&review.files[*ix]))
            })
            .flatten();
        self.save_review(cx);
        match next {
            Some(next) => self.review_open_file(next, cx),
            None => self.notify_pull_request_panes(cx),
        }
    }

    /// Saves the draft off the UI thread; a failure is reported.
    fn save_review(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(review) = self.review.as_mut() else {
            return;
        };
        let Some(file) = ReviewDraft::file(&review.draft.repo, review.draft.number) else {
            return;
        };
        let contents = match review.draft.contents() {
            Ok(contents) => contents,
            Err(err) => {
                self.push_toast(
                    components::ToastKind::Error,
                    format!("Couldn't save the pending review: {err}"),
                    cx,
                );
                return;
            }
        };
        review.write_seq += 1;
        let seq = review.write_seq;
        let written = review.written_seq.clone();
        let task = cx.background_spawn(async move {
            // Held across the write: writes land one at a time, newest wins.
            let mut last = written
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if seq <= *last {
                return Ok(());
            }
            *last = seq;
            write_draft(&file, contents.as_deref())
        });
        cx.spawn(async move |view, cx| {
            if let Err(err) = task.await {
                let _ = view.update(cx, |this, cx| {
                    this.push_toast(
                        components::ToastKind::Error,
                        format!("Couldn't save the pending review: {err}"),
                        cx,
                    );
                });
            }
        })
        .detach();
    }

    /// The shown file's pending comment lines, for the diff's gutter marks.
    fn sync_review_marks(&mut self, cx: &mut gpui::Context<Self>) {
        let marks: rustc_hash::FxHashSet<(ReviewSide, u32)> = self
            .review
            .as_ref()
            .and_then(|review| {
                let path = review.current_path()?;
                Some(
                    review
                        .draft
                        .comments
                        .iter()
                        .filter(|comment| comment.anchor.path == path)
                        .map(|comment| (comment.anchor.side, comment.anchor.line))
                        .collect(),
                )
            })
            .unwrap_or_default();
        self.main_pane.update(cx, |pane, cx| {
            pane.review_marks = marks;
            cx.notify();
        });
    }

    /// `c` in the diff: a comment on the line or lines under the cursor.
    fn review_comment_at_cursor(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let Some(review) = self.active_review() else {
            return;
        };
        let Some(path) = review.current_path().map(str::to_string) else {
            return;
        };
        let (repo_id, number) = (review.repo_id, review.number);
        if !self.review_diff_shown() {
            self.push_toast(
                components::ToastKind::Warning,
                format!("{path} is still opening."),
                cx,
            );
            return;
        }
        match self.main_pane.read(cx).review_selection_anchor(&path) {
            Ok(anchor) => self.open_review_composer(repo_id, number, anchor, None, window, cx),
            Err(message) => {
                self.push_toast(components::ToastKind::Warning, message.to_string(), cx)
            }
        }
    }

    pub(super) fn open_review_composer(
        &mut self,
        repo_id: RepoId,
        number: u64,
        anchor: ReviewAnchor,
        edit: Option<usize>,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let prefill = edit.and_then(|ix| self.review_comment_body(repo_id, number, ix));
        self.open_popover_from_key(
            PopoverKind::ReviewComment {
                repo_id,
                number,
                anchor,
                edit,
            },
            window,
            cx,
        );
        if let Some(text) = prefill {
            self.popover_host
                .update(cx, |host, cx| host.prefill_review_comment(text, cx));
        }
    }

    /// The composer's ctrl+enter: adds the comment, or replaces the one being
    /// edited. Returns whether it landed in a review in progress.
    pub(super) fn add_review_comment(
        &mut self,
        repo_id: RepoId,
        number: u64,
        anchor: ReviewAnchor,
        body: String,
        edit: Option<usize>,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let Some(review) = self
            .review
            .as_mut()
            .filter(|review| review.repo_id == repo_id && review.number == number)
        else {
            return false;
        };
        let comment = ReviewComment {
            anchor,
            body: body.trim_end().to_string(),
        };
        let ix = match edit.filter(|ix| *ix < review.draft.comments.len()) {
            Some(ix) => {
                review.draft.comments[ix] = comment;
                ix
            }
            None => {
                review.draft.comments.push(comment);
                review.draft.comments.len() - 1
            }
        };
        review.selected_comment = Some(ix);
        self.save_review(cx);
        self.sync_review_marks(cx);
        self.notify_pull_request_panes(cx);
        true
    }

    /// The pending comment being edited, for the composer to open with.
    pub(super) fn review_comment_body(
        &self,
        repo_id: RepoId,
        number: u64,
        ix: usize,
    ) -> Option<String> {
        self.review_of(repo_id, number)?
            .draft
            .comments
            .get(ix)
            .map(|comment| comment.body.clone())
    }

    fn review_select_comment(&mut self, direction: i8, cx: &mut gpui::Context<Self>) {
        let Some(review) = self.review.as_mut() else {
            return;
        };
        let len = review.draft.comments.len();
        let next = match (review.selected_comment, direction < 0) {
            (Some(ix), false) => Some(ix + 1).filter(|ix| *ix < len),
            (Some(ix), true) => ix.checked_sub(1),
            (None, false) => (len > 0).then_some(0),
            (None, true) => len.checked_sub(1),
        };
        if next.is_some() {
            review.selected_comment = next;
            self.notify_pull_request_panes(cx);
        }
    }

    /// Shows the selected pending comment's line in the diff.
    pub(super) fn review_jump_to_comment(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(review) = self.review.as_mut() else {
            return;
        };
        let Some(anchor) = review
            .draft
            .comments
            .get(ix)
            .map(|comment| comment.anchor.clone())
        else {
            return;
        };
        review.selected_comment = Some(ix);
        let Some(file_ix) = review.files.iter().position(|path| *path == anchor.path) else {
            self.push_toast(
                components::ToastKind::Warning,
                format!("{} isn't part of this pull request any more.", anchor.path),
                cx,
            );
            return;
        };
        if file_ix != review.file_ix {
            self.review_open_file(file_ix, cx);
        }
        // Lands once the file's diff is on screen (right away if it is).
        if let Some(review) = self.review.as_mut() {
            review.pending_jump = Some((anchor.side, anchor.line));
        }
        if self.diff_is_open() {
            self.focus_panel(FocusPanel::Diff, window, cx);
        } else {
            self.focus_diff_when_open = true;
        }
        self.notify_pull_request_panes(cx);
    }

    /// `d d` on a pending comment.
    fn review_delete_comment(&mut self, ix: usize, cx: &mut gpui::Context<Self>) {
        let Some(review) = self.review.as_mut() else {
            return;
        };
        if ix >= review.draft.comments.len() {
            return;
        }
        review.draft.comments.remove(ix);
        let len = review.draft.comments.len();
        review.selected_comment = (len > 0).then(|| ix.min(len - 1));
        self.save_review(cx);
        self.sync_review_marks(cx);
        self.notify_pull_request_panes(cx);
    }

    /// Runs each render: keeps the diff's review keys to the reviewed tab, and
    /// once the review's file is on screen, places its first cursor or shows
    /// a comment's line. Each happens once per file or jump.
    pub(super) fn review_after_render(
        &mut self,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let active = self.active_review().is_some();
        if self.main_pane.read(cx).review_active != active {
            let main = self.main_pane.clone();
            window.defer(cx, move |_window, cx| {
                main.update(cx, |pane, cx| {
                    pane.review_active = active;
                    cx.notify();
                });
            });
        }
        let Some(review) = self.active_review() else {
            return;
        };
        let jump = review.pending_jump;
        if (jump.is_none() && !review.needs_cursor)
            || !self.review_diff_shown()
            || !self.main_pane.read(cx).review_has_rows()
        {
            return;
        }
        if let Some(review) = self.review.as_mut() {
            review.pending_jump = None;
            review.needs_cursor = false;
        }
        let main = self.main_pane.clone();
        let view = cx.entity();
        window.defer(cx, move |_window, cx| {
            let landed = main.update(cx, |pane, cx| match jump {
                Some((side, line)) => pane.review_jump_to(side, line, cx),
                None => pane.review_cursor_to_start(cx),
            });
            if !landed && jump.is_some() {
                view.update(cx, |this, cx| {
                    this.push_toast(
                        components::ToastKind::Warning,
                        "That comment's line isn't in this version of the file.".to_string(),
                        cx,
                    );
                });
            }
        });
    }

    /// Review mode's keys. `None` leaves the key to the general handling.
    pub(super) fn handle_review_key(
        &mut self,
        current: Option<FocusPanel>,
        key: &str,
        shift: bool,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Option<bool> {
        self.active_review()?;
        // Any other key stands a pending `d` down.
        let armed = self
            .review
            .as_mut()
            .and_then(|review| review.armed_delete.take());
        let lower = key.to_ascii_lowercase();
        let shift = shift || lower != key;
        let direction: i8 = match lower.as_str() {
            "j" | "down" => 1,
            "k" | "up" => -1,
            _ => 0,
        };
        let shown = self.review_diff_shown();
        match (current, lower.as_str(), shift) {
            (_, "q", false) => self.leave_review(window, cx),
            (_, "s", true) => self.open_review_submit(window, cx),
            // History is hidden while reviewing; the diff stays.
            (_, "2", false) => {}
            // Replies arrive with threads; until then `r` does nothing here
            // rather than start another review.
            (_, "r", false) => {}
            (_, "]", false) => self.review_step_file(1, cx),
            (_, "[", false) => self.review_step_file(-1, cx),
            // After the jump the range is gone; collapsing it back onto the
            // landed row keeps it the cursor.
            (Some(FocusPanel::Diff), "}", _) | (Some(FocusPanel::Diff), "]", true) => {
                self.defer_pane_action(self.main_pane.clone(), cx, |pane, _, cx| {
                    let moved = pane.navigate_next_diff_change(cx);
                    pane.review_collapse_selection(cx);
                    moved
                });
            }
            (Some(FocusPanel::Diff), "{", _) | (Some(FocusPanel::Diff), "[", true) => {
                self.defer_pane_action(self.main_pane.clone(), cx, |pane, _, cx| {
                    let moved = pane.navigate_prev_diff_change(cx);
                    pane.review_collapse_selection(cx);
                    moved
                });
            }
            (Some(FocusPanel::Diff), _, _) if direction != 0 => {
                if shown {
                    self.defer_pane_action(self.main_pane.clone(), cx, move |pane, _, cx| {
                        pane.review_move_cursor(i32::from(direction), shift, cx)
                    });
                }
            }
            (Some(FocusPanel::Diff), "c", false) => self.review_comment_at_cursor(window, cx),
            (Some(FocusPanel::Diff | FocusPanel::Sidebar), "space", false) => {
                self.review_toggle_viewed(cx)
            }
            (_, "space", false) => {}
            (Some(FocusPanel::Sidebar), _, false) if direction != 0 => {
                self.review_step_file(direction, cx)
            }
            (Some(FocusPanel::Sidebar), "enter", false) => {
                if self.diff_is_open() {
                    self.focus_panel(FocusPanel::Diff, window, cx);
                }
            }
            (Some(FocusPanel::Details), _, false) if direction != 0 => {
                self.review_select_comment(direction, cx)
            }
            (Some(FocusPanel::Details), "enter", false) => {
                if let Some(ix) = self
                    .active_review()
                    .and_then(|review| review.selected_comment)
                {
                    self.review_jump_to_comment(ix, window, cx);
                }
            }
            (Some(FocusPanel::Details), "e", false) => {
                let picked = self.active_review().and_then(|review| {
                    let ix = review.selected_comment?;
                    let anchor = review.draft.comments.get(ix)?.anchor.clone();
                    Some((review.repo_id, review.number, ix, anchor))
                });
                if let Some((repo_id, number, ix, anchor)) = picked {
                    self.open_review_composer(repo_id, number, anchor, Some(ix), window, cx);
                }
            }
            (Some(FocusPanel::Details), "d", false) => {
                let Some(ix) = self
                    .active_review()
                    .and_then(|review| review.selected_comment)
                else {
                    return Some(true);
                };
                if armed == Some(ix) {
                    self.review_delete_comment(ix, cx);
                } else {
                    if let Some(review) = self.review.as_mut() {
                        review.armed_delete = Some(ix);
                    }
                    self.push_toast(
                        components::ToastKind::Warning,
                        "Press d again to delete this pending comment.".to_string(),
                        cx,
                    );
                }
            }
            _ => return None,
        }
        Some(true)
    }

    /// `S`: the submit dialog for the review in progress.
    fn open_review_submit(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let Some(review) = self.active_review() else {
            return;
        };
        let (repo_id, number) = (review.repo_id, review.number);
        self.open_pull_request_prompt(
            PopoverKind::PullRequestReview {
                repo_id,
                number,
                kind: crate::github::ReviewKind::Comment,
            },
            window,
            cx,
        );
    }
}
