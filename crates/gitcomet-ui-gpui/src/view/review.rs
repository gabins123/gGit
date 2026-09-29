//! Review mode: a pull request read file by file, with line comments that
//! wait as a pending review until one submit posts them all.
//!
//! The pending review lives on this computer, one JSON file per repository
//! and pull request, pinned to the head commit the diff shows. Nothing of it
//! reaches GitHub before the submit dialog's explicit confirm. Viewed marks
//! are the exception: GitHub keeps them as your own private state per pull
//! request, and each one is sent as you mark it.

use super::panel_focus::FocusPanel;
use super::panes::main::SinceLines;
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
    /// The head the viewed marks were last checked against. When the pull
    /// request moves on, the files changed since lose their mark, as on GitHub.
    #[serde(default)]
    pub(super) viewed_head: Option<String>,
    /// `space` presses GitHub hasn't confirmed yet, each with the head it
    /// was made at: sent once GitHub is reachable, at that head only.
    #[serde(default)]
    pub(super) unsynced: std::collections::BTreeMap<String, UnsyncedMark>,
}

/// A viewed mark pressed but not yet on GitHub.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct UnsyncedMark {
    pub(super) viewed: bool,
    /// The head shown when it was pressed.
    pub(super) head: String,
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

    /// The head moved from `viewed_head` to `head`: files in `changed` are no
    /// longer viewed. `None` (the old head can't be had) un-views them all.
    fn viewed_moved(&mut self, head: &str, changed: Option<&std::collections::BTreeSet<String>>) {
        match changed {
            Some(changed) => self.viewed.retain(|path| !changed.contains(path)),
            None => self.viewed.clear(),
        }
        self.viewed_head = Some(head.to_string());
    }

    /// The file's next contents: `None` when there's nothing left to keep.
    fn contents(&self) -> Result<Option<Vec<u8>>, String> {
        if self.comments.is_empty() && self.viewed.is_empty() && self.unsynced.is_empty() {
            return Ok(None);
        }
        serde_json::to_vec_pretty(self)
            .map(Some)
            .map_err(|err| err.to_string())
    }
}

/// Sequence numbers for review mode's background loads, from one counter for
/// the whole run: an answer from an earlier review session never matches.
fn next_seq() -> u64 {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}

/// A viewed mark GitHub took: laid over loads started before it did.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ConfirmedMark {
    viewed: bool,
    /// The newest viewed-marks load when GitHub took it; a load started
    /// later already has it.
    at: u64,
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

/// What changed from your last review's commit to the reviewed head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SinceReview {
    Changed(crate::github::ChangesSince),
    /// The last review's commit can't be had (a force push dropped it).
    Gone,
    /// Working it out failed for another reason; the next open retries.
    Failed(String),
}

/// Where the viewed marks come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ViewedSync {
    /// Not asked (tests).
    Idle,
    Loading,
    /// GitHub's own per-user marks; `space` changes them there too.
    GitHub {
        pr_id: String,
    },
    /// GitHub couldn't be reached: the marks are this computer's only.
    Offline,
}

/// A contiguous set of PR commits and the files changed from its oldest
/// commit's parent to its newest commit.
pub(super) struct CommitReviewRange {
    pub(super) selection: super::pull_requests::SelectedCommitRange,
    pub(super) changes: super::pull_requests::PrLoad<crate::github::CommitRangeChanges>,
    seq: u64,
}

#[derive(Clone, Copy, Default)]
pub(super) struct CommitScopePicker {
    /// All changes, Since last review, then newest-first commit rows.
    pub(super) cursor: usize,
    pub(super) selection: super::pull_requests::PrCommitSelection,
}

impl CommitScopePicker {
    fn step(&mut self, direction: i8, extend: bool, commits: usize) {
        let next = if direction < 0 {
            self.cursor.saturating_sub(1)
        } else {
            (self.cursor + 1).min(commits + 1)
        };
        if extend && self.cursor >= 2 && next < 2 {
            return;
        }
        if next >= 2 {
            if self.cursor < 2 {
                self.selection.select(Some(next - 2));
            } else {
                self.selection.step(direction, extend, commits);
            }
        } else {
            self.selection.select(None);
        }
        self.cursor = next;
    }
}

#[cfg(test)]
mod commit_scope_picker_tests {
    use super::*;

    #[test]
    fn extending_at_newest_commit_stays_on_the_commit() {
        let mut picker = CommitScopePicker {
            cursor: 2,
            ..Default::default()
        };
        picker.selection.select(Some(0));
        picker.step(-1, true, 3);
        assert_eq!(picker.cursor, 2);
        assert_eq!(picker.selection.range(3), Some((0, 0)));
    }
}

/// A viewed press as tests read it: path, viewed, confirmed by GitHub.
#[cfg(test)]
pub(super) type ViewedPressForTest = (String, bool, bool);

/// The pull request being reviewed and where the review is.
pub(super) struct ReviewMode {
    pub(super) repo_id: RepoId,
    pub(super) number: u64,
    pub(super) title: String,
    /// The pull request's changed files, in GitHub's order.
    pub(super) files: Vec<String>,
    /// The current PR's net-changed files, restored for All and Since.
    pub(super) all_files: Vec<String>,
    pub(super) file_ix: usize,
    pub(super) draft: ReviewDraft,
    /// The row picked in the Your review panel.
    pub(super) selected_comment: Option<usize>,
    /// Pending comments were written on an older head than the one shown.
    pub(super) head_moved: bool,
    /// A line to put the cursor on once its file's diff is on screen.
    pub(super) pending_jump: Option<(ReviewSide, u32)>,
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
    /// Commit used for the currently computed changes-since result.
    since_base_oid: Option<String>,
    /// What changed since that review; `None` until known.
    pub(super) since_review: Option<SinceReview>,
    /// `L`: the file list, and the keys walking it, keep to the files changed
    /// since your last review.
    pub(super) only_changed: bool,
    pub(super) commit_range: Option<CommitReviewRange>,
    /// The head a viewed-marks check is running for.
    viewed_syncing_to: Option<String>,
    /// Bumped per load of what changed since the last review.
    since_seq: u64,
    pub(super) viewed_sync: ViewedSync,
    viewed_seq: u64,
    /// Files you viewed on GitHub that changed since: not viewed any more.
    pub(super) dismissed: std::collections::BTreeSet<String>,
    /// Marks GitHub took this session, for loads still on their way.
    confirmed: std::collections::BTreeMap<String, ConfirmedMark>,
    /// Files dismissed before their unsynced press, to restore if GitHub
    /// refuses it.
    was_dismissed: std::collections::BTreeSet<String>,
    /// One viewed mutation at a time, so they land in order.
    viewed_pushing: bool,
    /// Per file, the head-side line ranges of the pull request's own diff:
    /// where a comment can go while `L` shows the changes since your review.
    /// Good for `hunk_key`'s (merge base, head) only.
    hunk_ranges: rustc_hash::FxHashMap<String, SinceLines>,
    hunk_ranges_loading: rustc_hash::FxHashSet<String>,
    hunk_key: Option<(String, String)>,
    /// The diff base a reopen was asked for, so a render asks only once.
    reopened_for: Option<String>,
    /// `/`: the file list keeps to paths matching it, as the Changes list's
    /// filter matches.
    pub(super) query: super::panes::ChangesQuery,
    /// `V`: viewed files stay in the list. Off, they're hidden, the open one
    /// included; its diff stays up and `j`/`k` go on from it.
    pub(super) show_viewed: bool,
    /// The PR's generated files (GitHub's `linguist-generated`), copied from
    /// `pull_requests` once gix has read them, so `file_listed` and friends
    /// (pure `&self` methods) don't have to reach back through it.
    pub(super) generated: Arc<std::collections::BTreeSet<String>>,
    /// `Shift+G`: generated files stay in the list, the same way `show_viewed`
    /// works for viewed ones. Off (the default), they're hidden regardless of
    /// viewed state — a generated file never counts toward viewed progress.
    pub(super) show_generated: bool,
    /// Generated files whose "Generated file" placeholder you've dismissed
    /// with `enter`, so their diff loads normally for the rest of the review
    /// session.
    pub(super) generated_placeholder_dismissed: std::collections::BTreeSet<String>,
}

impl ReviewMode {
    pub(super) fn current_path(&self) -> Option<&str> {
        self.files.get(self.file_ix).map(String::as_str)
    }

    /// Whether the open file's "Generated file" placeholder is showing right
    /// now: it's generated, and `enter` hasn't dismissed it yet this session.
    pub(super) fn generated_placeholder_active(&self) -> bool {
        self.current_path().is_some_and(|path| {
            self.is_generated(path) && !self.generated_placeholder_dismissed.contains(path)
        })
    }

    /// Lays GitHub's viewed marks (the answer to load `seq`) over this
    /// review: GitHub's viewed set replaces the one here, then the presses it
    /// doesn't have yet go on top. An unsynced press made at another head is
    /// dropped: the file may have changed since, and GitHub's own dismissal
    /// covers that. Returns whether marks here differed from GitHub's, beyond
    /// those presses.
    fn merge_viewed_states(&mut self, states: &crate::github::ViewedStates, seq: u64) -> bool {
        use crate::github::ViewedState;
        let head = self.draft.head_oid.clone();
        self.draft.unsynced.retain(|_, mark| mark.head == head);
        let unsynced = &self.draft.unsynced;
        self.was_dismissed
            .retain(|path| unsynced.contains_key(path));
        self.confirmed.retain(|_, mark| mark.at >= seq);
        let having = |wanted: ViewedState| -> std::collections::BTreeSet<String> {
            states
                .files
                .iter()
                .filter(|(_, state)| **state == wanted)
                .map(|(path, _)| path.clone())
                .collect()
        };
        let viewed = having(ViewedState::Viewed);
        let pressed =
            |path: &&String| unsynced.contains_key(*path) || self.confirmed.contains_key(*path);
        let differed = self
            .draft
            .viewed
            .iter()
            .filter(|path| !pressed(path))
            .ne(viewed.iter().filter(|path| !pressed(path)));
        self.draft.viewed = viewed;
        self.dismissed = having(ViewedState::Dismissed);
        let presses = self
            .confirmed
            .iter()
            .map(|(path, mark)| (path, mark.viewed))
            .chain(
                self.draft
                    .unsynced
                    .iter()
                    .map(|(path, mark)| (path, mark.viewed)),
            );
        for (path, viewed) in presses {
            self.dismissed.remove(path);
            if viewed {
                self.draft.viewed.insert(path.clone());
            } else {
                self.draft.viewed.remove(path);
            }
        }
        // Kept in step, so the local fallback starts from GitHub's marks.
        self.draft.viewed_head = Some(head);
        differed
    }

    /// Whether `comment` is a reply to an outdated conversation, whose line
    /// counts in the commit that conversation was written on.
    pub(super) fn replies_to_outdated(&self, comment: &ReviewComment) -> bool {
        comment.reply_to.as_ref().is_some_and(|to| {
            self.outdated_threads()
                .any(|thread| thread.root_id == to.id)
        })
    }

    /// Threads whose lines changed since they were written, in file order.
    pub(super) fn outdated_threads(&self) -> impl Iterator<Item = &ReviewThread> {
        self.threads.iter().filter(|thread| thread.outdated())
    }

    /// With `L` on and what changed known: your last review's commit, which
    /// the diff starts from instead of the merge base.
    pub(super) fn since_base(&self) -> Option<&str> {
        if !self.only_changed || !matches!(self.since_review, Some(SinceReview::Changed(_))) {
            return None;
        }
        self.since_base_oid.as_deref()
    }

    /// Whether `path` changed since your last review.
    pub(super) fn changed_since_review(&self, path: &str) -> bool {
        matches!(&self.since_review, Some(SinceReview::Changed(changes)) if changes.files.contains(path))
    }

    /// The pull request's files that changed since your last review.
    pub(super) fn files_changed_since_review(&self) -> usize {
        self.all_files
            .iter()
            .filter(|path| self.changed_since_review(path))
            .count()
    }

    /// Whether `path` is one of this PR's generated files (GitHub's
    /// `linguist-generated`), as read from `.gitattributes` at the head
    /// commit and copied here once known.
    pub(super) fn is_generated(&self, path: &str) -> bool {
        self.generated.contains(path)
    }

