//! Review mode: a pull request read file by file, with line comments that
//! wait as a pending review until one submit posts them all.
//!
//! The pending review lives on this computer, one JSON file per repository
//! and pull request, pinned to the head commit the diff shows. Nothing reaches
//! GitHub before the submit dialog's explicit confirm.

use super::panel_focus::FocusPanel;
use super::*;
use crate::github::{ReplyTarget, ReviewAnchor, ReviewComment, ReviewSide, ReviewThread};
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
        Some(dir.join(format!("{}{number}.json", draft_file_prefix(repo))))
    }

    /// Reads a draft file without touching it: for counting, where an
    /// unreadable file is simply not counted (opening it reports it).
    fn peek(file: &std::path::Path, repo: &str, number: u64) -> Option<Self> {
        let text = std::fs::read_to_string(file).ok()?;
        serde_json::from_str::<Self>(&text)
            .ok()
            .filter(|draft| draft.repo == repo && draft.number == number)
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
    /// Review threads already on GitHub, loaded when review mode opens.
    pub(super) threads: Vec<ReviewThread>,
    threads_loading: bool,
    /// Codex's suggested comments, waiting to be adopted (`a`) or dropped
    /// (`x`). Never posted as they are.
    pub(super) suggestions: Vec<ReviewComment>,
    /// Bumped whenever the reviewed head changes: a Codex run started before
    /// then answers about other lines, and its answer is dropped.
    pub(super) suggestion_generation: u64,
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

    /// Threads on lines of `path`: the ones `t` reaches and Details shows.
    pub(super) fn threads_on(&self, path: &str) -> usize {
        self.threads
            .iter()
            .filter(|thread| thread.path == path && thread.line.is_some())
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
            threads: Vec::new(),
            threads_loading: !cfg!(test),
            suggestions: Vec::new(),
            suggestion_generation: 0,
        });
        self.main_pane.update(cx, |pane, cx| {
            pane.review_active = true;
            cx.notify();
        });
        self.review_open_file(file_ix, cx);
        self.load_review_threads(cx);
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
            review.suggestions.clear();
            review.suggestion_generation += 1;
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

    /// After a submit: exactly the comments and replies that reached GitHub
    /// leave the draft. When the review itself went up and nothing is left,
    /// the draft goes and review mode closes; replies posted on their own
    /// leave the review, and its viewed files, as they were. Returns how many
    /// pending comments remain.
    pub(super) fn finish_review(
        &mut self,
        repo_id: RepoId,
        number: u64,
        submitted: &[ReviewComment],
        review_posted: bool,
        cx: &mut gpui::Context<Self>,
    ) -> usize {
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
                if review_posted && draft.comments.is_empty() {
                    draft.viewed.clear();
                }
                if let (Some(file), Ok(contents)) =
                    (ReviewDraft::file(&repo, number), draft.contents())
                {
                    let _ = write_draft(&file, contents.as_deref());
                }
                return draft.comments.len();
            }
            return 0;
        };
        review
            .draft
            .comments
            .retain(|comment| !is_submitted(comment));
        review.selected_comment = None;
        let remaining = review.draft.comments.len();
        if remaining > 0 || !review_posted {
            self.save_review(cx);
            self.sync_review_marks(cx);
            self.notify_pull_request_panes(cx);
            return remaining;
        }
        review.draft.viewed.clear();
        self.save_review(cx);
        self.review = None;
        self.main_pane.update(cx, |pane, cx| {
            pane.review_active = false;
            pane.review_marks.clear();
            pane.review_thread_marks.clear();
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
        0
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
        let (repo_id, number, pending) =
            (review.repo_id, review.number, review.draft.comments.len());
        self.set_pending_count(repo_id, number, pending);
        if contents.is_none() {
            // Removing is quick, and done before anything can rescan the
            // folder; the lock keeps an older write from recreating it.
            let mut last = written
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *last = seq;
            if let Err(err) = write_draft(&file, None) {
                drop(last);
                self.push_toast(
                    components::ToastKind::Error,
                    format!("Couldn't save the pending review: {err}"),
                    cx,
                );
            }
            return;
        }
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

    /// Fetches the review threads already on GitHub, for their marks, `t`
    /// and replies. Tests never run gh.
    fn load_review_threads(&mut self, cx: &mut gpui::Context<Self>) {
        if cfg!(test) {
            return;
        }
        let Some(review) = self.review.as_ref() else {
            return;
        };
        let (repo_id, number) = (review.repo_id, review.number);
        let Some(target) = self.github_target_for(repo_id) else {
            return;
        };
        let task = cx.background_spawn(async move {
            crate::github::list_review_threads(&target.workdir, &target.slug, number)
        });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                if let Some(review) = this
                    .review
                    .as_mut()
                    .filter(|review| review.repo_id == repo_id && review.number == number)
                {
                    review.threads_loading = false;
                    if let Ok(threads) = &result {
                        review.threads = threads.clone();
                    }
                }
                if let Err(err) = result {
                    this.push_toast(
                        components::ToastKind::Warning,
                        format!("Couldn't load the review threads of #{number}: {err}"),
                        cx,
                    );
                }
                this.sync_review_marks(cx);
                this.notify_pull_request_panes(cx);
            });
        })
        .detach();
    }

    /// The threads already on GitHub on the line under the cursor, oldest
    /// first. Two reviewers often start one each on the same line.
    pub(super) fn review_threads_at_cursor(&self, cx: &App) -> Vec<&ReviewThread> {
        let Some(review) = self.active_review() else {
            return Vec::new();
        };
        let (Some(path), true) = (review.current_path(), self.review_diff_shown()) else {
            return Vec::new();
        };
        let Some(row) = self.main_pane.read(cx).review_cursor_row() else {
            return Vec::new();
        };
        threads_on_row(&review.threads, path, row.old_line, row.new_line)
    }

    /// Codex's suggestions on the line under the cursor, with their indices.
    pub(super) fn review_suggestions_at_cursor(&self, cx: &App) -> Vec<(usize, &ReviewComment)> {
        let Some(review) = self.active_review() else {
            return Vec::new();
        };
        let (Some(path), true) = (review.current_path(), self.review_diff_shown()) else {
            return Vec::new();
        };
        let Some(row) = self.main_pane.read(cx).review_cursor_row() else {
            return Vec::new();
        };
        review
            .suggestions
            .iter()
            .enumerate()
            .filter(|(_, suggestion)| {
                suggestion.anchor.path == path
                    && match suggestion.anchor.side {
                        ReviewSide::Left => row.old_line == Some(suggestion.anchor.line),
                        ReviewSide::Right => row.new_line == Some(suggestion.anchor.line),
                    }
            })
            .collect()
    }

    /// `a`/`x` on a suggestion's line: adopt it as a pending comment of yours
    /// (to edit or delete like any other), or drop it.
    fn review_take_suggestion(&mut self, adopt: bool, cx: &mut gpui::Context<Self>) {
        let Some(ix) = self
            .review_suggestions_at_cursor(cx)
            .first()
            .map(|(ix, _)| *ix)
        else {
            self.push_toast(
                components::ToastKind::Warning,
                "No Codex suggestion on this line; t goes to the next one.".to_string(),
                cx,
            );
            return;
        };
        if adopt && !self.main_pane.read(cx).review_cursor_commentable() {
            self.push_toast(
                components::ToastKind::Warning,
                "GitHub only takes comments on changed lines and the 3 lines around them; x drops this suggestion.".to_string(),
                cx,
            );
            return;
        }
        let Some(review) = self.review.as_mut() else {
            return;
        };
        let suggestion = review.suggestions.remove(ix);
        if adopt {
            review.draft.comments.push(suggestion);
            review.selected_comment = Some(review.draft.comments.len() - 1);
            self.save_review(cx);
        }
        self.sync_review_marks(cx);
        self.notify_pull_request_panes(cx);
    }

    /// Codex's answer to "suggest line comments": the ones on this pull
    /// request's files join the review as suggestions, unless the review
    /// moved to another head since the run started.
    pub(super) fn add_review_suggestions(
        &mut self,
        repo_id: RepoId,
        number: u64,
        generation: u64,
        answer: &str,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(review) = self
            .review
            .as_mut()
            .filter(|review| review.repo_id == repo_id && review.number == number)
        else {
            return;
        };
        if review.suggestion_generation != generation {
            self.push_toast(
                components::ToastKind::Warning,
                format!("#{number} changed while Codex was reading it; i p asks again."),
                cx,
            );
            return;
        }
        let Some(suggestions) = parse_review_suggestions(answer, &review.files) else {
            self.push_toast(
                components::ToastKind::Warning,
                "Couldn't read line comments in Codex's answer; it's in the Codex panel."
                    .to_string(),
                cx,
            );
            return;
        };
        let count = suggestions.len();
        review.suggestions = suggestions;
        self.sync_review_marks(cx);
        self.notify_pull_request_panes(cx);
        let message = match count {
            0 => "Codex had no line comments to suggest.".to_string(),
            n => format!(
                "Codex suggested {n} line comment{}. t steps to them; a adds one to your review, x drops it.",
                if n == 1 { "" } else { "s" }
            ),
        };
        self.push_toast(components::ToastKind::Success, message, cx);
    }

    /// Details shows the thread under the cursor; the cursor lives in the
    /// diff, so a move repaints Details once the diff has moved it.
    fn notify_review_details_after_move(&self, cx: &mut gpui::Context<Self>) {
        let details = self.details_pane.clone();
        cx.defer(move |cx| details.update(cx, |_, cx| cx.notify()));
    }

    /// `t`/`T`: the next or previous thread of the shown file.
    fn review_step_thread(&mut self, direction: i8, cx: &mut gpui::Context<Self>) {
        let Some(review) = self.active_review() else {
            return;
        };
        let Some(path) = review.current_path() else {
            return;
        };
        let targets: Vec<(ReviewSide, u32)> = review
            .threads
            .iter()
            .filter(|thread| thread.path == path)
            .filter_map(|thread| Some((thread.side, thread.line?)))
            .chain(
                review
                    .suggestions
                    .iter()
                    .filter(|suggestion| suggestion.anchor.path == path)
                    .map(|suggestion| (suggestion.anchor.side, suggestion.anchor.line)),
            )
            .collect();
        if targets.is_empty() {
            let message = if review.threads_loading {
                "Still loading the threads."
            } else {
                "No threads or suggestions on this file's lines."
            };
            self.push_toast(components::ToastKind::Warning, message.to_string(), cx);
            return;
        }
        if !self.review_diff_shown() {
            return;
        }
        self.defer_pane_action(self.main_pane.clone(), cx, move |pane, _, cx| {
            pane.review_step_to(&targets, direction, cx)
        });
        self.notify_review_details_after_move(cx);
    }

    /// `r` in the diff: a reply to the thread on the line under the cursor.
    fn review_reply_at_cursor(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let Some((repo_id, number)) = self
            .active_review()
            .map(|review| (review.repo_id, review.number))
        else {
            return;
        };
        // Several threads on the line: the one talked in most recently.
        let thread = self
            .review_threads_at_cursor(cx)
            .into_iter()
            .max_by_key(|thread| thread.comments.last().map(|comment| comment.at.clone()));
        let Some(thread) = thread else {
            self.push_toast(
                components::ToastKind::Warning,
                "No thread on this line. c comments on it; t goes to the next thread.".to_string(),
                cx,
            );
            return;
        };
        let Some(line) = thread.line else {
            return;
        };
        let anchor = ReviewAnchor {
            path: thread.path.clone(),
            side: thread.side,
            line,
            start: None,
        };
        let reply_to = ReplyTarget {
            id: thread.root_id,
            author: thread
                .comments
                .first()
                .map(|comment| comment.author.clone())
                .unwrap_or_default(),
        };
        self.open_review_composer(
            repo_id,
            number,
            anchor,
            None,
            Some(reply_to),
            None,
            window,
            cx,
        );
    }

    /// The shown file's pending comment lines and thread lines, for the
    /// diff's gutter marks.
    fn sync_review_marks(&mut self, cx: &mut gpui::Context<Self>) {
        let threads: rustc_hash::FxHashSet<(ReviewSide, u32)> = self
            .review
            .as_ref()
            .and_then(|review| {
                let path = review.current_path()?;
                Some(
                    review
                        .threads
                        .iter()
                        .filter(|thread| thread.path == path)
                        .filter_map(|thread| Some((thread.side, thread.line?)))
                        .collect(),
                )
            })
            .unwrap_or_default();
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
        let suggestions: rustc_hash::FxHashSet<(ReviewSide, u32)> = self
            .review
            .as_ref()
            .and_then(|review| {
                let path = review.current_path()?;
                Some(
                    review
                        .suggestions
                        .iter()
                        .filter(|suggestion| suggestion.anchor.path == path)
                        .map(|suggestion| (suggestion.anchor.side, suggestion.anchor.line))
                        .collect(),
                )
            })
            .unwrap_or_default();
        self.main_pane.update(cx, |pane, cx| {
            pane.review_marks = marks;
            pane.review_thread_marks = threads;
            pane.review_suggestion_marks = suggestions;
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
        let main = self.main_pane.read(cx);
        let anchor = main.review_selection_anchor(&path);
        // A suggestion replaces lines of the new version only.
        let head_side_only = anchor.as_ref().is_ok_and(|anchor| {
            anchor.side == ReviewSide::Right
                && anchor
                    .start
                    .is_none_or(|(side, _)| side == ReviewSide::Right)
        });
        let suggestion = head_side_only
            .then(|| main.review_selection_new_text())
            .flatten();
        match anchor {
            Ok(anchor) => self
                .open_review_composer(repo_id, number, anchor, None, None, suggestion, window, cx),
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
        reply_to: Option<ReplyTarget>,
        suggestion: Option<String>,
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
                reply_to,
            },
            window,
            cx,
        );
        // Set after the dialog opens: it can't read the root while opening.
        self.popover_host.update(cx, |host, cx| {
            host.set_review_suggestion(suggestion, cx);
            if let Some(text) = prefill {
                host.prefill_review_comment(text, cx);
            }
        });
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
        reply_to: Option<ReplyTarget>,
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
            reply_to,
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
        let details = self.details_pane.clone();
        let view = cx.entity();
        window.defer(cx, move |_window, cx| {
            let landed = main.update(cx, |pane, cx| match jump {
                Some((side, line)) => pane.review_jump_to(side, line, cx),
                None => pane.review_cursor_to_start(cx),
            });
            details.update(cx, |_, cx| cx.notify());
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
            (Some(FocusPanel::Diff), "r", false) => self.review_reply_at_cursor(window, cx),
            // Elsewhere `r` does nothing rather than start another review.
            (_, "r", false) => {}
            (Some(FocusPanel::Diff), "t", _) => {
                self.review_step_thread(if shift { -1 } else { 1 }, cx)
            }
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
                self.notify_review_details_after_move(cx);
            }
            (Some(FocusPanel::Diff), "{", _) | (Some(FocusPanel::Diff), "[", true) => {
                self.defer_pane_action(self.main_pane.clone(), cx, |pane, _, cx| {
                    let moved = pane.navigate_prev_diff_change(cx);
                    pane.review_collapse_selection(cx);
                    moved
                });
                self.notify_review_details_after_move(cx);
            }
            (Some(FocusPanel::Diff), _, _) if direction != 0 => {
                if shown {
                    self.defer_pane_action(self.main_pane.clone(), cx, move |pane, _, cx| {
                        pane.review_move_cursor(i32::from(direction), shift, cx)
                    });
                    self.notify_review_details_after_move(cx);
                }
            }
            (Some(FocusPanel::Diff), "c", false) => self.review_comment_at_cursor(window, cx),
            (Some(FocusPanel::Diff), "a", false) => self.review_take_suggestion(true, cx),
            (Some(FocusPanel::Diff), "x", false) => self.review_take_suggestion(false, cx),
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
                    let comment = review.draft.comments.get(ix)?;
                    Some((
                        review.repo_id,
                        review.number,
                        ix,
                        comment.anchor.clone(),
                        comment.reply_to.clone(),
                    ))
                });
                if let Some((repo_id, number, ix, anchor, reply_to)) = picked {
                    self.open_review_composer(
                        repo_id,
                        number,
                        anchor,
                        Some(ix),
                        reply_to,
                        None,
                        window,
                        cx,
                    );
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

/// The threads on a diff row of `path`: a base-side thread by the row's old
/// line, a head-side one by its new line. Outdated threads have no line.
fn threads_on_row<'a>(
    threads: &'a [ReviewThread],
    path: &str,
    old_line: Option<u32>,
    new_line: Option<u32>,
) -> Vec<&'a ReviewThread> {
    threads
        .iter()
        .filter(|thread| {
            thread.path == path
                && thread.line.is_some_and(|line| match thread.side {
                    ReviewSide::Left => old_line == Some(line),
                    ReviewSide::Right => new_line == Some(line),
                })
        })
        .collect()
}