    /// Whether file `ix` is in the list, the one `j`/`k`, `]`/`[` and
    /// `space` walk: it matches the `/` filter, passes `L` (only files
    /// changed since your last review), and isn't viewed, unless `V` shows
    /// viewed files. The open file follows the same rule: once everything is
    /// viewed the list is empty, even though a diff is still up. A dismissed
    /// file ("changed since you viewed") isn't viewed.
    ///
    /// A generated file is hidden purely by `show_generated`, regardless of
    /// viewed state: generated files don't count toward viewed progress, so
    /// whether one happens to be marked viewed never affects its visibility.
    pub(super) fn file_listed(&self, ix: usize) -> bool {
        self.file_passes_filters(ix)
            && self.files.get(ix).is_some_and(|path| {
                if self.is_generated(path) {
                    self.show_generated
                } else {
                    self.show_viewed || !self.draft.viewed.contains(path)
                }
            })
    }

    /// The `/` filter and `L`, viewed or not.
    fn file_passes_filters(&self, ix: usize) -> bool {
        // While what changed is being worked out, `L` hides nothing.
        let known = matches!(self.since_review, Some(SinceReview::Changed(_)));
        self.files.get(ix).is_some_and(|path| {
            (!self.only_changed || !known || self.changed_since_review(path))
                && self.query.matches_path(path)
        })
    }

    pub(super) fn range_head(&self) -> &str {
        self.commit_range
            .as_ref()
            .map_or(self.draft.head_oid.as_str(), |range| {
                range.selection.newest_oid.as_str()
            })
    }

    pub(super) fn historical_range(&self) -> bool {
        self.commit_range
            .as_ref()
            .is_some_and(|range| range.selection.newest_oid != self.draft.head_oid)
    }

    /// Viewed files the list hides for now: `V` shows them. A generated file
    /// is never counted here, even when it's viewed and hidden — it's
    /// counted once, under `generated_hidden`, so the two lines never
    /// double-count the same file.
    pub(super) fn viewed_hidden(&self) -> usize {
        (0..self.files.len())
            .filter(|ix| {
                self.file_passes_filters(*ix)
                    && !self.file_listed(*ix)
                    && !self.files.get(*ix).is_some_and(|path| self.is_generated(path))
            })
            .count()
    }

    /// Generated files the list hides for now: `Shift+G` shows them.
    pub(super) fn generated_hidden(&self) -> usize {
        (0..self.files.len())
            .filter(|ix| {
                self.file_passes_filters(*ix)
                    && !self.show_generated
                    && self.files.get(*ix).is_some_and(|path| self.is_generated(path))
            })
            .count()
    }

    /// Files that count toward viewed progress ("X of Y viewed"): every file
    /// but the generated ones, whether or not `Shift+G` is currently showing
    /// them.
    pub(super) fn non_generated_file_count(&self) -> usize {
        self.files
            .iter()
            .filter(|path| !self.is_generated(path))
            .count()
    }