/// Whether a submit posts a review at all. Pending replies post on their own
/// threads; with nothing else to say (a Comment with no summary and no line
/// comments), there's no review to post around them.
pub(super) fn review_needed(
    kind: crate::github::ReviewKind,
    body: &str,
    line_comments: usize,
    replies: usize,
) -> bool {
    replies == 0
        || line_comments > 0
        || kind != crate::github::ReviewKind::Comment
        || !body.trim().is_empty()
}

/// `owner/name` as the start of its draft files' names, `owner~name~`: one
/// flat file per pull request.
fn draft_file_prefix(repo: &str) -> String {
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
    format!("{name}~")
}

/// Pending comment counts of the reviews saved on this computer for `repo`,
/// by pull request number. Only files that read back with comments count.
pub(super) fn pending_review_counts(repo: &str) -> rustc_hash::FxHashMap<u64, usize> {
    gitcomet_state::session::review_drafts_dir()
        .map(|dir| pending_review_counts_in(&dir, repo))
        .unwrap_or_default()
}

fn pending_review_counts_in(
    dir: &std::path::Path,
    repo: &str,
) -> rustc_hash::FxHashMap<u64, usize> {
    let mut counts = rustc_hash::FxHashMap::default();
    let prefix = draft_file_prefix(repo);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return counts;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(number) = name
            .to_str()
            .and_then(|name| name.strip_prefix(&prefix))
            .and_then(|rest| rest.strip_suffix(".json"))
            .and_then(|number| number.parse::<u64>().ok())
        else {
            continue;
        };
        if let Some(draft) = ReviewDraft::peek(&entry.path(), repo, number)
            && !draft.comments.is_empty()
        {
            counts.insert(number, draft.comments.len());
        }
    }
    counts
}