    /// "Your last review: Approved · 2 days ago · at abc1234 · 3 commits since
    /// · 4 files changed since", once the review is known.
    pub(super) fn last_review_line(
        &self,
        last: &crate::github::LastReview,
        now: std::time::SystemTime,
    ) -> String {
        let mut parts = vec![last.verdict().to_string()];
        if let Ok(at) = last.submitted_at.parse::<jiff::Timestamp>() {
            parts.push(super::date_time::format_relative_time(at.as_second(), now));
        }
        if !last.commit_id.is_empty() {
            let short: String = last.commit_id.chars().take(7).collect();
            parts.push(format!("at {short}"));
        }
        if let Some(SinceReview::Changed(changes)) = &self.since_review {
            let plural = |n: usize| if n == 1 { "" } else { "s" };
            let files = self.files_changed_since_review();
            parts.push(format!(
                "{} commit{} since",
                changes.commits,
                plural(changes.commits)
            ));
            parts.push(format!("{files} file{} changed since", plural(files)));
        }
        format!("Your last review: {}", parts.join(" · "))
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

    pub(super) fn handle_commit_scope_picker_key(
        &mut self,
        keystroke: &gpui::Keystroke,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let mods = keystroke.modifiers;
        if mods.control || mods.alt || mods.platform || mods.function {
            return true;
        }
        let key = keystroke.key.to_ascii_lowercase();
        let count = self
            .active_repo_id()
            .and_then(|repo_id| self.pull_requests.repo(repo_id))
            .and_then(|prs| prs.detail.ready())
            .map_or(0, |detail| detail.commits.len());
        let Some(picker) = self.commit_scope_picker.as_mut() else {
            return false;
        };
        match key.as_str() {
            "j" | "down" => picker.step(1, mods.shift, count),
            "k" | "up" => picker.step(-1, mods.shift, count),
            "escape" => self.commit_scope_picker = None,
            "enter" => {
                let choice = *picker;
                let selection = self
                    .active_repo_id()
                    .and_then(|repo_id| self.pull_requests.repo(repo_id))
                    .and_then(|prs| prs.detail.ready())
                    .and_then(|detail| choice.selection.selected_range(&detail.commits));
                self.commit_scope_picker = None;
                if choice.cursor == 0
                    && self
                        .active_review()
                        .is_some_and(|review| review.commit_range.is_none() && !review.only_changed)
                    || choice.cursor == 1
                        && self
                            .active_review()
                            .is_some_and(|review| review.only_changed)
                    || choice.cursor >= 2
                        && self
                            .active_review()
                            .and_then(|review| review.commit_range.as_ref())
                            .is_some_and(|range| Some(&range.selection) == selection.as_ref())
                {
                    cx.notify();
                    return true;
                }
                match choice.cursor {
                    0 => self.set_review_commit_range(None, cx),
                    1 => {
                        if let Some(review) = self.review.as_mut() {
                            review.only_changed = false;
                        }
                        self.review_toggle_only_changed(cx);
                    }
                    _ => self.set_review_commit_range(selection, cx),
                }
            }
            _ => {}
        }
        cx.notify();
        true
    }

    fn open_commit_scope_picker(&mut self, cx: &mut gpui::Context<Self>) {
        let mut picker = CommitScopePicker::default();
        if let Some(review) = self.active_review() {
            if review.only_changed {
                picker.cursor = 1;
            } else if let Some(range) = &review.commit_range
                && let Some(commits) = self
                    .active_repo_id()
                    .and_then(|repo_id| self.pull_requests.repo(repo_id))
                    .and_then(|prs| prs.detail.ready())
                    .map(|detail| detail.commits.as_slice())
            {
                let newest = commits
                    .iter()
                    .position(|commit| commit.oid == range.selection.newest_oid);
                let oldest = commits
                    .iter()
                    .position(|commit| commit.oid == range.selection.oldest_oid);
                if let (Some(newest), Some(oldest)) = (newest, oldest) {
                    picker.selection.select_range(newest, oldest, commits.len());
                    picker.cursor = oldest + 2;
                }
            }
        }
        self.commit_scope_picker = Some(picker);
        cx.notify();
    }

    pub(super) fn render_commit_scope_picker(
        &self,
        picker: CommitScopePicker,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let scale = crate::ui_scale::UiScale::current(cx);
        let commits = self
            .active_repo_id()
            .and_then(|repo_id| self.pull_requests.repo(repo_id))
            .and_then(|prs| prs.detail.ready())
            .map(|detail| detail.commits.as_slice())
            .unwrap_or(&[]);
        let selected = picker.selection.range(commits.len());
        let mut body = components::modal_surface(theme)
            .p(scale.px(14.0))
            .flex()
            .flex_col()
            .gap(scale.px(5.0))
            .child(div().font_weight(FontWeight::BOLD).child("Review commits"))
            .child(
                div()
                    .text_color(theme.colors.foreground.secondary)
                    .child("j/k move · J/K extend · enter apply · esc cancel"),
            );
        let rows = commits.len() + 2;
        let start = picker.cursor.saturating_sub(8).min(rows.saturating_sub(18));
        let end = (start + 18).min(rows);
        if start > 0 {
            body = body.child(format!("{start} earlier rows above"));
        }
        for ix in start..end {
            let label = match ix {
                0 => "All changes".to_string(),
                1 => "Since last review".to_string(),
                _ => {
                    let commit = &commits[ix - 2];
                    let checked = selected.is_some_and(|(a, b)| (a..=b).contains(&(ix - 2)));
                    format!(
                        "{} {}  {}",
                        if checked { "☑" } else { "☐" },
                        commit.oid.get(..7).unwrap_or(&commit.oid),
                        commit.headline
                    )
                }
            };
            body = body.child(
                div()
                    .px(scale.px(8.0))
                    .py(scale.px(3.0))
                    .bg(if picker.cursor == ix {
                        theme.colors.interaction.selected_background
                    } else {
                        theme.colors.surface.panel
                    })
                    .child(label),
            );
        }
        if end < rows {
            body = body.child(format!("{} later commits below", rows - end));
        }
        let scrim = components::modal_scrim(theme)
            .id("commit_scope_scrim")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.commit_scope_picker = None;
                    cx.notify();
                }),
            );
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .child(scrim)
            .child(
                div()
                    .absolute()
                    .top(scale.px(80.0))
                    .left_0()
                    .w_full()
                    .flex()
                    .justify_center()
                    .child(div().w(scale.px(560.0)).child(body)),
            )
            .into_any_element()
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
    pub(super) fn review_diff_shown(&self) -> bool {
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
                    from_commit_id,
                } => {
                    head.as_ref() == review.range_head()
                        && shown == std::path::Path::new(path)
                        && self
                            .review_diff_base(review.repo_id, review.number)
                            .is_none_or(|base| from_commit_id.as_ref() == base)
                }
                _ => false,
            })
    }

    /// Whether the review's files past the first 100 are still being listed:
    /// until then `files` isn't the whole pull request.
    pub(super) fn review_files_listing(&self) -> bool {
        self.review.as_ref().is_some_and(|review| {
            review.commit_range.is_none()
                && self.pull_request_files_listing(review.repo_id, review.number)
        })
    }

    /// Some of the review's files couldn't be listed; `R` lists them again.
    pub(super) fn review_files_missing(&self) -> bool {
        self.review.as_ref().is_some_and(|review| {
            review.commit_range.is_none()
                && self
                    .pull_request_files_error(review.repo_id, review.number)
                    .is_some()
        })
    }

    /// Where the review's diff starts: your last review's commit while `L`
    /// shows the changes since it, else the pull request's merge base.
    /// `None` while neither is known.
    pub(super) fn review_diff_base(&self, repo_id: RepoId, number: u64) -> Option<String> {
        let review = self.review_of(repo_id, number)?;
        if let Some(range) = &review.commit_range {
            return range
                .changes
                .ready()
                .map(|changes| changes.base_oid.clone());
        }
        match review.since_base() {
            Some(base) => Some(base.to_string()),
            None => self
                .pull_requests
                .repo(repo_id)
                .and_then(|prs| prs.diff_base.ready())
                .cloned(),
        }
    }

    /// Copies the PR's generated-files set (once gix has read it) into the
    /// active review of it, so `ReviewMode`'s pure `&self` methods
    /// (`file_listed`, `generated_hidden`, `is_generated`) can consult it
    /// without reaching back through `self.pull_requests`.
    pub(super) fn review_sync_generated_files(
        &mut self,
        repo_id: RepoId,
        number: u64,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(generated) = self
            .pull_requests
            .repo(repo_id)
            .and_then(|prs| prs.generated_files.ready())
            .cloned()
        else {
            return;
        };
        if let Some(review) = self
            .review
            .as_mut()
            .filter(|review| review.repo_id == repo_id && review.number == number)
        {
            review.generated = generated;
            self.sync_review_marks(cx);
        }
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
        let cached_threads = prs.threads.ready().cloned();
        let selected_range = prs.commit_selection.selected_range(&detail.commits);
        let retry_diff_base = matches!(prs.diff_base, super::pull_requests::PrLoad::Failed(_));
        let generated = prs.generated_files.ready().cloned().unwrap_or_default();
        if detail.too_large_for_app() || (selected_range.is_none() && detail.files.is_empty()) {
            self.push_toast(
                components::ToastKind::Warning,
                format!(
                    "#{number} has more files than GitHub lists ({}). Press o to review it on GitHub.",
                    crate::github::MAX_LISTED_FILES
                ),
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
        let mut draft = draft.unwrap_or_else(|| ReviewDraft {
            repo: target.slug.clone(),
            number,
            head_oid: detail.head_oid.clone(),
            ..ReviewDraft::default()
        });
        // A draft from before viewed marks tracked a head: they were made on
        // the head its comments are pinned to.
        if draft.viewed_head.is_none() {
            draft.viewed_head = Some(draft.head_oid.clone());
        }
        if retry_diff_base {
            self.reset_pull_request_diff_base(repo_id);
        }
        // Opening the first file moves an older draft to the head, which
        // checks the viewed marks itself.
        let head_moves = draft.head_oid != detail.head_oid;
        let files: Vec<String> = detail.files.iter().map(|file| file.path.clone()).collect();
        let file_ix = files
            .iter()
            .position(|path| !draft.viewed.contains(path))
            .unwrap_or(0);
        self.review = Some(ReviewMode {
            repo_id,
            number,
            title: detail.title.clone(),
            all_files: files.clone(),
            files: if selected_range.is_some() {
                Vec::new()
            } else {
                files
            },
            file_ix,
            draft,
            selected_comment: None,
            head_moved: false,
            pending_jump: None,
            needs_cursor: true,
            armed_delete: None,
            write_seq: 0,
            written_seq: Default::default(),
            threads: cached_threads
                .as_ref()
                .map_or_else(Vec::new, |threads| threads.as_ref().clone()),
            threads_loading: cached_threads.is_none() && !cfg!(test),
            suggestions: Vec::new(),
            suggestion_generation: 0,
            since_base_oid: None,
            since_review: None,
            only_changed: false,
            commit_range: selected_range.map(|selection| CommitReviewRange {
                selection,
                changes: super::pull_requests::PrLoad::Loading,
                seq: next_seq(),
            }),
            viewed_syncing_to: None,
            since_seq: 0,
            viewed_sync: ViewedSync::Idle,
            viewed_seq: 0,
            dismissed: Default::default(),
            confirmed: Default::default(),
            was_dismissed: Default::default(),
            viewed_pushing: false,
            hunk_ranges: Default::default(),
            hunk_ranges_loading: Default::default(),
            hunk_key: None,
            reopened_for: None,
            query: Default::default(),
            show_viewed: false,
            generated,
            show_generated: false,
            generated_placeholder_dismissed: Default::default(),
        });
        self.main_pane.update(cx, |pane, cx| {
            pane.review_active = true;
            cx.notify();
        });
        // A filter left from another review doesn't carry over.
        self.sidebar_pane
            .update(cx, |pane, cx| pane.reset_review_query(cx));
        if self
            .review
            .as_ref()
            .is_some_and(|review| review.commit_range.is_some())
        {
            self.load_review_commit_range(cx);
        } else {
            self.review_open_file(file_ix, cx);
        }
        if !head_moves {
            self.review_sync_viewed(cx);
            self.load_viewed_states(cx);
        }
        if cached_threads.is_some() {
            self.sync_review_marks(cx);
        }
        self.load_review_threads(cx);
        if self
            .pull_requests
            .repo(repo_id)
            .and_then(|prs| prs.last_review.ready())
            .is_some()
        {
            self.review_load_changes_since(cx);
        } else if self.pull_requests.repo(repo_id).is_some_and(|prs| {
            matches!(
                prs.last_review,
                super::pull_requests::PrLoad::Idle | super::pull_requests::PrLoad::Failed(_)
            )
        }) {
            self.load_pull_request_last_review(repo_id, number, cx);
        }
        self.diff_return_panel = FocusPanel::Sidebar;
        self.focus_diff_when_open = true;
    }

    /// Enter on a conversation thread resumes the review on that file and line.
    pub(super) fn start_review_at_thread(
        &mut self,
        thread: &ReviewThread,
        cx: &mut gpui::Context<Self>,
    ) {
        // A thread belongs to the current PR diff, regardless of a range
        // left selected in Details.
        self.clear_pull_request_commit_selection(cx);
        self.start_review(cx);
        let Some(file_ix) = self
            .active_review()
            .and_then(|review| review.files.iter().position(|path| path == &thread.path))
        else {
            return;
        };
        self.review_open_file(file_ix, cx);
        if let Some(line) = thread.line.or(thread.original_line)
            && let Some(review) = self.review.as_mut()
        {
            review.pending_jump = Some((thread.side, line));
        }
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
        // A base merged in moves the merge base too; the hunk ranges follow
        // it through their key.
        let repo_id = self.review.as_ref().map(|review| review.repo_id);
        if let Some(repo_id) = repo_id {
            self.reset_pull_request_diff_base(repo_id);
        }
        self.save_review(cx);
        self.review_sync_viewed(cx);
        // GitHub works out which viewed files the new commits dismissed.
        self.load_viewed_states(cx);
        self.review_load_changes_since(cx);
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
        if self
            .active_repo()
            .is_some_and(|repo| repo.diff_state.diff_target.is_some())
        {
            self.store.dispatch(Msg::ClearDiffSelection {
                repo_id: review.repo_id,
            });
        }
        self.focus_panel(FocusPanel::Sidebar, window, cx);
        self.notify_pull_request_panes(cx);
    }

    /// After a submit: exactly the comments and replies that reached GitHub
    /// leave the draft. When the review itself went up and nothing is left,
    /// review mode closes; replies posted on their own leave the review as it
    /// was. Viewed marks belong to the pull request, not to one review, so
    /// they stay either way. Returns how many pending comments remain.
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
        self.save_review(cx);
        self.review = None;
        self.main_pane.update(cx, |pane, cx| {
            pane.review_active = false;
            pane.review_marks.clear();
            pane.review_thread_marks.clear();
            cx.notify();
        });
        if self.active_repo_id() == Some(repo_id) {
            if self
                .active_repo()
                .is_some_and(|repo| repo.diff_state.diff_target.is_some())
            {
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
        let (repo_id, path) = (review.repo_id, review.files[ix].clone());
        self.review_follow_head(cx);
        if self
            .pull_requests
            .repo(repo_id)
            .is_some_and(|prs| matches!(prs.diff_base, super::pull_requests::PrLoad::Idle))
        {
            self.fetch_pull_request_commits(repo_id, cx);
        }
        if self.review.as_ref().is_some_and(|review| {
            review
                .commit_range
                .as_ref()
                .is_some_and(|range| range.changes.ready().is_none())
        }) {
            self.store.dispatch(Msg::ClearDiffSelection { repo_id });
            self.notify_pull_request_panes(cx);
            return;
        }
        self.review_load_hunk_ranges(cx);
        self.sync_review_marks(cx);
        // A generated file's diff isn't fetched until its placeholder is
        // dismissed (`enter`): it's usually large and uninteresting (a
        // lockfile), so there's no point loading it before the reader asks.
        let placeholder_active = self
            .active_review()
            .is_some_and(ReviewMode::generated_placeholder_active);
        if let Some((base, head)) =
            self.review
                .as_ref()
                .and_then(|review| review.commit_range.as_ref())
                .and_then(|range| {
                    range.changes.ready().map(|changes| {
                        (changes.base_oid.clone(), range.selection.newest_oid.clone())
                    })
                })
        {
            // Leaving the previous target in place while the placeholder is
            // up is harmless: `review_diff_shown` already refuses to treat
            // another file's diff as this one's, and the placeholder covers
            // the main pane regardless of what's loaded underneath it.
            if !placeholder_active {
                self.store.dispatch(Msg::SelectDiff {
                    repo_id,
                    target: DiffTarget::CommitRange {
                        from_commit_id: CommitId(base.into()),
                        to_commit_id: Some(CommitId(head.into())),
                        path: Some(std::path::PathBuf::from(&path)),
                    },
                });
            }
            self.notify_pull_request_panes(cx);
            return;
        }
        // By path: review mode's list and the pull request's are the same,
        // but a reload at a new head can change the latter under it. A file
        // it no longer has shows no diff rather than another file's.
        let detail_ix = self
            .pull_requests
            .repo(repo_id)
            .and_then(|prs| prs.detail.ready())
            .and_then(|detail| detail.files.iter().position(|file| file.path == path));
        if detail_ix.is_some() {
            let base_and_head = self.review.as_ref().and_then(|review| {
                self.review_diff_base(repo_id, review.number)
                    .map(|base| (base, review.draft.head_oid.clone()))
            });
            // See the commit-range branch above: while the placeholder is up,
            // leave whatever was loaded before in place rather than clear it.
            if let (Some((base, head)), false) = (base_and_head, placeholder_active) {
                self.store.dispatch(Msg::SelectDiff {
                    repo_id,
                    target: DiffTarget::CommitRange {
                        from_commit_id: CommitId(base.into()),
                        to_commit_id: Some(CommitId(head.into())),
                        path: Some(std::path::PathBuf::from(&path)),
                    },
                });
            } else if !placeholder_active {
                self.store.dispatch(Msg::ClearDiffSelection { repo_id });
            }
        } else {
            // The last file's diff would carry this one's marks.
            self.store.dispatch(Msg::ClearDiffSelection { repo_id });
            let message = if self.review_files_listing() {
                format!("{path} is still being listed; try again in a moment.")
            } else if self.review_files_missing() {
                format!("{path} couldn't be listed; R lists it again.")
            } else {
                format!("{path} isn't part of this pull request any more.")
            };
            self.push_toast(components::ToastKind::Warning, message, cx);
        }
        self.notify_pull_request_panes(cx);
    }

    /// `enter` on the open file's "Generated file" placeholder: loads its
    /// diff for the rest of this review, reachable from the Sidebar or from
    /// the Diff panel itself once focus moves there.
    fn review_dismiss_generated_placeholder(&mut self, cx: &mut gpui::Context<Self>) {
        let Some((ix, path)) = self
            .active_review()
            .filter(|review| review.generated_placeholder_active())
            .and_then(|review| Some((review.file_ix, review.current_path()?.to_string())))
        else {
            return;
        };
        if let Some(review) = self.review.as_mut() {
            review.generated_placeholder_dismissed.insert(path);
        }
        // Re-opens the same file now that its placeholder is dismissed, which
        // is what actually dispatches the diff load `review_open_file` skips
        // while a generated file's placeholder is showing.
        self.review_open_file(ix, cx);
    }

    pub(super) fn set_review_commit_range(
        &mut self,
        selection: Option<super::pull_requests::SelectedCommitRange>,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(review) = self.review.as_mut() else {
            return;
        };
        review.only_changed = false;
        review.commit_range = selection.map(|selection| CommitReviewRange {
            selection,
            changes: super::pull_requests::PrLoad::Loading,
            seq: next_seq(),
        });
        if review.commit_range.is_some() {
            review.files.clear();
            review.file_ix = 0;
        } else {
            review.files = review.all_files.clone();
            review.file_ix = review.file_ix.min(review.files.len().saturating_sub(1));
        }
        review.reopened_for = None;
        // The range just changed: a run still in flight for the old one must
        // not land its findings (wrong lines, or `ReviewSuggestions`-routed
        // findings meant for a different range) once it finishes.
        review.suggestions.clear();
        review.suggestion_generation += 1;
        self.store.dispatch(Msg::ClearDiffSelection {
            repo_id: review.repo_id,
        });
        if self
            .review
            .as_ref()
            .is_some_and(|review| review.commit_range.is_some())
        {
            self.load_review_commit_range(cx);
        } else if let Some(ix) = self.review.as_ref().map(|review| review.file_ix) {
            self.review_open_file(ix, cx);
        }
        self.sync_review_marks(cx);
        self.notify_pull_request_panes(cx);
    }

    fn load_review_commit_range(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(review) = self.review.as_ref() else {
            return;
        };
        let Some(range) = review.commit_range.as_ref() else {
            return;
        };
        if cfg!(test) {
            return;
        }
        let (repo_id, number, seq) = (review.repo_id, review.number, range.seq);
        let (oldest, newest) = (
            range.selection.oldest_oid.clone(),
            range.selection.newest_oid.clone(),
        );
        let pr_files = review.all_files.clone();
        let Some(target) = self.github_target_for(repo_id) else {
            return;
        };
        let task = cx.background_executor().spawn(async move {
            crate::github::commit_range_changes(
                &target.workdir,
                &target.remote,
                &oldest,
                &newest,
                &pr_files,
            )
        });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                let Some(review) = this
                    .review
                    .as_mut()
                    .filter(|review| review.repo_id == repo_id && review.number == number)
                else {
                    return;
                };
                let Some(range) = review
                    .commit_range
                    .as_mut()
                    .filter(|range| range.seq == seq)
                else {
                    return;
                };
                range.changes = match result {
                    Ok(changes) => super::pull_requests::PrLoad::Ready(changes),
                    Err(err) => super::pull_requests::PrLoad::Failed(err),
                };
                if let Some(changes) = range.changes.ready() {
                    review.files = changes.files.iter().cloned().collect();
                    review.file_ix = review
                        .files
                        .iter()
                        .position(|path| !review.draft.viewed.contains(path))
                        .unwrap_or(0);
                }
                let first = (0..review.files.len())
                    .find(|ix| review.file_listed(*ix))
                    .or_else(|| (!review.files.is_empty()).then_some(0));
                if let Some(ix) = first {
                    this.review_open_file(ix, cx);
                } else {
                    this.sync_review_marks(cx);
                    this.notify_pull_request_panes(cx);
                }
            });
        })
        .detach();
    }

    fn review_step_file(&mut self, direction: i8, cx: &mut gpui::Context<Self>) {
        let Some(review) = self.review.as_ref() else {
            return;
        };
        let next = if direction < 0 {
            (0..review.file_ix).rev().find(|ix| review.file_listed(*ix))
        } else {
            (review.file_ix + 1..review.files.len()).find(|ix| review.file_listed(*ix))
        };
        if let Some(next) = next {
            self.review_open_file(next, cx);
        }
    }

    /// `L`: GitHub's "Changes since your last review". The file list keeps to
    /// the files changed since it and the diff starts at its commit; `L` again
    /// is the whole pull request.
    fn review_toggle_only_changed(&mut self, cx: &mut gpui::Context<Self>) {
        use super::pull_requests::PrLoad;
        let listing = self
            .review
            .as_ref()
            .is_some_and(|review| self.pull_request_files_listing(review.repo_id, review.number));
        let missing = self.review.as_ref().is_some_and(|review| {
            self.pull_request_files_error(review.repo_id, review.number)
                .is_some()
        });
        let last_review = self
            .review
            .as_ref()
            .and_then(|review| self.pull_requests.repo(review.repo_id))
            .map(|prs| &prs.last_review);
        let Some(review) = self.review.as_mut() else {
            return;
        };
        if review.only_changed {
            review.only_changed = false;
            let ix = review.file_ix;
            // Whole again: the diff starts at the merge base.
            self.review_open_file(ix, cx);
            return;
        }
        let refusal = match (last_review, &review.since_review) {
            (Some(PrLoad::Ready(None)), _) => {
                Some("You haven't reviewed this pull request before; every file is new to you.")
            }
            (_, Some(SinceReview::Gone)) => {
                Some("Your last review's commit is gone; showing everything.")
            }
            (_, Some(SinceReview::Failed(_))) => {
                Some("Couldn't work out what changed since your last review.")
            }
            (_, Some(SinceReview::Changed(_)))
                if review.files_changed_since_review() == 0 && listing =>
            {
                Some(
                    "None of the files listed so far changed since your last review; the rest are still being listed.",
                )
            }
            (_, Some(SinceReview::Changed(_)))
                if review.files_changed_since_review() == 0 && missing =>
            {
                Some(
                    "None of the files listed changed since your last review, but some couldn't be listed: R lists them again.",
                )
            }
            (_, Some(SinceReview::Changed(_))) if review.files_changed_since_review() == 0 => {
                Some("None of this pull request's files changed since your last review.")
            }
            (_, Some(SinceReview::Changed(_))) => None,
            (Some(PrLoad::Failed(_)), None) => Some("Couldn't load your last review."),
            _ => Some("Still working out what changed since your last review."),
        };
        if let Some(message) = refusal {
            self.push_toast(components::ToastKind::Warning, message.to_string(), cx);
            return;
        }
        review.only_changed = true;
        review.commit_range = None;
        review.files = review.all_files.clone();
        review.file_ix = review.file_ix.min(review.files.len().saturating_sub(1));
        let ix = (!review.file_listed(review.file_ix))
            .then(|| (0..review.files.len()).find(|ix| review.file_listed(*ix)))
            .flatten()
            .unwrap_or(review.file_ix);
        // Reopened either way: the diff now starts at your last review.
        self.review_open_file(ix, cx);
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
        if !review.all_files.contains(&path) {
            self.push_toast(
                components::ToastKind::Warning,
                "This file is not in the current PR's file list, so GitHub cannot mark it viewed."
                    .to_string(),
                cx,
            );
            return;
        }
        let now_viewed = review.draft.viewed.insert(path.clone());
        if !now_viewed {
            review.draft.viewed.remove(&path);
        }
        // Shown at once and kept in the draft until GitHub has it: laid over
        // any viewed-marks load on its way, and sent when GitHub is reachable.
        if !review.draft.unsynced.contains_key(&path) && review.dismissed.contains(&path) {
            review.was_dismissed.insert(path.clone());
        }
        review.dismissed.remove(&path);
        review.confirmed.remove(&path);
        let head = review.draft.head_oid.clone();
        review.draft.unsynced.insert(
            path.clone(),
            UnsyncedMark {
                viewed: now_viewed,
                head,
            },
        );
        self.review_push_viewed(cx);
        let Some(review) = self.review.as_mut() else {
            return;
        };
        let next = now_viewed
            .then(|| {
                let from = review.file_ix;
                (1..review.files.len())
                    .map(|step| (from + step) % review.files.len())
                    .find(|ix| {
                        review.file_listed(*ix) && !review.draft.viewed.contains(&review.files[*ix])
                    })
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

    /// Works out the files changed from your last review's commit to the
    /// reviewed head, fetching the old commit by id if it isn't local. The
    /// diff shown stays the whole pull request's, so line comments hold.
    pub(super) fn review_load_changes_since(&mut self, cx: &mut gpui::Context<Self>) {
        let old = self
            .review
            .as_ref()
            .and_then(|review| self.pull_requests.repo(review.repo_id))
            .and_then(|prs| prs.last_review.ready())
            .and_then(Option::as_ref)
            .map(|last| last.commit_id.clone())
            .filter(|old| !old.is_empty());
        let Some(review) = self.review.as_mut() else {
            return;
        };
        // What's known stays until the new answer lands: the diff `L` shows
        // keeps its base meanwhile.
        review.since_seq = next_seq();
        let seq = review.since_seq;
        let Some(old) = old else {
            review.since_review = None;
            review.since_base_oid = None;
            review.only_changed = false;
            self.review_refresh(cx);
            return;
        };
        let head = review.draft.head_oid.clone();
        if old == head {
            review.since_review = Some(SinceReview::Changed(Default::default()));
            review.since_base_oid = Some(old);
            review.only_changed = false;
            self.review_refresh(cx);
            return;
        }
        if cfg!(test) {
            return;
        }
        let (repo_id, number) = (review.repo_id, review.number);
        let old_for_result = old.clone();
        let Some(target) = self.github_target_for(repo_id) else {
            return;
        };
        let task = {
            let head = head.clone();
            cx.background_spawn(async move {
                crate::github::changes_since(&target.workdir, &target.remote, &old, &head)
            })
        };
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                let listing = this.pull_request_files_listing(repo_id, number);
                let Some(review) = this.review.as_mut().filter(|review| {
                    review.repo_id == repo_id
                        && review.number == number
                        && review.draft.head_oid == head
                        && review.since_seq == seq
                }) else {
                    return;
                };
                let since = match result {
                    Ok(changes) => {
                        review.since_base_oid = Some(old_for_result);
                        SinceReview::Changed(changes)
                    }
                    Err(crate::github::SinceFailure::Gone) => SinceReview::Gone,
                    Err(crate::github::SinceFailure::Failed(why)) => SinceReview::Failed(why),
                };
                review.since_review = Some(since);
                // `L` with nothing left to show would be an empty list, unless
                // files still being listed may yet fill it.
                review.only_changed &= listing || review.files_changed_since_review() > 0;
                this.review_refresh(cx);
            });
        })
        .detach();
    }

    /// After your last review, what changed since it or `L` changed: the
    /// marks and where comments may go follow at once. A diff that now
    /// starts elsewhere reopens from `review_after_render`.
    fn review_refresh(&mut self, cx: &mut gpui::Context<Self>) {
        self.sync_review_marks(cx);
        self.notify_pull_request_panes(cx);
    }

    /// The base the review's diff should start at, when the diff on screen
    /// is the review's file at its head but starts somewhere else.
    fn review_base_moved(&self) -> Option<String> {
        let review = self.active_review()?;
        let path = review.current_path()?;
        let base = self.review_diff_base(review.repo_id, review.number)?;
        let shown = self
            .active_repo()
            .and_then(|repo| repo.diff_state.diff_target.as_ref())?;
        match shown {
            DiffTarget::CommitRange {
                from_commit_id,
                to_commit_id: Some(head),
                path: Some(shown),
            } if head.as_ref() == review.range_head()
                && shown == std::path::Path::new(path)
                && from_commit_id.as_ref() != base =>
            {
                Some(base)
            }
            _ => None,
        }
    }

    /// Viewed marks made on an older head: the files changed since lose
    /// theirs, worked out off the UI thread. GitHub works that out itself
    /// once it holds the marks.
    fn review_sync_viewed(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(review) = self.review.as_mut() else {
            return;
        };
        if matches!(review.viewed_sync, ViewedSync::GitHub { .. }) {
            return;
        }
        let head = review.draft.head_oid.clone();
        let Some(from) = review
            .draft
            .viewed_head
            .clone()
            .filter(|from| *from != head)
        else {
            return;
        };
        if review.draft.viewed.is_empty() {
            // Nothing to un-view; the next save writes it.
            review.draft.viewed_head = Some(head);
            return;
        }
        if cfg!(test) || review.viewed_syncing_to.as_deref() == Some(head.as_str()) {
            return;
        }
        review.viewed_syncing_to = Some(head.clone());
        let (repo_id, number) = (review.repo_id, review.number);
        let Some(target) = self.github_target_for(repo_id) else {
            return;
        };
        let task = {
            let (from, head) = (from.clone(), head.clone());
            cx.background_spawn(async move {
                crate::github::changes_since(&target.workdir, &target.remote, &from, &head)
            })
        };
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| match result {
                Ok(changes) => {
                    this.review_viewed_moved(repo_id, number, &from, &head, Some(changes.files), cx)
                }
                // The old head is gone: nothing can say which marks still
                // hold, so they all go.
                Err(crate::github::SinceFailure::Gone) => {
                    this.review_viewed_moved(repo_id, number, &from, &head, None, cx)
                }
                // A passing failure leaves every mark as it was; the next
                // open tries again.
                Err(crate::github::SinceFailure::Failed(_)) => {
                    if let Some(review) = this.review.as_mut()
                        && review.viewed_syncing_to.as_deref() == Some(head.as_str())
                    {
                        review.viewed_syncing_to = None;
                    }
                }
            });
        })
        .detach();
    }

    /// The viewed marks' head moved from `from` to `head`, changing `changed`
    /// (`None`: the old head is gone, so every mark goes).
    fn review_viewed_moved(
        &mut self,
        repo_id: RepoId,
        number: u64,
        from: &str,
        head: &str,
        changed: Option<std::collections::BTreeSet<String>>,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(review) = self
            .review
            .as_mut()
            .filter(|review| review.repo_id == repo_id && review.number == number)
        else {
            return;
        };
        if review.viewed_syncing_to.as_deref() == Some(head) {
            review.viewed_syncing_to = None;
        }
        if review.draft.viewed_head.as_deref() != Some(from) || review.draft.head_oid != head {
            return;
        }
        // ponytail: a file marked while this ran and changed by the move
        // loses the fresh mark too; track marks per head if that ever bites.
        review.draft.viewed_moved(head, changed.as_ref());
        self.save_review(cx);
        self.notify_pull_request_panes(cx);
    }

    /// A minimal review of `files`, its cursor already placed, for tests that
    /// drive review mode's keys without the real open flow (`gh pr view`, its
    /// draft file, GitHub's viewed marks).
    #[cfg(test)]
    pub(super) fn open_review_for_test(
        &mut self,
        repo_id: RepoId,
        number: u64,
        files: Vec<String>,
        head_oid: impl Into<String>,
        cx: &mut gpui::Context<Self>,
    ) {
        self.review = Some(ReviewMode {
            repo_id,
            number,
            title: String::new(),
            all_files: files.clone(),
            files,
            file_ix: 0,
            draft: ReviewDraft {
                head_oid: head_oid.into(),
                ..ReviewDraft::default()
            },
            selected_comment: None,
            head_moved: false,
            pending_jump: None,
            needs_cursor: false,
            armed_delete: None,
            write_seq: 0,
            written_seq: Default::default(),
            threads: Vec::new(),
            threads_loading: false,
            suggestions: Vec::new(),
            suggestion_generation: 0,
            since_base_oid: None,
            since_review: None,
            only_changed: false,
            commit_range: None,
            viewed_syncing_to: None,
            since_seq: 0,
            viewed_sync: ViewedSync::Idle,
            viewed_seq: 0,
            dismissed: Default::default(),
            confirmed: Default::default(),
            was_dismissed: Default::default(),
            viewed_pushing: false,
            hunk_ranges: Default::default(),
            hunk_ranges_loading: Default::default(),
            hunk_key: None,
            reopened_for: None,
            query: Default::default(),
            show_viewed: false,
            generated: Default::default(),
            show_generated: false,
            generated_placeholder_dismissed: Default::default(),
        });
        self.main_pane.update(cx, |pane, _| pane.review_active = true);
        self.notify_pull_request_panes(cx);
    }

    /// Seeds the active review's generated-files set directly, bypassing the
    /// real gix fetch, for tests that only care about `Shift+G` and the
    /// generated-file placeholder.
    #[cfg(test)]
    pub(super) fn seed_review_generated_files_for_test(
        &mut self,
        paths: impl IntoIterator<Item = String>,
        cx: &mut gpui::Context<Self>,
    ) {
        if let Some(review) = self.review.as_mut() {
            review.generated = Arc::new(paths.into_iter().collect());
        }
        // The placeholder flag `sync_review_marks` copies into `main_pane`
        // only updates when this runs; production always reaches it because
        // `generated` and `review_open_file` land together (`start_review`).
        self.sync_review_marks(cx);
    }

    /// Your last review and what changed since it, as gh and git would load
    /// them, so tests run neither.
    #[cfg(test)]
    pub(super) fn seed_last_review_for_test(
        &mut self,
        last: Option<crate::github::LastReview>,
        since: Option<SinceReview>,
        cx: &mut gpui::Context<Self>,
    ) {
        if let Some(review) = self.review.as_mut() {
            self.pull_requests.repo_mut(review.repo_id).last_review =
                super::pull_requests::PrLoad::Ready(last.clone());
            review.since_base_oid = last.as_ref().map(|last| last.commit_id.clone());
            review.since_review = since;
        }
        self.notify_pull_request_panes(cx);
    }

    /// Loads your viewed marks from GitHub, which keeps them per user and
    /// pull request. Tests never run gh; they seed the answer.
    pub(super) fn load_viewed_states(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(target) = self
            .review
            .as_ref()
            .and_then(|review| self.github_target_for(review.repo_id))
        else {
            return;
        };
        let Some((repo_id, number, seq)) = self.review.as_mut().map(|review| {
            if matches!(review.viewed_sync, ViewedSync::Idle | ViewedSync::Offline) {
                review.viewed_sync = ViewedSync::Loading;
            }
            review.viewed_seq = next_seq();
            (review.repo_id, review.number, review.viewed_seq)
        }) else {
            return;
        };
        if cfg!(test) {
            return;
        }
        let task = cx.background_spawn(async move {
            crate::github::viewed_states(&target.workdir, &target.slug, number)
        });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                this.review_apply_viewed_states(repo_id, number, seq, result, cx)
            });
        })
        .detach();
    }

    /// GitHub's viewed marks, once loaded, are the truth: a file viewed there
    /// is viewed, a dismissed one (viewed, then changed) isn't. `space`
    /// presses it doesn't have yet (kept in the draft, across sessions) stay
    /// on top and go up to it, but only those made at the head shown. When
    /// GitHub can't be reached the marks on this computer stay, and say so.
    fn review_apply_viewed_states(
        &mut self,
        repo_id: RepoId,
        number: u64,
        seq: u64,
        result: Result<crate::github::ViewedStates, crate::github::PrError>,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(review) = self.review.as_mut().filter(|review| {
            review.repo_id == repo_id && review.number == number && review.viewed_seq == seq
        }) else {
            return;
        };
        let states = match result {
            Ok(states) => states,
            // A passing failure after GitHub answered once keeps its marks.
            Err(_) if matches!(review.viewed_sync, ViewedSync::GitHub { .. }) => return,
            Err(_) => {
                review.viewed_sync = ViewedSync::Offline;
                self.notify_pull_request_panes(cx);
                return;
            }
        };
        let first_answer = !matches!(review.viewed_sync, ViewedSync::GitHub { .. });
        let differed = review.merge_viewed_states(&states, seq);
        review.viewed_sync = ViewedSync::GitHub {
            pr_id: states.pr_id,
        };
        self.save_review(cx);
        self.review_push_viewed(cx);
        if first_answer && differed {
            self.push_toast(
                components::ToastKind::Success,
                "Viewed marks now come from GitHub.".to_string(),
                cx,
            );
        }
        self.notify_pull_request_panes(cx);
    }

    /// Sends the next `space` GitHub hasn't had yet, one at a time so they
    /// land in order; each answer sends the next. Only presses made at the
    /// head shown go.
    fn review_push_viewed(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(review) = self.review.as_mut() else {
            return;
        };
        let ViewedSync::GitHub { pr_id } = &review.viewed_sync else {
            return;
        };
        if review.viewed_pushing {
            return;
        }
        let head = &review.draft.head_oid;
        let Some((path, viewed)) = review
            .draft
            .unsynced
            .iter()
            .find(|(_, mark)| mark.head == *head)
            .map(|(path, mark)| (path.clone(), mark.viewed))
        else {
            return;
        };
        if cfg!(test) {
            return;
        }
        let (repo_id, number, pr_id) = (review.repo_id, review.number, pr_id.clone());
        let Some(target) = self.github_target_for(repo_id) else {
            return;
        };
        if let Some(review) = self.review.as_mut() {
            review.viewed_pushing = true;
        }
        let task = {
            let path = path.clone();
            cx.background_spawn(async move {
                crate::github::set_file_viewed(&target.workdir, &pr_id, &path, viewed)
            })
        };
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                this.review_viewed_pushed(repo_id, number, &path, viewed, result, cx)
            });
        })
        .detach();
    }

    /// GitHub's answer to one viewed mark. Taken: it leaves the unsynced
    /// presses, and stays laid over loads already on their way. Refused:
    /// that one mark, and its dismissed state, go back to what they were,
    /// unless it was pressed again since.
    pub(super) fn review_viewed_pushed(
        &mut self,
        repo_id: RepoId,
        number: u64,
        path: &str,
        viewed: bool,
        result: Result<(), crate::github::PrError>,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(review) = self
            .review
            .as_mut()
            .filter(|review| review.repo_id == repo_id && review.number == number)
        else {
            return;
        };
        review.viewed_pushing = false;
        let still_wanted = review
            .draft
            .unsynced
            .get(path)
            .is_some_and(|mark| mark.viewed == viewed);
        match result {
            Ok(()) if still_wanted => {
                review.draft.unsynced.remove(path);
                review.was_dismissed.remove(path);
                let at = review.viewed_seq;
                review
                    .confirmed
                    .insert(path.to_string(), ConfirmedMark { viewed, at });
                self.save_review(cx);
            }
            Err(err) if still_wanted => {
                review.draft.unsynced.remove(path);
                if review.draft.viewed.contains(path) == viewed {
                    if viewed {
                        review.draft.viewed.remove(path);
                    } else {
                        review.draft.viewed.insert(path.to_string());
                    }
                }
                if review.was_dismissed.remove(path) && !review.draft.viewed.contains(path) {
                    review.dismissed.insert(path.to_string());
                }
                self.save_review(cx);
                let what = if viewed { "viewed" } else { "not viewed" };
                self.push_toast(
                    components::ToastKind::Error,
                    format!("Couldn't mark {path} {what} on GitHub: {err}"),
                    cx,
                );
            }
            // Pressed again since: the newer press goes up next.
            _ => {}
        }
        self.review_push_viewed(cx);
        self.notify_pull_request_panes(cx);
    }

    /// With `L` on, the shown file's head-side hunk ranges in the pull
    /// request's own diff, once per file, merge base and head: GitHub takes
    /// comments only inside them.
    fn review_load_hunk_ranges(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(review) = self.review.as_ref() else {
            return;
        };
        let (Some(path), true) = (
            review.current_path(),
            review.since_base().is_some()
                || (review.commit_range.is_some() && !review.historical_range()),
        ) else {
            return;
        };
        let (repo_id, number, path, head) = (
            review.repo_id,
            review.number,
            path.to_string(),
            review.draft.head_oid.clone(),
        );
        let Some(merge_base) = self
            .pull_requests
            .repo(repo_id)
            .and_then(|prs| prs.diff_base.ready())
            .cloned()
        else {
            return;
        };
        let key = (merge_base.clone(), head.clone());
        let Some(review) = self.review.as_mut() else {
            return;
        };
        if review.hunk_key.as_ref() != Some(&key) {
            let had = review.hunk_key.is_some();
            review.hunk_ranges.clear();
            review.hunk_ranges_loading.clear();
            review.hunk_key = Some(key.clone());
            if had {
                self.sync_review_marks(cx);
            }
        }
        let Some(review) = self.review.as_mut() else {
            return;
        };
        if cfg!(test)
            || review.hunk_ranges.contains_key(&path)
            || review.hunk_ranges_loading.contains(&path)
        {
            return;
        }
        review.hunk_ranges_loading.insert(path.clone());
        let Some(target) = self.github_target_for(repo_id) else {
            return;
        };
        let task = {
            let path = path.clone();
            cx.background_spawn(async move {
                crate::github::pr_hunk_ranges(&target.workdir, &merge_base, &head, &path)
            })
        };
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                let Some(review) = this
                    .review
                    .as_mut()
                    .filter(|review| review.repo_id == repo_id && review.number == number)
                else {
                    return;
                };
                if review.hunk_key.as_ref() != Some(&key) {
                    // Worked out for an older head or merge base: again.
                    this.review_load_hunk_ranges(cx);
                    return;
                }
                review.hunk_ranges_loading.remove(&path);
                let lines = match result {
                    Ok(ranges) => SinceLines::Ranges(ranges),
                    Err(err) => {
                        this.push_toast(
                            components::ToastKind::Warning,
                            format!("Couldn't work out which lines of {path} the pull request changed: {err}"),
                            cx,
                        );
                        SinceLines::Failed
                    }
                };
                if let Some(review) = this.review.as_mut() {
                    review.hunk_ranges.insert(path, lines);
                }
                this.sync_review_marks(cx);
            });
        })
        .detach();
    }

    /// GitHub's viewed marks as gh would load them (or its failure), as the
    /// answer to the load numbered `seq`.
    #[cfg(test)]
    pub(super) fn seed_viewed_states_for_test(
        &mut self,
        seq: u64,
        result: Result<crate::github::ViewedStates, crate::github::PrError>,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some((repo_id, number)) = self
            .review
            .as_ref()
            .map(|review| (review.repo_id, review.number))
        else {
            return;
        };
        self.review_apply_viewed_states(repo_id, number, seq, result, cx);
    }

    /// The newest viewed-marks load, and each press as (path, viewed,
    /// confirmed by GitHub).
    #[cfg(test)]
    pub(super) fn viewed_load_for_test(&self) -> Option<(u64, Vec<ViewedPressForTest>)> {
        let review = self.review.as_ref()?;
        let unsynced = review
            .draft
            .unsynced
            .iter()
            .map(|(path, mark)| (path.clone(), (mark.viewed, false)));
        let confirmed = review
            .confirmed
            .iter()
            .map(|(path, mark)| (path.clone(), (mark.viewed, true)));
        let marks: std::collections::BTreeMap<_, _> = confirmed.chain(unsynced).collect();
        Some((
            review.viewed_seq,
            marks
                .into_iter()
                .map(|(path, (viewed, confirmed))| (path, viewed, confirmed))
                .collect(),
        ))
    }

    /// The review threads already on GitHub, and a file's hunk ranges, as gh
    /// and git would load them.
    #[cfg(test)]
    pub(super) fn seed_review_threads_for_test(
        &mut self,
        threads: Vec<ReviewThread>,
        hunk_ranges: Vec<(String, Vec<(u32, u32)>)>,
        cx: &mut gpui::Context<Self>,
    ) {
        if let Some(review) = self.review.as_mut() {
            review.threads = threads;
            review.threads_loading = false;
            review.hunk_ranges.extend(
                hunk_ranges
                    .into_iter()
                    .map(|(path, ranges)| (path, SinceLines::Ranges(ranges))),
            );
        }
        self.sync_review_marks(cx);
        self.notify_pull_request_panes(cx);
    }

    #[cfg(test)]
    pub(super) fn seed_commit_range_for_test(
        &mut self,
        base_oid: String,
        files: std::collections::BTreeSet<String>,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(review) = self.review.as_mut() else {
            return;
        };
        let Some(range) = review.commit_range.as_mut() else {
            return;
        };
        range.changes = super::pull_requests::PrLoad::Ready(crate::github::CommitRangeChanges {
            base_oid,
            files,
        });
        review.files = range
            .changes
            .ready()
            .unwrap()
            .files
            .iter()
            .cloned()
            .collect();
        review.file_ix = review
            .files
            .iter()
            .position(|path| !review.draft.viewed.contains(path))
            .unwrap_or(0);
        let first = (0..review.files.len())
            .find(|ix| review.file_listed(*ix))
            .or_else(|| (!review.files.is_empty()).then_some(0));
        if let Some(ix) = first {
            self.review_open_file(ix, cx);
        }
        self.sync_review_marks(cx);
        self.notify_pull_request_panes(cx);
    }

    /// The threads already on GitHub on the line under the cursor, oldest
    /// first. Two reviewers often start one each on the same line.
    pub(super) fn review_threads_at_cursor(&self, cx: &App) -> Vec<&ReviewThread> {
        let Some(review) = self.active_review() else {
            return Vec::new();
        };
        if review.historical_range() {
            return Vec::new();
        }
        let (Some(path), true) = (review.current_path(), self.review_diff_shown()) else {
            return Vec::new();
        };
        let Some(row) = self.main_pane.read(cx).review_cursor_row() else {
            return Vec::new();
        };
        // Since your last review the old side is that review's version, not
        // the base the threads' old lines count in.
        let old_line = row
            .old_line
            .filter(|_| review.since_base().is_none() && review.commit_range.is_none());
        threads_on_row(&review.threads, path, old_line, row.new_line)
    }

    /// Codex's suggestions on the line under the cursor, with their indices.
    pub(super) fn review_suggestions_at_cursor(&self, cx: &App) -> Vec<(usize, &ReviewComment)> {
        let Some(review) = self.active_review() else {
            return Vec::new();
        };
        if review.historical_range() {
            return Vec::new();
        }
        let (Some(path), true) = (review.current_path(), self.review_diff_shown()) else {
            return Vec::new();
        };
        let Some(row) = self.main_pane.read(cx).review_cursor_row() else {
            return Vec::new();
        };
        let since = review.since_base().is_some() || review.commit_range.is_some();
        review
            .suggestions
            .iter()
            .enumerate()
            .filter(|(_, suggestion)| {
                suggestion.anchor.path == path
                    && match suggestion.anchor.side {
                        ReviewSide::Left => !since && row.old_line == Some(suggestion.anchor.line),
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
            .as_ref()
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
        let total = suggestions.len();
        let (suggestions, dropped) = self.filter_suggestions_to_hunks(repo_id, number, suggestions);
        let Some(review) = self
            .review
            .as_mut()
            .filter(|review| review.repo_id == repo_id && review.number == number)
        else {
            return;
        };
        let count = suggestions.len();
        review.suggestions = suggestions;
        self.sync_review_marks(cx);
        self.notify_pull_request_panes(cx);
        let mut message = match count {
            0 if total == 0 => "Codex had no line comments to suggest.".to_string(),
            0 => "Codex's line comments were all outside the diff's hunks; none could be kept."
                .to_string(),
            n => format!(
                "Codex suggested {n} line comment{}. t steps to them; a adds one to your review, x drops it.",
                if n == 1 { "" } else { "s" }
            ),
        };
        if dropped > 0 && count > 0 {
            message.push_str(&format!(
                " ({dropped} outside the diff's hunks {} dropped)",
                if dropped == 1 { "was" } else { "were" }
            ));
        }
        self.push_toast(components::ToastKind::Success, message, cx);
    }

    /// Drops suggestions whose line falls outside the PR's own diff hunks
    /// (GitHub would refuse the whole review over one such line): best
    /// effort, so a path whose hunk ranges can't be read right now (still
    /// loading, or on the old side, which has no equivalent range here)
    /// passes its suggestions through unfiltered rather than guessing.
    /// Returns the kept suggestions and how many were dropped.
    fn filter_suggestions_to_hunks(
        &self,
        repo_id: RepoId,
        number: u64,
        suggestions: Vec<ReviewComment>,
    ) -> (Vec<ReviewComment>, usize) {
        let Some(workdir) = self
            .state
            .repos
            .iter()
            .find(|repo| repo.id == repo_id)
            .map(|repo| repo.spec.workdir.clone())
        else {
            return (suggestions, 0);
        };
        let Some(base) = self.review_diff_base(repo_id, number) else {
            return (suggestions, 0);
        };
        let Some(head) = self
            .review_of(repo_id, number)
            .map(|review| review.range_head().to_string())
        else {
            return (suggestions, 0);
        };
        let mut ranges_by_path: FxHashMap<String, Vec<(u32, u32)>> = FxHashMap::default();
        let mut dropped = 0;
        let kept = suggestions
            .into_iter()
            .filter(|comment| {
                // Only right-side (added/unchanged, new-file) lines have a
                // ready-made ranges helper; an old-side line is left as is.
                if comment.anchor.side != ReviewSide::Right {
                    return true;
                }
                let ranges = ranges_by_path
                    .entry(comment.anchor.path.clone())
                    .or_insert_with(|| {
                        crate::github::pr_hunk_ranges(&workdir, &base, &head, &comment.anchor.path)
                            .unwrap_or_default()
                    });
                let ok = ranges
                    .iter()
                    .any(|(lo, hi)| comment.anchor.line >= *lo && comment.anchor.line <= *hi);
                if !ok {
                    dropped += 1;
                }
                ok
            })
            .collect();
        (kept, dropped)
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
            // Since your last review, old-side lines count in another version.
            .filter(|(side, _)| review.since_base().is_none() || *side == ReviewSide::Right)
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

    /// `r` on an outdated conversation in Your review: a reply to it, which
    /// waits with the rest of the review like any other.
    fn review_reply_to_outdated(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let picked = self.active_review().and_then(|review| {
            let ix = review
                .selected_comment?
                .checked_sub(review.draft.comments.len())?;
            let thread = review.outdated_threads().nth(ix)?;
            Some((
                review.repo_id,
                review.number,
                ReviewAnchor {
                    path: thread.path.clone(),
                    side: thread.side,
                    line: thread.original_line?,
                    start: None,
                },
                ReplyTarget {
                    id: thread.root_id,
                    author: thread
                        .comments
                        .first()
                        .map(|comment| comment.author.clone())
                        .unwrap_or_default(),
                },
            ))
        });
        let Some((repo_id, number, anchor, reply_to)) = picked else {
            self.push_toast(
                components::ToastKind::Warning,
                "Pick an outdated conversation first (j/k); r in the diff replies to the thread on its line.".to_string(),
                cx,
            );
            return;
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
    /// diff's gutter marks, and where a comment may go.
    fn sync_review_marks(&mut self, cx: &mut gpui::Context<Self>) {
        use super::panes::main::ReviewCommentScope;
        type Marks = rustc_hash::FxHashSet<(ReviewSide, u32)>;
        let (marks, threads, suggestions, scope) = self
            .review
            .as_ref()
            .and_then(|review| {
                let path = review.current_path()?;
                let since = review.since_base().is_some() || review.commit_range.is_some();
                // Since your last review, old-side lines count in another
                // version: only head-side marks still point at their line.
                let keep = |key: &(ReviewSide, u32)| {
                    !review.historical_range() && (!since || key.0 == ReviewSide::Right)
                };
                // A reply to an outdated conversation sits on a line of an
                // older commit: nothing in this diff to mark.
                let marks: Marks = review
                    .draft
                    .comments
                    .iter()
                    .filter(|comment| comment.anchor.path == path)
                    .filter(|comment| !review.replies_to_outdated(comment))
                    .map(|comment| (comment.anchor.side, comment.anchor.line))
                    .filter(keep)
                    .collect();
                let threads: Marks = review
                    .threads
                    .iter()
                    .filter(|thread| thread.path == path)
                    .filter_map(|thread| Some((thread.side, thread.line?)))
                    .filter(keep)
                    .collect();
                let suggestions: Marks = review
                    .suggestions
                    .iter()
                    .filter(|suggestion| suggestion.anchor.path == path)
                    .map(|suggestion| (suggestion.anchor.side, suggestion.anchor.line))
                    .filter(keep)
                    .collect();
                let scope = if review.historical_range() {
                    ReviewCommentScope::Historical
                } else if review.commit_range.is_some() {
                    ReviewCommentScope::Range(
                        review
                            .hunk_ranges
                            .get(path)
                            .cloned()
                            .unwrap_or(SinceLines::Loading),
                    )
                } else if since {
                    ReviewCommentScope::Since(
                        review
                            .hunk_ranges
                            .get(path)
                            .cloned()
                            .unwrap_or(SinceLines::Loading),
                    )
                } else {
                    ReviewCommentScope::Full
                };
                Some((marks, threads, suggestions, scope))
            })
            .unwrap_or_default();
        let generated_placeholder = self
            .review
            .as_ref()
            .is_some_and(ReviewMode::generated_placeholder_active);
        self.main_pane.update(cx, |pane, cx| {
            pane.review_marks = marks;
            pane.review_thread_marks = threads;
            pane.review_suggestion_marks = suggestions;
            pane.review_comment_scope = scope;
            pane.review_generated_placeholder = generated_placeholder;
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
        // Your pending comments, then the outdated conversations below them.
        let len = review.draft.comments.len() + review.outdated_threads().count();
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
        let Some(comment) = review.draft.comments.get(ix).cloned() else {
            return;
        };
        let anchor = comment.anchor.clone();
        review.selected_comment = Some(ix);
        let refusal = if review.historical_range() {
            Some(
                "This comment is pinned to the current PR head; press C for All changes to open its line.",
            )
        } else if review.replies_to_outdated(&comment) {
            Some("This replies to an outdated conversation; its line isn't in this diff.")
        } else if review.commit_range.is_some() && anchor.side == ReviewSide::Left {
            Some(
                "This comment uses the full PR base's old lines; press C for All changes to open it.",
            )
        } else if review.since_base().is_some() && anchor.side == ReviewSide::Left {
            // The old side shown is your last review's version, not the base.
            Some("This comment is on an old line of the base; press L for the whole pull request.")
        } else {
            None
        };
        if let Some(message) = refusal {
            self.push_toast(components::ToastKind::Warning, message.to_string(), cx);
            self.notify_pull_request_panes(cx);
            return;
        }
        let Some(file_ix) = review.files.iter().position(|path| *path == anchor.path) else {
            let message = if review.commit_range.is_some() {
                format!(
                    "{} is outside the selected commit range; press C for All changes.",
                    anchor.path
                )
            } else if self.review_files_listing() {
                format!(
                    "{} is still being listed; try again once the file list is in.",
                    anchor.path
                )
            } else if self.review_files_missing() {
                format!("{} couldn't be listed; R lists it again.", anchor.path)
            } else {
                format!("{} isn't part of this pull request any more.", anchor.path)
            };
            self.push_toast(components::ToastKind::Warning, message, cx);
            return;
        };
        self.review_jump_to(file_ix, anchor.side, anchor.line, window, cx);
    }

    /// Opens `file_ix` (if it isn't already) and lands the cursor on
    /// `side`/`line` once its diff is on screen, focusing the diff (or
    /// asking to once it opens). Shared tail of `review_jump_to_comment`
    /// (above) and the reviewer menu's `b`-row jump
    /// (`reviewer_menu::review_jump_to_line`).
    pub(super) fn review_jump_to(
        &mut self,
        file_ix: usize,
        side: ReviewSide,
        line: u32,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(review) = self.review.as_ref() else {
            return;
        };
        if file_ix != review.file_ix {
            self.review_open_file(file_ix, cx);
        }
        // Lands once the file's diff is on screen (right away if it is).
        if let Some(review) = self.review.as_mut() {
            review.pending_jump = Some((side, line));
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
            let reopen_ix = active
                .then(|| self.review.as_ref().map(|review| review.file_ix))
                .flatten()
                .filter(|_| !self.review_diff_shown());
            let view = cx.entity();
            window.defer(cx, move |_window, cx| {
                main.update(cx, |pane, cx| {
                    pane.review_active = active;
                    cx.notify();
                });
                if let Some(ix) = reopen_ix {
                    view.update(cx, |this, cx| this.review_open_file(ix, cx));
                }
            });
        }
        if let Some(base) = self.review_base_moved()
            && let Some(review) = self.review.as_mut()
            && review.reopened_for.as_deref() != Some(base.as_str())
        {
            // `L`, your last review or what changed since it moved the base.
            review.reopened_for = Some(base);
            let ix = review.file_ix;
            let view = cx.entity();
            window.defer(cx, move |_window, cx| {
                view.update(cx, |this, cx| this.review_open_file(ix, cx));
            });
        }
        if self.active_review().is_some_and(|review| {
            review.since_base().is_some()
                || (review.commit_range.is_some() && !review.historical_range())
        }) {
            // Once the merge base is known, or after it moved.
            let view = cx.entity();
            window.defer(cx, move |_window, cx| {
                view.update(cx, |this, cx| this.review_load_hunk_ranges(cx));
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
        // A rendered block has no single text-diff line: reading the cursor
        // as one (`r`, `t`/`T`, `a`/`x`) would act on whatever row happens to
        // share that row number, not the block actually under the cursor.
        let markdown_preview = self.main_pane.read(cx).is_markdown_preview_active();
        match (current, lower.as_str(), shift) {
            (_, "c", true) => {
                self.open_commit_scope_picker(cx);
            }
            (_, "q", false) => self.leave_review(window, cx),
            (_, "s", true) => self.open_review_submit(window, cx),
            // History is hidden while reviewing; the diff stays.
            (_, "2", false) if !self.diff_is_open() => {}
            (Some(FocusPanel::Diff), "r", false) if markdown_preview => self.push_toast(
                components::ToastKind::Warning,
                "A rendered block has no line to reply on; c switches to Text.".to_string(),
                cx,
            ),
            (Some(FocusPanel::Diff), "r", false)
                if !self
                    .active_review()
                    .is_some_and(ReviewMode::historical_range) =>
            {
                self.review_reply_at_cursor(window, cx)
            }
            (Some(FocusPanel::Diff), "r", false) => self.push_toast(
                components::ToastKind::Warning,
                "This range ends before the PR head. Choose a range ending at the current head to add line comments.".to_string(), cx),
            (Some(FocusPanel::Details), "r", false) => self.review_reply_to_outdated(window, cx),
            // Elsewhere `r` does nothing rather than start another review.
            (_, "r", false) => {}
            (Some(FocusPanel::Diff), "t", _) if markdown_preview => self.push_toast(
                components::ToastKind::Warning,
                "A rendered block has no line to step threads by; c switches to Text.".to_string(),
                cx,
            ),
            (Some(FocusPanel::Diff), "t", _)
                if !self
                    .active_review()
                    .is_some_and(ReviewMode::historical_range) =>
            {
                self.review_step_thread(if shift { -1 } else { 1 }, cx)
            }
            (Some(FocusPanel::Diff), "t", _) => self.push_toast(
                components::ToastKind::Warning,
                "This range ends before the PR head. Choose a range ending at the current head to add line comments.".to_string(), cx),
            (_, "l", true) => self.review_toggle_only_changed(cx),
            (_, "v", true) => {
                if let Some(review) = self.review.as_mut() {
                    review.show_viewed = !review.show_viewed;
                }
                self.notify_pull_request_panes(cx);
            }
            (_, "g", true) => {
                if let Some(review) = self.review.as_mut() {
                    review.show_generated = !review.show_generated;
                }
                self.notify_pull_request_panes(cx);
            }
            (Some(FocusPanel::Sidebar | FocusPanel::Diff), "/", false) => self
                .sidebar_pane
                .update(cx, |pane, cx| pane.open_review_query(cx)),
            (Some(FocusPanel::Sidebar), "escape", false)
                if self
                    .active_review()
                    .is_some_and(|review| !review.query.is_empty()) =>
            {
                self.sidebar_pane
                    .update(cx, |pane, cx| pane.reset_review_query(cx))
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
                    let markdown_preview = self.main_pane.read(cx).is_markdown_preview_active();
                    self.defer_pane_action(self.main_pane.clone(), cx, move |pane, _, cx| {
                        if markdown_preview {
                            pane.review_move_markdown_block_cursor(i32::from(direction), cx)
                        } else {
                            pane.review_move_cursor(i32::from(direction), shift, cx)
                        }
                    });
                    self.notify_review_details_after_move(cx);
                }
            }
            // In the rendered preview a row has no single line to comment on,
            // so `c` does what GitHub's own preview offers instead: switch to
            // Text with the cursor already on the block's first source line.
            (Some(FocusPanel::Diff), "c", false)
                if self.main_pane.read(cx).is_markdown_preview_active() =>
            {
                self.review_switch_markdown_preview_to_text_at_cursor(cx)
            }
            (Some(FocusPanel::Diff), "c", false) => self.review_comment_at_cursor(window, cx),
            (Some(FocusPanel::Diff), "a" | "x", false) if markdown_preview => self.push_toast(
                components::ToastKind::Warning,
                "A rendered block isn't a suggestion's line; c switches to Text.".to_string(),
                cx,
            ),
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
                let dismissed_placeholder = self
                    .active_review()
                    .is_some_and(ReviewMode::generated_placeholder_active);
                self.review_dismiss_generated_placeholder(cx);
                if dismissed_placeholder {
                    // The diff this just asked for is still in flight (it was
                    // never requested while the placeholder was up); focus it
                    // as soon as it opens rather than needing a second enter.
                    self.focus_diff_when_open = true;
                } else if self.diff_is_open() {
                    self.focus_panel(FocusPanel::Diff, window, cx);
                }
            }
            // `enter` also loads a generated file's diff from the Diff panel
            // itself (the placeholder can be reached by moving there while
            // it's up, not only from the Sidebar).
            (Some(FocusPanel::Diff), "enter", false)
                if self
                    .active_review()
                    .is_some_and(ReviewMode::generated_placeholder_active) =>
            {
                self.review_dismiss_generated_placeholder(cx);
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
                let Some(ix) = self.active_review().and_then(|review| {
                    review
                        .selected_comment
                        .filter(|ix| *ix < review.draft.comments.len())
                }) else {
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

    /// The file list's `/` filter as typed, mirrored from the Sidebar's box.
    pub(super) fn review_set_query(
        &mut self,
        query: super::panes::ChangesQuery,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(review) = self.review.as_mut().filter(|review| review.query != query) else {
            return;
        };
        review.query = query;
        self.notify_pull_request_panes(cx);
    }

    /// `c` in the rendered preview: a rendered row has no single line to
    /// comment on the way a text diff row does, so this switches to Text
    /// instead, with the cursor already on the block's first source line.
    fn review_switch_markdown_preview_to_text_at_cursor(&mut self, cx: &mut gpui::Context<Self>) {
        let landed = self.main_pane.update(cx, |pane, cx| {
            let Some((side, line)) = pane.markdown_preview_cursor_source_line() else {
                return false;
            };
            pane.rendered_preview_modes
                .set(RenderedPreviewKind::Markdown, RenderedPreviewMode::Source);
            pane.diff_search_recompute_matches();
            let landed = pane.review_jump_to(side, line, cx);
            if !landed {
                // The mode already flipped to Text, so the markdown row index
                // still sitting in `diff_selection_anchor`/`range` no longer
                // names a row at all there; leaving it would draw the cursor
                // on whatever text-diff row happens to share that number.
                pane.diff_selection_anchor = None;
                pane.diff_selection_range = None;
            }
            landed
        });
        if landed {
            self.notify_review_details_after_move(cx);
        } else {
            self.push_toast(
                components::ToastKind::Warning,
                "That block has no line to switch to in Text.".to_string(),
                cx,
            );
        }
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
///
/// With `open`, every open pull request, the saved reviews of the others go
/// when all they keep is viewed marks GitHub already has: a submitted review
/// leaves its marks behind, and a merged pull request never needs them again.
/// Unsent comments and marks GitHub hasn't taken stay.
pub(super) fn pending_review_counts(
    repo: &str,
    open: Option<&rustc_hash::FxHashSet<u64>>,
) -> rustc_hash::FxHashMap<u64, usize> {
    gitcomet_state::session::review_drafts_dir()
        .map(|dir| pending_review_counts_in(&dir, repo, open))
        .unwrap_or_default()
}

fn pending_review_counts_in(
    dir: &std::path::Path,
    repo: &str,
    open: Option<&rustc_hash::FxHashSet<u64>>,
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
        let Some(draft) = ReviewDraft::peek(&entry.path(), repo, number) else {
            continue;
        };
        if !draft.comments.is_empty() {
            counts.insert(number, draft.comments.len());
        } else if draft.unsynced.is_empty() && open.is_some_and(|open| !open.contains(&number)) {
            let _ = std::fs::remove_file(entry.path());
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
                ..Default::default()
            };
            std::fs::write(dir.path().join(name), serde_json::to_vec(&draft).unwrap()).unwrap();
        };
        write("o~r~7.json", "o/r", 7, 2);
        write("o~r~8.json", "o/r", 8, 0);
        write("o~r~c~9.json", "o/r~c", 9, 1);
        write("o~r~10.json", "someone/else", 10, 1);
        std::fs::write(dir.path().join("o~r~11.json"), "not json").unwrap();
        std::fs::write(dir.path().join("o~r~12.json.unreadable"), "{}").unwrap();
        let counts = pending_review_counts_in(dir.path(), "o/r", None);
        assert_eq!(counts.len(), 1, "{counts:?}");
        assert_eq!(counts.get(&7), Some(&2));
        // Counting never moves an unreadable file aside.
        assert!(dir.path().join("o~r~11.json").exists());
    }

    #[test]
    fn viewed_marks_drop_on_files_changed_since_and_old_drafts_still_load() {
        // A draft written before viewed marks tracked a head.
        let old: ReviewDraft = serde_json::from_str(
            r#"{"repo": "o/r", "number": 7, "head_oid": "a", "comments": [], "viewed": ["a.rs", "b.rs", "c.rs"]}"#,
        )
        .expect("an old draft loads");
        assert_eq!(old.viewed_head, None);
        let mut draft = old.clone();
        let changed: std::collections::BTreeSet<String> =
            ["b.rs".to_string(), "elsewhere.rs".to_string()].into();
        draft.viewed_moved("b", Some(&changed));
        assert_eq!(
            draft.viewed.iter().map(String::as_str).collect::<Vec<_>>(),
            ["a.rs", "c.rs"]
        );
        assert_eq!(draft.viewed_head.as_deref(), Some("b"));
        // The old head is gone: nothing can be trusted as viewed.
        let mut gone = old;
        gone.viewed_moved("b", None);
        assert!(gone.viewed.is_empty());
        // Viewed marks alone keep the draft file.
        let marks_only = ReviewDraft {
            viewed: ["a.rs".to_string()].into(),
            ..Default::default()
        };
        assert!(marks_only.contents().expect("serializes").is_some());
    }

    #[test]
    fn saved_viewed_marks_alone_are_not_a_pending_review() {
        let dir = tempfile::tempdir().expect("tempdir");
        let draft = ReviewDraft {
            repo: "o/r".into(),
            number: 3,
            head_oid: "a".repeat(40),
            viewed: ["a.rs".to_string()].into(),
            viewed_head: Some("a".repeat(40)),
            ..Default::default()
        };
        std::fs::write(
            dir.path().join("o~r~3.json"),
            serde_json::to_vec(&draft).unwrap(),
        )
        .unwrap();
        assert!(pending_review_counts_in(dir.path(), "o/r", None).is_empty());
    }

    #[test]
    fn saved_reviews_of_closed_pull_requests_go_once_only_synced_marks_are_left() {
        let dir = tempfile::tempdir().expect("tempdir");
        let write = |number: u64, comments: usize, unsynced: bool| {
            let draft = ReviewDraft {
                repo: "o/r".into(),
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
                viewed: ["a.rs".to_string()].into(),
                unsynced: if unsynced {
                    [(
                        "a.rs".to_string(),
                        UnsyncedMark {
                            viewed: true,
                            head: "a".repeat(40),
                        },
                    )]
                    .into()
                } else {
                    Default::default()
                },
                ..Default::default()
            };
            let path = dir.path().join(format!("o~r~{number}.json"));
            std::fs::write(&path, serde_json::to_vec(&draft).unwrap()).unwrap();
            path
        };
        let open_marks = write(1, 0, false);
        let closed_marks = write(2, 0, false);
        let closed_comments = write(3, 1, false);
        let closed_unsynced = write(4, 0, true);
        // A list cut at its limit can't say what's closed: nothing goes.
        pending_review_counts_in(dir.path(), "o/r", None);
        assert!(closed_marks.exists());
        let open: rustc_hash::FxHashSet<u64> = [1].into_iter().collect();
        let counts = pending_review_counts_in(dir.path(), "o/r", Some(&open));
        assert_eq!(counts.get(&3), Some(&1));
        assert!(open_marks.exists());
        assert!(!closed_marks.exists());
        assert!(closed_comments.exists());
        assert!(closed_unsynced.exists());
    }

    /// A review of a.rs, b.rs and c.rs at head `h1`, with nothing loaded.
    fn test_review() -> ReviewMode {
        let mut review = ReviewMode {
            repo_id: RepoId(1),
            number: 7,
            title: String::new(),
            files: vec!["a.rs".into(), "b.rs".into(), "c.rs".into()],
            all_files: vec!["a.rs".into(), "b.rs".into(), "c.rs".into()],
            file_ix: 0,
            draft: ReviewDraft::default(),
            selected_comment: None,
            head_moved: false,
            pending_jump: None,
            needs_cursor: false,
            armed_delete: None,
            write_seq: 0,
            threads: Vec::new(),
            threads_loading: false,
            suggestions: Vec::new(),
            suggestion_generation: 0,
            written_seq: Default::default(),
            since_base_oid: None,
            since_review: None,
            only_changed: false,
            commit_range: None,
            viewed_syncing_to: None,
            since_seq: 0,
            viewed_sync: ViewedSync::Idle,
            viewed_seq: 0,
            dismissed: Default::default(),
            confirmed: Default::default(),
            was_dismissed: Default::default(),
            viewed_pushing: false,
            hunk_ranges: Default::default(),
            hunk_ranges_loading: Default::default(),
            hunk_key: None,
            reopened_for: None,
            query: Default::default(),
            show_viewed: false,
            generated: Default::default(),
            show_generated: false,
            generated_placeholder_dismissed: Default::default(),
        };
        review.draft.head_oid = "h1".into();
        review
    }

    /// A generated file that's also viewed is hidden, and counted once —
    /// under `generated_hidden`, never `viewed_hidden` too — until `Shift+G`
    /// shows it. Viewing it doesn't change that.
    #[test]
    fn generated_files_are_hidden_regardless_of_viewed_and_counted_once() {
        let mut review = test_review();
        review.generated = Arc::new(["b.rs".to_string()].into_iter().collect());
        // Nothing viewed yet: b.rs is hidden only because it's generated.
        assert_eq!(
            (0..review.files.len())
                .filter(|ix| review.file_listed(*ix))
                .collect::<Vec<_>>(),
            vec![0, 2]
        );
        assert_eq!(review.viewed_hidden(), 0);
        assert_eq!(review.generated_hidden(), 1);
        assert_eq!(review.non_generated_file_count(), 2);

        // Viewing the generated file changes nothing about its visibility or
        // which counter it falls under.
        review.draft.viewed.insert("b.rs".to_string());
        assert_eq!(
            (0..review.files.len())
                .filter(|ix| review.file_listed(*ix))
                .collect::<Vec<_>>(),
            vec![0, 2]
        );
        assert_eq!(review.viewed_hidden(), 0);
        assert_eq!(review.generated_hidden(), 1);

        // A separately viewed, non-generated file is hidden under
        // `viewed_hidden` instead.
        review.draft.viewed.insert("a.rs".to_string());
        assert_eq!(review.viewed_hidden(), 1);
        assert_eq!(review.generated_hidden(), 1);

        // Shift+G shows the generated file; it still doesn't count as viewed.
        review.show_generated = true;
        assert_eq!(
            (0..review.files.len())
                .filter(|ix| review.file_listed(*ix))
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(review.generated_hidden(), 0);
    }

    #[test]
    fn the_last_review_line_says_what_changed_since() {
        let mut review = test_review();
        let now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        let last = crate::github::LastReview {
            state: "APPROVED".into(),
            body: String::new(),
            // Two days before `now`.
            submitted_at: "2001-09-07T01:46:40Z".into(),
            commit_id: "abc1234def".into(),
        };
        assert_eq!(
            review.last_review_line(&last, now),
            "Your last review: Approved · 2 days ago · at abc1234"
        );
        review.since_review = Some(SinceReview::Changed(crate::github::ChangesSince {
            files: ["b.rs".to_string(), "not-in-pr.rs".to_string()].into(),
            commits: 3,
        }));
        assert_eq!(
            review.last_review_line(&last, now),
            "Your last review: Approved · 2 days ago · at abc1234 · 3 commits since · 1 file changed since"
        );
        // `L` keeps the walk to b.rs, and the diff starts at the last review.
        assert_eq!(review.since_base(), None);
        review.since_base_oid = Some(last.commit_id.clone());
        review.only_changed = true;
        assert_eq!(
            (0..3).map(|ix| review.file_listed(ix)).collect::<Vec<_>>(),
            [false, true, false]
        );
        assert_eq!(review.since_base(), Some("abc1234def"));
        review.since_review = Some(SinceReview::Gone);
        assert_eq!(review.since_base(), None);
    }

    fn github(files: &[(&str, crate::github::ViewedState)]) -> crate::github::ViewedStates {
        crate::github::ViewedStates {
            pr_id: "PR_1".into(),
            files: files
                .iter()
                .map(|(path, state)| (path.to_string(), *state))
                .collect(),
        }
    }

    fn unsynced(viewed: bool, head: &str) -> UnsyncedMark {
        UnsyncedMark {
            viewed,
            head: head.into(),
        }
    }

    #[test]
    fn an_offline_unmark_survives_a_new_session_and_goes_up() {
        use crate::github::ViewedState::*;
        // Offline, a.rs was un-marked; the draft on disk keeps the press.
        let mut draft = ReviewDraft {
            head_oid: "h1".into(),
            unsynced: [("a.rs".to_string(), unsynced(false, "h1"))].into(),
            ..Default::default()
        };
        draft = serde_json::from_slice(&draft.contents().unwrap().expect("kept")).unwrap();
        let mut review = test_review();
        review.draft = draft;
        // GitHub still has it viewed: the press wins, and waits to be sent.
        review.merge_viewed_states(&github(&[("a.rs", Viewed), ("b.rs", Viewed)]), 1);
        assert_eq!(
            review
                .draft
                .viewed
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["b.rs"]
        );
        assert_eq!(
            review.draft.unsynced.get("a.rs"),
            Some(&unsynced(false, "h1"))
        );
    }

    #[test]
    fn a_mark_made_at_an_older_head_is_not_sent() {
        use crate::github::ViewedState::*;
        let mut review = test_review();
        // Marked offline at h0; the pull request has moved to h1 since.
        review.draft.viewed = ["a.rs".to_string()].into();
        review.draft.unsynced = [("a.rs".to_string(), unsynced(true, "h0"))].into();
        review.merge_viewed_states(&github(&[("a.rs", Unviewed)]), 1);
        assert!(review.draft.unsynced.is_empty());
        assert!(review.draft.viewed.is_empty());
        assert_eq!(review.draft.viewed_head.as_deref(), Some("h1"));
    }

    #[test]
    fn a_web_unmark_is_not_overwritten_on_the_next_open() {
        use crate::github::ViewedState::*;
        let mut review = test_review();
        // The last answer had a.rs viewed; it was un-marked on github.com.
        review.draft.viewed = ["a.rs".to_string()].into();
        let differed = review.merge_viewed_states(&github(&[("a.rs", Unviewed)]), 1);
        assert!(differed, "worth saying the marks now come from GitHub");
        assert!(review.draft.viewed.is_empty());
        assert!(review.draft.unsynced.is_empty(), "nothing to send");
        // Dismissed there: not viewed here either.
        review.draft.viewed = ["b.rs".to_string()].into();
        review.merge_viewed_states(&github(&[("b.rs", Dismissed)]), 2);
        assert!(review.draft.viewed.is_empty());
        assert!(review.dismissed.contains("b.rs"));
    }

    #[test]
    fn viewed_files_leave_the_list_unless_shown() {
        let mut review = test_review();
        let listed = |review: &ReviewMode| {
            (0..review.files.len())
                .filter(|ix| review.file_listed(*ix))
                .collect::<Vec<_>>()
        };
        review.draft.viewed = ["a.rs".to_string(), "b.rs".to_string()].into();
        // a.rs is open, and viewed: it leaves the list like any other.
        assert_eq!(listed(&review), [2]);
        assert_eq!(review.viewed_hidden(), 2);
        review.file_ix = 2;
        assert_eq!(listed(&review), [2]);
        assert_eq!(review.viewed_hidden(), 2);
        review.show_viewed = true;
        assert_eq!(listed(&review), [0, 1, 2]);
        assert_eq!(review.viewed_hidden(), 0);
        // Changed since you viewed it on GitHub: not viewed, so listed.
        review.show_viewed = false;
        review.draft.viewed.remove("b.rs");
        review.dismissed.insert("b.rs".into());
        assert_eq!(listed(&review), [1, 2]);
    }

    #[test]
    fn the_file_filter_matches_like_the_changes_list_and_stacks_with_l() {
        let mut review = test_review();
        review.files = vec![
            "src/view/panel_focus.rs".into(),
            "docs/shortcuts.md".into(),
            "src/lib.rs".into(),
        ];
        let listed = |review: &ReviewMode| {
            (0..review.files.len())
                .filter(|ix| review.file_listed(*ix))
                .collect::<Vec<_>>()
        };
        review.query = super::super::panes::ChangesQuery::parse("pnl FOC");
        assert_eq!(listed(&review), [0]);
        review.query = super::super::panes::ChangesQuery::parse(".rs");
        assert_eq!(listed(&review), [0, 2]);
        // `L` on too: only what changed since your last review, of those.
        review.only_changed = true;
        review.since_review = Some(SinceReview::Changed(crate::github::ChangesSince {
            files: ["src/lib.rs".to_string(), "docs/shortcuts.md".to_string()].into(),
            commits: 1,
        }));
        assert_eq!(listed(&review), [2]);
        // Opening a filtered-out file doesn't bring it back into the list.
        review.file_ix = 1;
        assert_eq!(listed(&review), [2]);
    }

    #[test]
    fn threads_match_a_row_by_their_own_side() {
        let thread = |root_id, side, line| ReviewThread {
            root_id,
            path: "a.rs".into(),
            side,
            line,
            original_line: line,
            is_resolved: false,
            is_outdated: false,
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