/// What Codex is asked for when it suggests line comments in review mode.
pub(super) const SUGGESTION_INSTRUCTIONS: &str = "Review this pull request's patch and suggest line comments. Reply with only a JSON array, no prose and no code fences. Each item is one comment: {\"path\": the file path as in the patch, without its a/ or b/ prefix, \"line\": a line number, \"side\": \"RIGHT\" for an added or unchanged line (its number in the new file) or \"LEFT\" for a removed line (its number in the old file), \"body\": the comment, specific and constructive}. Only comment on lines inside the patch's hunks. At most 15 comments, the important issues first; an empty array if nothing is worth saying.";

/// Codex's suggested comments, read from its answer: the first JSON array in
/// it that parses, item by item (a malformed item is skipped, not the lot),
/// kept to this pull request's files and to at most 30. `None` when there's no
/// array to read at all. The text stays plain and is never posted as is.
pub(super) fn parse_review_suggestions(
    answer: &str,
    files: &[String],
) -> Option<Vec<ReviewComment>> {
    #[derive(serde::Deserialize)]
    struct Suggested {
        path: String,
        line: u32,
        #[serde(default)]
        side: Option<String>,
        body: String,
    }
    let items = answer.match_indices('[').find_map(|(start, _)| {
        serde_json::Deserializer::from_str(&answer[start..])
            .into_iter::<Vec<serde_json::Value>>()
            .next()?
            .ok()
    })?;
    // A path as the patch spells it (`b/src/x.rs`) is the PR's `src/x.rs`.
    let known = |path: &str| -> Option<String> {
        if files.iter().any(|file| file == path) {
            return Some(path.to_string());
        }
        path.strip_prefix("a/")
            .or_else(|| path.strip_prefix("b/"))
            .filter(|rest| files.iter().any(|file| file == rest))
            .map(str::to_string)
    };
    Some(
        items
            .into_iter()
            .filter_map(|item| serde_json::from_value::<Suggested>(item).ok())
            .filter(|item| item.line > 0 && !item.body.trim().is_empty())
            .filter_map(|item| {
                let path = known(&item.path)?;
                let side = if item
                    .side
                    .as_deref()
                    .is_some_and(|side| side.eq_ignore_ascii_case("LEFT"))
                {
                    ReviewSide::Left
                } else {
                    ReviewSide::Right
                };
                Some(ReviewComment {
                    anchor: ReviewAnchor {
                        path,
                        side,
                        line: item.line,
                        start: None,
                    },
                    body: item.body.trim().chars().take(2_000).collect(),
                    reply_to: None,
                })
            })
            .take(30)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::{ReviewKind, ThreadComment};

    #[test]
    fn a_review_posts_unless_only_replies_remain_with_nothing_to_say() {
        use ReviewKind::*;
        let cases = [
            (Comment, "", 0, 0, true),
            (Comment, "", 2, 0, true),
            (Comment, "", 0, 1, false),
            (Comment, "  ", 0, 1, false),
            (Comment, "summary", 0, 1, true),
            (Comment, "", 1, 1, true),
            (Approve, "", 0, 1, true),
            (RequestChanges, "why", 0, 1, true),
        ];
        for (kind, body, lines, replies, posts) in cases {
            assert_eq!(
                review_needed(kind, body, lines, replies),
                posts,
                "{kind:?} {body:?} {lines} {replies}"
            );
        }
    }

    #[test]
    fn codex_suggestions_keep_to_the_pull_requests_files() {
        let files = ["src/a.rs".to_string()];
        let answer = r#"Here you go:
[{"path": "src/a.rs", "line": 4, "side": "RIGHT", "body": "Check the edge."},
 {"path": "src/a.rs", "line": 2, "side": "LEFT", "body": "Why remove this?"},
 {"path": "elsewhere.rs", "line": 1, "body": "not in the PR"},
 {"path": "src/a.rs", "line": 0, "body": "no line"},
 {"path": "src/a.rs", "line": 5, "body": "   "}]"#;
        let suggestions = parse_review_suggestions(answer, &files).expect("an array");
        let got: Vec<_> = suggestions
            .iter()
            .map(|s| (s.anchor.side, s.anchor.line, s.body.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                (ReviewSide::Right, 4, "Check the edge."),
                (ReviewSide::Left, 2, "Why remove this?"),
            ]
        );
        // A bad item is skipped, a prefixed path and a lowercase side are read,
        // and a `[` in the prose before the array doesn't hide it.
        let messy = r#"Notes [draft]: here
[{"path": "b/src/a.rs", "line": 7, "side": "left", "body": "Old line."},
 {"path": "src/a.rs", "line": "8", "body": "string line"}] done ]"#;
        let messy = parse_review_suggestions(messy, &files).expect("an array");
        assert_eq!(messy.len(), 1);
        assert_eq!(
            (
                messy[0].anchor.path.as_str(),
                messy[0].anchor.side,
                messy[0].anchor.line
            ),
            ("src/a.rs", ReviewSide::Left, 7)
        );
        assert!(parse_review_suggestions("no json here", &files).is_none());
        assert!(parse_review_suggestions("[not json]", &files).is_none());
        assert_eq!(parse_review_suggestions("[]", &files), Some(Vec::new()));
    }

    #[test]
    fn saved_reviews_are_counted_per_pull_request_of_this_repository() {
        let dir = tempfile::tempdir().expect("tempdir");
        let write = |name: &str, repo: &str, number: u64, comments: usize| {
            let draft = ReviewDraft {
                repo: repo.into(),
                number,
                head_oid: "a".repeat(40),
                comments: (0..comments)
                    .map(|n| ReviewComment {
                        anchor: ReviewAnchor {
                            path: "a.rs".into(),
                            side: ReviewSide::Right,
                            line: n as u32 + 1,
                            start: None,
                        },
                        body: "?".into(),
                        reply_to: None,
                    })
                    .collect(),
                viewed: Default::default(),
            };
            std::fs::write(dir.path().join(name), serde_json::to_vec(&draft).unwrap()).unwrap();
        };
        write("o~r~7.json", "o/r", 7, 2);
        write("o~r~8.json", "o/r", 8, 0);
        write("o~r~c~9.json", "o/r~c", 9, 1);
        write("o~r~10.json", "someone/else", 10, 1);
        std::fs::write(dir.path().join("o~r~11.json"), "not json").unwrap();
        std::fs::write(dir.path().join("o~r~12.json.unreadable"), "{}").unwrap();
        let counts = pending_review_counts_in(dir.path(), "o/r");
        assert_eq!(counts.len(), 1, "{counts:?}");
        assert_eq!(counts.get(&7), Some(&2));
        // Counting never moves an unreadable file aside.
        assert!(dir.path().join("o~r~11.json").exists());
    }

    #[test]
    fn threads_match_a_row_by_their_own_side() {
        let thread = |root_id, side, line| ReviewThread {
            root_id,
            path: "a.rs".into(),
            side,
            line,
            comments: vec![ThreadComment {
                author: "octo".into(),
                body: "?".into(),
                at: String::new(),
            }],
        };
        let threads = [
            thread(1, ReviewSide::Right, Some(7)),
            thread(2, ReviewSide::Left, Some(6)),
            thread(3, ReviewSide::Right, Some(7)),
            thread(4, ReviewSide::Right, None),
        ];
        let ids = |old_line, new_line| {
            threads_on_row(&threads, "a.rs", old_line, new_line)
                .into_iter()
                .map(|thread| thread.root_id)
                .collect::<Vec<_>>()
        };
        // A split-view pair carries both an old and a new line.
        assert_eq!(ids(Some(6), Some(7)), [1, 2, 3]);
        assert_eq!(ids(None, Some(7)), [1, 3]);
        assert_eq!(ids(Some(7), None), Vec::<u64>::new());
    }
}
