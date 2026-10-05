//! The `i` menu on a pull request (reviewer agents, docs/pr-mode.md): a scope picker and
//! a set of Codex actions driven by `.reviewer/`, replacing the plain Codex
//! menu (`codex_panel.rs`) while a pull request is on screen (the PR tab or
//! review mode). Everywhere else `i` still opens the plain menu unchanged.
//!
//! `.reviewer/` itself (parsing, the JSON result shapes) lives in
//! `crate::reviewer`; this module is the GPUI glue: where the menu's state
//! lives, what it looks like, how it decides which commit `.reviewer/` may
//! be trusted from, and how its actions turn into a
//! [`super::codex_panel::Material`] handed to
//! [`super::codex_panel::GitCometView::dispatch_codex`].

use super::*;
use super::codex_panel::{CodexDestination, Material, ResultShape};
use crate::github::{self, ReviewSide};
use crate::reviewer::{self, ReviewerConfig};
use gitcomet_state::model::SidebarMode;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// How much of the pull request an action's material covers. `tab` widens
/// it; it never narrows on its own.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ReviewScope {
    /// The selected lines (review mode's `Shift+J/K` range) and their hunk.
    Lines,
    /// The open file's diff.
    File,
    /// The reviewed commit range's diff (review mode's own — the commit
    /// picker selection or `L` since your last review — else the whole PR).
    Commits,
    /// The whole pull request's diff, `merge_base..head`.
    Pr,
}

impl ReviewScope {
    fn widen(self) -> Self {
        match self {
            Self::Lines => Self::File,
            Self::File => Self::Commits,
            Self::Commits | Self::Pr => Self::Pr,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Lines => "Lines",
            Self::File => "File",
            Self::Commits => "Commits",
            Self::Pr => "Whole PR",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ReviewerActionKind {
    Brief,
    Explain,
    Thread,
    Review,
    TestGaps,
    DescriptionVsCode,
    DraftSummary,
    Ask,
    /// `.reviewer/agents/*.md`, by index into the loaded config's (sorted)
    /// agent list.
    Agent(usize),
}

/// The menu's static actions, in order, each with its key and label. Agents
/// from `.reviewer/agents` are appended after these (`reviewer_menu_rows`).
const REVIEWER_ACTIONS: [(ReviewerActionKind, char, &str); 8] = [
    (ReviewerActionKind::Brief, 'b', "Brief me"),
    (ReviewerActionKind::Explain, 'e', "Explain this"),
    (ReviewerActionKind::Thread, 'h', "Thread"),
    (ReviewerActionKind::Review, 'r', "Review against the rules"),
    (ReviewerActionKind::TestGaps, 't', "Test gaps"),
    (ReviewerActionKind::DescriptionVsCode, 'v', "Description vs code"),
    (ReviewerActionKind::DraftSummary, 's', "Draft my review summary"),
    (ReviewerActionKind::Ask, 'q', "Ask"),
];

#[derive(Clone)]
pub(super) struct ReviewerMenuRow {
    pub(super) key: char,
    pub(super) label: String,
    kind: ReviewerActionKind,
    /// Why this row can't run right now, shown instead of running it.
    pub(super) disabled: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ReviewerMenuState {
    pub(super) scope: ReviewScope,
    pub(super) cursor: usize,
}

/// `.reviewer/` resolved for one PR base commit — or not.
#[derive(Clone)]
pub(super) enum ReviewerLoad {
    Ready(Arc<ReviewerConfig>),
    /// Couldn't establish a commit to trust `.reviewer/` from (the default
    /// branch, or where this PR's base leaves it, isn't known yet — e.g.
    /// its commits aren't fetched locally), or reading it failed outright.
    /// The reviewer menu runs with no `.reviewer` rules until this clears.
    Disabled(String),
}

impl ReviewerLoad {
    pub(super) fn config(&self) -> Option<&ReviewerConfig> {
        match self {
            Self::Ready(config) => Some(config),
            Self::Disabled(_) => None,
        }
    }
}

/// `.reviewer/` loads, one per PR base commit ([`ReviewerConfig`] is read
/// from a commit *trusted* to represent that base, never the PR head — see
/// [`reviewer_trusted_base_commit`]), kept for the run of the app. `None`
/// from `get` means "still loading" (or not asked for yet): never confused
/// with [`ReviewerLoad::Disabled`], which is a settled answer.
#[derive(Default)]
pub(super) struct ReviewerCache {
    entries: FxHashMap<String, ReviewerLoad>,
    loading: FxHashSet<String>,
}

impl ReviewerCache {
    pub(super) fn get(&self, base_oid: &str) -> Option<ReviewerLoad> {
        self.entries.get(base_oid).cloned()
    }
}

/// Everything a reviewer action needs about the pull request it runs on,
/// gathered once per dispatch.
struct ReviewerContext {
    repo_id: RepoId,
    workdir: PathBuf,
    number: u64,
    /// The PR's own base commit, as GitHub reports it (`baseRefOid`) — never
    /// read from directly; see [`reviewer_trusted_base_commit`].
    base_oid: String,
    /// The local merge base of base and head: what material diffs start
    /// from by default. `None` until the pull request's commits are fetched
    /// locally.
    merge_base: Option<String>,
    head_oid: String,
    /// The PR's own changed files (for "this PR changes .reviewer/…" and
    /// `ReviewerConfig::files_for`), not the current scope's files. Recomputed
    /// fresh at dispatch, per [`reviewer::touched_reviewer_files`] — never
    /// cached alongside a `.reviewer/` load, which is keyed by base commit
    /// and so can be shared by another pull request with a different file
    /// list.
    changed_files: Vec<String>,
    generated: Arc<BTreeSet<String>>,
    /// The repository's github.com remote, if it has one: needed to find
    /// the default branch `.reviewer/`'s trust is measured against.
    remote: Option<String>,
}

impl GitCometView {
    /// Whether `i` should open the reviewer menu instead of the plain Codex
    /// one: on the pull requests tab with one selected, or in review mode.
    pub(super) fn pr_reviewer_context_active(&self) -> bool {
        self.active_review().is_some()
            || (self.state.sidebar_mode == SidebarMode::PullRequests
                && self
                    .active_pull_requests()
                    .and_then(|prs| prs.selected)
                    .is_some())
    }

    /// Drops a `q` (Ask) left pending on a pull request that's no longer the
    /// one an Ask would actually run against — the sidebar left the PR tab
    /// (and review mode too), the repo changed, or a different pull request
    /// is now selected or being reviewed. Called on every state application
    /// and pull request selection, so `Enter` in the ask box never has a
    /// stale target to (mis)match against in the first place.
    pub(super) fn clear_pending_reviewer_ask_if_stale(&mut self) {
        let Some((repo_id, number, _)) = self.pending_reviewer_ask else {
            return;
        };
        let matches = self.pr_reviewer_context_active()
            && self.active_repo_id() == Some(repo_id)
            && self.reviewer_context().is_some_and(|context| context.number == number);
        if !matches {
            self.pending_reviewer_ask = None;
        }
    }

    fn reviewer_context(&self) -> Option<ReviewerContext> {
        let repo_id = self.active_repo_id()?;
        let repo = self.active_repo()?;
        let number = self
            .active_review()
            .map(|review| review.number)
            .or_else(|| self.active_pull_requests().and_then(|prs| prs.selected))?;
        let prs = self.pull_requests.repo(repo_id)?;
        let detail = prs.detail.ready()?;
        if detail.number != number {
            return None;
        }
        Some(ReviewerContext {
            repo_id,
            workdir: repo.spec.workdir.clone(),
            number,
            base_oid: detail.base_oid.clone(),
            merge_base: prs.diff_base.ready().cloned(),
            head_oid: detail.head_oid.clone(),
            changed_files: detail.files.iter().map(|file| file.path.clone()).collect(),
            generated: prs.generated_files.ready().cloned().unwrap_or_default(),
            remote: self.github_target_for(repo_id).map(|target| target.remote),
        })
    }

    /// The reviewer load for the active PR's base commit, if it has
    /// settled; kicks off the background load either way. `None` means
    /// still loading — callers hold dispatch and the menu says so, rather
    /// than treating that as "no `.reviewer/` folder".
    fn reviewer_config(
        &mut self,
        context: &ReviewerContext,
        cx: &mut gpui::Context<Self>,
    ) -> Option<ReviewerLoad> {
        let cached = self.reviewer_cache.get(&context.base_oid);
        // Test builds never run the real load: gpui's test scheduler aborts the
        // process when a background git task settles mid-test. Tests seed the
        // cache instead (`seed_reviewer_config_for_test`).
        if !cfg!(test)
            && cached.is_none()
            && !self.reviewer_cache.loading.contains(&context.base_oid)
        {
            self.reviewer_cache.loading.insert(context.base_oid.clone());
            let workdir = context.workdir.clone();
            let base_oid = context.base_oid.clone();
            let remote = context.remote.clone();
            let task = cx.background_spawn(smol::unblock({
                let base_oid = base_oid.clone();
                move || reviewer_load(&workdir, remote.as_deref(), &base_oid)
            }));
            cx.spawn(async move |view, cx| {
                let load = task.await;
                let _ = view.update(cx, |this, cx| {
                    this.reviewer_cache.loading.remove(&base_oid);
                    this.reviewer_cache.entries.insert(base_oid, load);
                    cx.notify();
                });
            })
            .detach();
        }
        cached
    }

    /// Where the scope starts: the review-mode line selection (only a
    /// genuine new-side range — a left-side or mixed-side selection has no
    /// hunk material to build, so it starts at File instead), else the
    /// current file (an open diff, or the thread under the cursor's file),
    /// else the whole PR.
    fn initial_reviewer_scope(&self, cx: &App) -> ReviewScope {
        if let Some(review) = self.active_review()
            && let Some(path) = review.current_path()
        {
            let selecting_new_side_range = self
                .main_pane
                .read(cx)
                .review_selection_anchor(path)
                .is_ok_and(|anchor| {
                    anchor.side == ReviewSide::Right
                        && anchor
                            .start
                            .is_some_and(|(start_side, _)| start_side == ReviewSide::Right)
                });
            if selecting_new_side_range {
                return ReviewScope::Lines;
            }
            return ReviewScope::File;
        }
        if self.thread_under_cursor(cx).is_some() {
            return ReviewScope::File;
        }
        ReviewScope::Pr
    }

    /// The thread under the cursor: the review diff's cursor first (the
    /// most recently active thread there), else the Comments tab's
    /// selection.
    fn thread_under_cursor(&self, cx: &App) -> Option<github::ReviewThread> {
        if let Some(thread) = self
            .review_threads_at_cursor(cx)
            .into_iter()
            .max_by_key(|thread| thread.comments.last().map(|comment| comment.at.clone()))
        {
            return Some(thread.clone());
        }
        self.selected_pull_request_thread()
    }

    /// `i` on a pull request: opens the reviewer menu at its starting scope
    /// and kicks off loading `.reviewer/` for the chips.
    pub(super) fn open_reviewer_menu(&mut self, cx: &mut gpui::Context<Self>) {
        let scope = self.initial_reviewer_scope(cx);
        self.reviewer_menu = Some(ReviewerMenuState { scope, cursor: 0 });
        if let Some(context) = self.reviewer_context() {
            self.reviewer_config(&context, cx);
        }
        cx.notify();
    }

    fn close_reviewer_menu(&mut self, cx: &mut gpui::Context<Self>) {
        self.reviewer_menu = None;
        cx.notify();
    }

    /// Lines/File's one open file, for `reviewer_scope_material`'s diff
    /// (Commits/Pr build their material from a base/head range instead, so
    /// they have no single path here — see `scope_files` for what they use
    /// for `.reviewer/areas` and agent `paths` matching).
    fn scope_path(&self, scope: ReviewScope) -> Option<String> {
        match scope {
            ReviewScope::Lines | ReviewScope::File => self
                .active_review()
                .and_then(|review| review.current_path())
                .map(str::to_string)
                .or_else(|| self.thread_under_cursor_path()),
            ReviewScope::Commits | ReviewScope::Pr => None,
        }
    }

    fn thread_under_cursor_path(&self) -> Option<String> {
        self.selected_pull_request_thread().map(|thread| thread.path)
    }

    /// The scope's own touched files, for `.reviewer/areas` and agent
    /// `paths` matching: the current file for Lines/File, the on-screen
    /// commit range's files for Commits, the pull request's full
    /// changed-file list for Pr ("Whole PR" genuinely means every file, not
    /// "unknown, so match everything"). An empty result means no files are
    /// in scope, so `ReviewerConfig::files_for`/`instructions_text` and the
    /// agent-disabling check below must treat it as "no areas apply", never
    /// as the old "empty means unrestricted".
    fn scope_files(&self, context: &ReviewerContext, scope: ReviewScope) -> Vec<String> {
        match scope {
            ReviewScope::Lines | ReviewScope::File => self.scope_path(scope).into_iter().collect(),
            ReviewScope::Commits => self
                .active_review()
                .filter(|review| review.repo_id == context.repo_id && review.number == context.number)
                .map(|review| review.files.clone())
                .unwrap_or_default(),
            ReviewScope::Pr => context.changed_files.clone(),
        }
    }

    /// The base/head a Lines, File or Commits scope's diff runs between:
    /// review mode's own choice (a picked commit range, or `L` since your
    /// last review), matching exactly what's on screen — not always the
    /// pull request's raw merge base and head.
    fn scope_diff_range(&self, context: &ReviewerContext) -> Result<(String, String), &'static str> {
        if let Some(review) = self
            .active_review()
            .filter(|review| review.repo_id == context.repo_id && review.number == context.number)
        {
            let base = self
                .review_diff_base(context.repo_id, context.number)
                .ok_or("Still loading this pull request's commits.")?;
            return Ok((base, review.range_head().to_string()));
        }
        let base = context
            .merge_base
            .clone()
            .ok_or("Still loading this pull request's commits.")?;
        Ok((base, context.head_oid.clone()))
    }

    /// The base/head the Whole PR scope always runs between — `p`'s own
    /// choice (merge base to the pull request's actual current head), never
    /// narrowed by a commit-range picker or `L`.
    fn whole_pr_diff_range(&self, context: &ReviewerContext) -> Result<(String, String), &'static str> {
        let base = context
            .merge_base
            .clone()
            .ok_or("Still loading this pull request's commits.")?;
        let head = self
            .active_review()
            .filter(|review| review.repo_id == context.repo_id && review.number == context.number)
            .map(|review| review.draft.head_oid.clone())
            .unwrap_or_else(|| context.head_oid.clone());
        Ok((base, head))
    }

    /// The menu's rows: the static actions, then one per `.reviewer/agents`
    /// entry (key `1`-`9`), each with why it can't run right now, if it
    /// can't — `h` with no thread under the cursor, or an agent whose
    /// `paths` don't match the current scope's file.
    fn reviewer_menu_rows(
        &self,
        load: Option<&ReviewerLoad>,
        context: Option<&ReviewerContext>,
        scope: ReviewScope,
        cx: &App,
    ) -> Vec<ReviewerMenuRow> {
        let has_thread = self.thread_under_cursor(cx).is_some();
        let mut rows: Vec<ReviewerMenuRow> = REVIEWER_ACTIONS
            .iter()
            .map(|&(kind, key, label)| ReviewerMenuRow {
                key,
                label: label.to_string(),
                kind,
                disabled: match kind {
                    ReviewerActionKind::Thread if !has_thread => {
                        Some("No thread under the cursor.")
                    }
                    _ => None,
                },
            })
            .collect();
        if let Some(config) = load.and_then(ReviewerLoad::config) {
            let paths = context.map(|context| self.scope_files(context, scope)).unwrap_or_default();
            for (ix, agent) in config.agents.iter().enumerate() {
                let disabled = (!agent.paths.is_empty()
                    && !paths.iter().any(|path| reviewer::area_matches(&agent.paths, path)))
                .then_some("Doesn't apply to this scope's files.");
                rows.push(ReviewerMenuRow {
                    key: agent.key,
                    label: agent.title.clone(),
                    kind: ReviewerActionKind::Agent(ix),
                    disabled,
                });
            }
        }
        rows
    }

    pub(super) fn handle_reviewer_menu_key(
        &mut self,
        keystroke: &gpui::Keystroke,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if self.reviewer_menu.is_none() {
            return false;
        }
        if !self.panel_keys_active(window, cx) && !self.codex_panel_focused(window) {
            self.close_reviewer_menu(cx);
            return false;
        }
        let mods = keystroke.modifiers;
        if mods.control || mods.alt || mods.platform || mods.function {
            return false;
        }
        let key = keystroke.key.as_str();
        if key == "escape" || key == "i" {
            self.close_reviewer_menu(cx);
            return true;
        }
        if key == "tab" {
            if let Some(state) = self.reviewer_menu.as_mut() {
                state.scope = state.scope.widen();
            }
            cx.notify();
            return true;
        }
        let context = self.reviewer_context();
        let load = context
            .as_ref()
            .and_then(|context| self.reviewer_cache.get(&context.base_oid));
        let scope = self.reviewer_menu.map_or(ReviewScope::Pr, |state| state.scope);
        let rows = self.reviewer_menu_rows(load.as_ref(), context.as_ref(), scope, cx);
        if rows.is_empty() {
            return true;
        }
        if key == "j" || key == "k" {
            if let Some(state) = self.reviewer_menu.as_mut() {
                state.cursor = if key == "j" {
                    (state.cursor + 1) % rows.len()
                } else {
                    (state.cursor + rows.len() - 1) % rows.len()
                };
            }
            cx.notify();
            return true;
        }
        let picked = if key == "enter" {
            let cursor = self.reviewer_menu.map_or(0, |state| state.cursor.min(rows.len() - 1));
            Some(rows[cursor].clone())
        } else {
            key.chars()
                .next()
                .and_then(|key| rows.iter().find(|row| row.key == key).cloned())
        };
        let Some(row) = picked else {
            // The menu is modal: other plain keys go nowhere.
            return true;
        };
        if let Some(reason) = row.disabled {
            self.push_toast(components::ToastKind::Warning, reason.to_string(), cx);
            return true;
        }
        self.close_reviewer_menu(cx);
        self.dispatch_reviewer_action(row.kind, scope, window, cx);
        true
    }

    /// Turns a reviewer action and scope into a [`Material`] and
    /// instructions, and starts the run through
    /// [`super::codex_panel::GitCometView::dispatch_codex`]. Also reached
    /// from `codex_panel.rs`'s ask box when a reviewer `q` (Ask) was asked
    /// to type a question first.
    pub(super) fn dispatch_reviewer_action(
        &mut self,
        kind: ReviewerActionKind,
        scope: ReviewScope,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(context) = self.reviewer_context() else {
            self.push_toast(
                components::ToastKind::Warning,
                "Select a pull request first.".to_string(),
                cx,
            );
            return;
        };
        let load = self.reviewer_config(&context, cx);
        // Hold dispatch while the `.reviewer/` load is still in flight,
        // rather than running with rules that silently aren't there yet (or
        // worse, than claiming there are none before that's actually known).
        let Some(load) = load else {
            self.push_toast(
                components::ToastKind::Warning,
                "Still loading .reviewer rules; try again in a moment.".to_string(),
                cx,
            );
            return;
        };
        let config = load.config();

        // An agent forces its own declared scope when it runs, regardless
        // of the menu's; `v`/`s` always run on the whole pull request.
        let scope = match kind {
            ReviewerActionKind::DescriptionVsCode | ReviewerActionKind::DraftSummary => {
                ReviewScope::Pr
            }
            ReviewerActionKind::Agent(ix) => config
                .and_then(|config| config.agents.get(ix))
                .and_then(|agent| agent.scope)
                .map(agent_review_scope)
                .unwrap_or(scope),
            _ => scope,
        };

        let scope_path = self.scope_path(scope);
        let paths = self.scope_files(&context, scope);
        let reviewer_text = config.map(|config| config.instructions_text(&paths)).unwrap_or_default();

        let question = self
            .codex
            .as_ref()
            .map(|panel| panel.ask_input.read(cx).text().trim().to_string())
            .unwrap_or_default();
        if kind == ReviewerActionKind::Ask && question.is_empty() {
            self.pending_reviewer_ask = Some((context.repo_id, context.number, scope));
            self.focus_codex_panel(window, cx);
            if let Some(panel) = self.codex.as_ref() {
                let handle = panel.ask_input.read(cx).focus_handle();
                window.focus(&handle, cx);
            }
            return;
        }

        // A historical range (ending before the PR head) carries range-head
        // line numbers that would land on the wrong lines once the range
        // changes back to All changes; route those findings to the Panel
        // only, never into the hidden `ReviewSuggestions` queue.
        let review_generation = self
            .active_review()
            .filter(|review| review.repo_id == context.repo_id && review.number == context.number)
            .filter(|review| !review.historical_range())
            .map(|review| review.suggestion_generation);

        let material_result: Result<Material, &'static str> = if kind == ReviewerActionKind::Thread {
            self.thread_material(&context, cx)
        } else {
            self.reviewer_scope_material(&context, scope, scope_path.as_deref(), cx)
        };
        let material = match material_result {
            Ok(material) => material,
            Err(reason) => {
                self.push_toast(components::ToastKind::Warning, reason.to_string(), cx);
                return;
            }
        };

        let (task_instructions, shape, destination, title) = match kind {
            ReviewerActionKind::Brief => (
                format!(
                    "Summarize this pull request scope for a reviewer about to read it: the \
                     overall change, a sensible reading order, and specific spots worth a closer \
                     look. {}",
                    reviewer::BRIEF_JSON_INSTRUCTIONS
                ),
                ResultShape::BriefRows,
                CodexDestination::Panel,
                "Brief me".to_string(),
            ),
            ReviewerActionKind::Explain => (
                "Explain what this change does and the likely reason, for a developer reading it \
                 for the first time."
                    .to_string(),
                ResultShape::PlainText,
                CodexDestination::Panel,
                "Explain this".to_string(),
            ),
            ReviewerActionKind::Thread => (
                "Summarize the conversation thread in the material below and draft a short, \
                 constructive reply. Whether a later commit already addresses it isn't available \
                 here — judge only from the file's current diff, also included."
                    .to_string(),
                ResultShape::PlainText,
                CodexDestination::Panel,
                "Thread".to_string(),
            ),
            ReviewerActionKind::Review => {
                let checklist_intro = match config {
                    Some(config) if !config.checklist.is_empty() => String::new(),
                    _ => reviewer::builtin_checklist_text(),
                };
                (
                    format!(
                        "{checklist_intro}\n\n{}",
                        reviewer::RULE_REVIEW_JSON_INSTRUCTIONS
                    )
                    .trim()
                    .to_string(),
                    ResultShape::RuleReview,
                    match review_generation {
                        Some(generation) => CodexDestination::ReviewSuggestions(
                            context.repo_id,
                            context.number,
                            generation,
                        ),
                        None => CodexDestination::Panel,
                    },
                    "Review against the rules".to_string(),
                )
            }
            ReviewerActionKind::TestGaps => (
                "List what tests are missing for this change: cases not covered, and why they \
                 matter."
                    .to_string(),
                ResultShape::PlainText,
                CodexDestination::Panel,
                "Test gaps".to_string(),
            ),
            ReviewerActionKind::DescriptionVsCode => (
                "The pull request's description comes first in the material, then its diff. \
                 Point out anything the description claims that the diff doesn't do, and \
                 anything notable the diff does that the description doesn't mention."
                    .to_string(),
                ResultShape::PlainText,
                CodexDestination::Panel,
                "Description vs code".to_string(),
            ),
            ReviewerActionKind::DraftSummary => {
                // `fill_pull_request_review_draft` only fills a summary into
                // the review/submit dialog while it's open on this PR; `s`
                // opens it itself, the same way `S` does, rather than
                // silently doing nothing until the user opens it by hand.
                self.open_pull_request_prompt(
                    PopoverKind::PullRequestReview {
                        repo_id: context.repo_id,
                        number: context.number,
                        kind: crate::github::ReviewKind::Comment,
                    },
                    window,
                    cx,
                );
                (
                    "Write a pull request review summary for the change below, in the \
                     reviewer's own words: the overall take, then the main points. Output only \
                     the summary, without code fences."
                        .to_string(),
                    ResultShape::PlainText,
                    CodexDestination::ReviewDraft(context.repo_id, context.number),
                    "Draft my review summary".to_string(),
                )
            }
            ReviewerActionKind::Ask => (
                format!("Answer this question about the change below: {question}"),
                ResultShape::PlainText,
                CodexDestination::Panel,
                format!("Ask: {question}"),
            ),
            ReviewerActionKind::Agent(ix) => {
                let Some(agent) = config.and_then(|config| config.agents.get(ix)) else {
                    self.push_toast(
                        components::ToastKind::Warning,
                        "That agent is no longer available.".to_string(),
                        cx,
                    );
                    return;
                };
                (
                    agent.body.clone(),
                    ResultShape::PlainText,
                    CodexDestination::Panel,
                    agent.title.clone(),
                )
            }
        };

        let material = if kind == ReviewerActionKind::DescriptionVsCode {
            let body = self
                .active_pull_requests()
                .and_then(|prs| prs.detail.ready())
                .filter(|detail| detail.number == context.number)
                .map(|detail| detail.body.clone())
                .unwrap_or_default();
            Material::WithPrefix {
                prefix: format!("Pull request description:\n{body}"),
                inner: Box::new(material),
            }
        } else {
            material
        };

        let instructions = if reviewer_text.is_empty() {
            task_instructions
        } else {
            format!("{reviewer_text}\n\n{task_instructions}")
        };
        // Computed fresh from the PR's own current file list, not cached
        // alongside the `.reviewer/` load (which is keyed by base commit,
        // shareable with another pull request whose files differ).
        let touched = reviewer::touched_reviewer_files(&context.changed_files);
        let title = match touched.first() {
            Some(file) => format!("{title} — This PR changes {file}"),
            None => title,
        };

        // The head (or, on a commit range, its range head) a Brief/Review
        // row's line numbers are anchored to, so `enter` on one later can
        // refuse the jump once this pull request or head has moved on.
        let jump_head = self
            .active_review()
            .filter(|review| review.repo_id == context.repo_id && review.number == context.number)
            .map(|review| review.range_head().to_string())
            .unwrap_or_else(|| context.head_oid.clone());
        self.dispatch_codex(
            context.repo_id,
            context.workdir,
            title,
            instructions,
            material,
            destination,
            shape,
            Some((context.number, jump_head)),
            false,
            kind == ReviewerActionKind::Ask,
            window,
            cx,
        );
    }

    /// `h` (Thread)'s material: the thread's own comments (untrusted —
    /// written by whoever could comment) ahead of the file's current diff,
    /// so Codex can actually judge whether it looks addressed.
    fn thread_material(&self, context: &ReviewerContext, cx: &App) -> Result<Material, &'static str> {
        let thread = self.thread_under_cursor(cx).ok_or("No thread under the cursor.")?;
        let (base, head) = self.scope_diff_range(context)?;
        Ok(Material::WithPrefix {
            prefix: format_thread_text(&thread),
            inner: Box::new(Material::FileDiff {
                base,
                head,
                path: thread.path,
            }),
        })
    }

    /// The plain, ungathered `Material` for one scope — a caller wraps it
    /// (`Material::WithPrefix`) or sends it as-is.
    fn reviewer_scope_material(
        &self,
        context: &ReviewerContext,
        scope: ReviewScope,
        scope_path: Option<&str>,
        cx: &App,
    ) -> Result<Material, &'static str> {
        match scope {
            ReviewScope::Lines => {
                let review = self
                    .active_review()
                    .filter(|review| review.repo_id == context.repo_id && review.number == context.number)
                    .ok_or("Open a file in review mode first.")?;
                let path = review.current_path().ok_or("Open a file first.")?.to_string();
                let anchor = self
                    .main_pane
                    .read(cx)
                    .review_selection_anchor(&path)
                    .map_err(|_| "Select a range of lines first (Shift+J/K).")?;
                let (start_side, start_line) = anchor
                    .start
                    .ok_or("Select a range of lines first (Shift+J/K).")?;
                if start_side != ReviewSide::Right || anchor.side != ReviewSide::Right {
                    return Err("Select a range on the new (right) side of the diff.");
                }
                let (lo, hi) = (start_line.min(anchor.line), start_line.max(anchor.line));
                let (base, head) = self.scope_diff_range(context)?;
                Ok(Material::FileDiffHunk {
                    base,
                    head,
                    path,
                    lo,
                    hi,
                })
            }
            ReviewScope::File => {
                let path = scope_path.map(str::to_string).ok_or("Open a file first.")?;
                let (base, head) = self.scope_diff_range(context)?;
                Ok(Material::ReviewerScopeDiff {
                    base,
                    head,
                    path: Some(path),
                    generated: (*context.generated).clone(),
                })
            }
            ReviewScope::Commits => {
                let (base, head) = self.scope_diff_range(context)?;
                Ok(Material::ReviewerScopeDiff {
                    base,
                    head,
                    path: None,
                    generated: (*context.generated).clone(),
                })
            }
            ReviewScope::Pr => {
                let (base, head) = self.whole_pr_diff_range(context)?;
                Ok(Material::ReviewerScopeDiff {
                    base,
                    head,
                    path: None,
                    generated: (*context.generated).clone(),
                })
            }
        }
    }


    /// Seeds `.reviewer/` for `base_oid` directly (as already `Ready`),
    /// bypassing the real git-backed load, for tests that only care about
    /// what a loaded config does (an agent's menu row, say).
    #[cfg(test)]
    pub(super) fn seed_reviewer_config_for_test(&mut self, base_oid: &str, config: ReviewerConfig) {
        self.reviewer_cache
            .entries
            .insert(base_oid.to_string(), ReviewerLoad::Ready(Arc::new(config)));
    }

    /// The material one scope would produce right now, without ever calling
    /// `gather` (a git diff run for real is real subprocess activity gpui's
    /// deterministic test scheduler refuses to let a test drive to
    /// completion) — the same path `dispatch_reviewer_action` takes up to
    /// (and excluding) `gather`, so a test can drive real keystrokes to open
    /// the menu and pick a scope, then check what that scope would send.
    #[cfg(test)]
    pub(super) fn reviewer_scope_material_for_test(
        &self,
        scope: ReviewScope,
        cx: &App,
    ) -> Result<Material, &'static str> {
        let context = self.reviewer_context().ok_or("Select a pull request first.")?;
        let scope_path = self.scope_path(scope);
        self.reviewer_scope_material(&context, scope, scope_path.as_deref(), cx)
    }

    /// `enter` on a `b` (brief me) row or an `r` finding row: jumps review
    /// mode to that row's file and line, on the new (right) side. Outside
    /// review mode, a jump has nowhere to land, so it's a no-op.
    pub(super) fn review_jump_to_line(
        &mut self,
        path: &str,
        line: u32,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(file_ix) = self
            .active_review()
            .and_then(|review| review.files.iter().position(|candidate| candidate == path))
        else {
            return;
        };
        self.review_jump_to(file_ix, ReviewSide::Right, line, window, cx);
    }

    pub(super) fn render_reviewer_menu(&self, cx: &mut gpui::Context<Self>) -> AnyElement {
        let theme = self.theme;
        let scale = crate::ui_scale::UiScale::current(cx);
        let Some(state) = self.reviewer_menu else {
            return div().into_any_element();
        };
        let context = self.reviewer_context();
        let load = context
            .as_ref()
            .and_then(|context| self.reviewer_cache.get(&context.base_oid));
        let paths = context.as_ref().map(|context| self.scope_files(context, state.scope)).unwrap_or_default();
        let rows = self.reviewer_menu_rows(load.as_ref(), context.as_ref(), state.scope, cx);

        let scope_row = div()
            .flex()
            .items_center()
            .justify_between()
            .py(scale.px(4.0))
            .child(
                div()
                    .text_size(scale.ui_text(13.0))
                    .text_color(theme.colors.foreground.primary)
                    .child(format!("Scope: {}", state.scope.label())),
            )
            .child(
                div()
                    .text_size(scale.ui_text(11.0))
                    .text_color(theme.colors.foreground.secondary)
                    .child("tab widens"),
            );

        let chip_text = match &load {
            None => "Loading .reviewer…".to_string(),
            Some(ReviewerLoad::Disabled(reason)) => reason.clone(),
            Some(ReviewerLoad::Ready(config)) if config.is_builtin() => {
                "No .reviewer folder: built-in reviewer".to_string()
            }
            Some(ReviewerLoad::Ready(config)) => {
                let chips = config.files_for(&paths);
                let base = if chips.is_empty() {
                    "No .reviewer files apply to this scope".to_string()
                } else {
                    chips.join(" · ")
                };
                if config.truncated {
                    format!("{base} (.reviewer text truncated at 64 KB)")
                } else {
                    base
                }
            }
        };
        let chip_row = div()
            .text_size(scale.ui_text(11.0))
            .text_color(theme.colors.foreground.secondary)
            .child(chip_text);

        let action_rows = rows.iter().enumerate().map(|(ix, row)| {
            div()
                .flex()
                .items_center()
                .gap(scale.px(12.0))
                .py(scale.px(3.0))
                .when(ix == state.cursor, |d| d.bg(theme.colors.interaction.hover_background))
                .child(
                    div()
                        .w(scale.px(24.0))
                        .flex_shrink_0()
                        .child(components::shortcut_keys(&row.key.to_string(), theme, scale)),
                )
                .child(
                    div()
                        .flex_1()
                        .text_size(scale.ui_text(13.0))
                        .text_color(if row.disabled.is_some() {
                            theme.colors.foreground.secondary
                        } else {
                            theme.colors.foreground.primary
                        })
                        .child(row.label.clone()),
                )
                .children(row.disabled.map(|reason| {
                    div()
                        .text_size(scale.ui_text(11.0))
                        .text_color(theme.colors.foreground.secondary)
                        .child(reason)
                }))
        });

        let body = components::modal_surface(theme)
            .p(scale.px(14.0))
            .flex()
            .flex_col()
            .gap(scale.px(6.0))
            .child(
                div()
                    .text_size(scale.ui_text(14.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.colors.foreground.primary)
                    .child("Reviewer"),
            )
            .child(scope_row)
            .child(chip_row)
            .children(action_rows)
            .child(
                div()
                    .pt(scale.px(6.0))
                    .text_size(scale.ui_text(12.0))
                    .text_color(theme.colors.foreground.secondary)
                    .child("Suggestions only: nothing is posted, committed or pushed. esc closes."),
            );
        let scrim = components::modal_scrim(theme).id("reviewer_menu_scrim").on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                this.close_reviewer_menu(cx);
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
                    .child(
                        div()
                            .debug_selector(|| "reviewer_menu".to_string())
                            .w(scale.px(400.0))
                            .child(body),
                    ),
            )
            .into_any_element()
    }
}

fn agent_review_scope(scope: reviewer::AgentScope) -> ReviewScope {
    match scope {
        reviewer::AgentScope::Lines => ReviewScope::Lines,
        reviewer::AgentScope::File => ReviewScope::File,
        reviewer::AgentScope::Commits => ReviewScope::Commits,
        reviewer::AgentScope::Pr => ReviewScope::Pr,
    }
}

fn format_thread_text(thread: &github::ReviewThread) -> String {
    let mut text = format!("Conversation thread on {}", thread.path);
    if let Some(line) = thread.line.or(thread.original_line) {
        text.push_str(&format!(", line {line}"));
    }
    text.push_str(":\n\n");
    for comment in &thread.comments {
        text.push_str(&format!("{} ({}):\n{}\n\n", comment.author, comment.at, comment.body));
    }
    text
}

/// Background body of [`GitCometView::reviewer_config`]: finds the commit
/// `.reviewer/` may be trusted from, then loads it — both steps run git, so
/// this all happens off the UI thread.
pub(in crate::view) fn reviewer_load(
    workdir: &Path,
    remote: Option<&str>,
    base_oid: &str,
) -> ReviewerLoad {
    let Some(remote) = remote else {
        return ReviewerLoad::Disabled("Rules off: no GitHub remote.".to_string());
    };
    let trusted = match reviewer_trusted_base_commit(workdir, remote, base_oid) {
        Ok(commit) => commit,
        Err(reason) => return ReviewerLoad::Disabled(format!("Rules off: {reason}.")),
    };
    let source = reviewer::GitReviewerSource {
        workdir,
        base_oid: &trusted,
    };
    match reviewer::load_reviewer_config(&source) {
        Ok(config) => ReviewerLoad::Ready(Arc::new(config)),
        Err(reason) => ReviewerLoad::Disabled(format!("Rules off: {reason}.")),
    }
}

/// The commit `.reviewer/` is trusted to be read from for a pull request
/// whose base is `base_oid`: `merge_base(base_oid, <default branch tip>)`,
/// never `base_oid` itself.
///
/// A pull request stacked on another, still-open one has that PR's own head
/// as its `base_oid` — that PR's author, not this repository's maintainers,
/// controls it until it merges into the default branch, so it is not a
/// trusted source of review rules. The merge base with the default branch
/// is the newest commit both share, which is always on the trusted,
/// already-merged side of that boundary.
fn reviewer_trusted_base_commit(
    workdir: &Path,
    remote: &str,
    base_oid: &str,
) -> Result<String, String> {
    let (default_name, default_oid) = github::default_branch_ref_and_oid(workdir, remote)
        .ok_or_else(|| "couldn't determine the default branch".to_string())?;
    github::prepare_diff_range(workdir, remote, base_oid, &default_oid)
        .map_err(|_| format!("couldn't find where this PR leaves {default_name}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reviewer::ReviewerSource;

    #[test]
    fn scope_widens_one_step_at_a_time_and_stops_at_pr() {
        assert_eq!(ReviewScope::Lines.widen(), ReviewScope::File);
        assert_eq!(ReviewScope::File.widen(), ReviewScope::Commits);
        assert_eq!(ReviewScope::Commits.widen(), ReviewScope::Pr);
        assert_eq!(ReviewScope::Pr.widen(), ReviewScope::Pr);
    }

    fn init_git_repo(workdir: &std::path::Path) {
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(workdir)
                .args(args)
                .output()
                .expect("git command to run");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
    }

    fn commit(workdir: &std::path::Path, file: &str, contents: &str) -> String {
        let path = workdir.join(file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&path, contents).expect("write file");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(workdir)
                .args(args)
                .output()
                .expect("git command to run");
            assert!(output.status.success(), "git {args:?} failed");
            output
        };
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "commit"]);
        let out = git(&["rev-parse", "HEAD"]);
        String::from_utf8(out.stdout).expect("utf8").trim().to_string()
    }

    /// The whole point of phase 6's `.reviewer` sandboxing: a PR stacked on
    /// another, unmerged one must not have `.reviewer/` read from that
    /// other PR's own head (which its author fully controls) — it must be
    /// read from the commit where the stack actually leaves the default
    /// branch.
    #[test]
    fn reviewer_reads_from_where_the_stacked_prs_base_leaves_default_not_its_head() {
        let workdir = std::env::temp_dir().join(format!(
            "gitcomet_ui_test_{}_reviewer_trusted_base",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&workdir);
        std::fs::create_dir_all(&workdir).expect("create workdir");
        init_git_repo(&workdir);

        // main: the default branch's tip, before the stack.
        let main_tip = commit(&workdir, "a.txt", "on main\n");
        std::process::Command::new("git")
            .arg("-C")
            .arg(&workdir)
            .args([
                "update-ref",
                "refs/remotes/origin/main",
                &main_tip,
            ])
            .status()
            .expect("update-ref");
        std::process::Command::new("git")
            .arg("-C")
            .arg(&workdir)
            .args([
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ])
            .status()
            .expect("symbolic-ref");

        // PR A's own branch and head — unmerged into main.
        std::process::Command::new("git")
            .arg("-C")
            .arg(&workdir)
            .args(["checkout", "-q", "-b", "pr-a"])
            .status()
            .expect("checkout");
        commit(&workdir, "b.txt", "PR A's own commit\n");
        // A malicious `.reviewer/checklist.md`, only on PR A's own branch —
        // `pr_a_head` (what PR B would see as its `base_oid`) is the commit
        // that adds it.
        let pr_a_head = commit(&workdir, ".reviewer/checklist.md", "- Nothing to see here\n");
        std::process::Command::new("git")
            .arg("-C")
            .arg(&workdir)
            .args(["checkout", "-q", "-"])
            .status()
            .expect("checkout back");

        // PR B is stacked on PR A: its base is PR A's own head.
        let trusted = reviewer_trusted_base_commit(&workdir, "origin", &pr_a_head)
            .expect("main is a known default branch locally");
        assert_eq!(
            trusted, main_tip,
            "must read .reviewer/ from where the stack leaves main, not from PR A's own head"
        );
        assert_ne!(trusted, pr_a_head);

        // Confirms the point: `.reviewer/checklist.md` at the untrusted
        // head exists (PR A really could plant one) but the trusted commit
        // has no `.reviewer/` at all.
        let untrusted_source = reviewer::GitReviewerSource {
            workdir: &workdir,
            base_oid: &pr_a_head,
        };
        assert!(
            !untrusted_source.list().unwrap_or_default().is_empty(),
            "PR A's own head does carry a .reviewer/ folder"
        );
        let trusted_source = reviewer::GitReviewerSource {
            workdir: &workdir,
            base_oid: &trusted,
        };
        assert!(trusted_source.list().unwrap_or_default().is_empty());

        let _ = std::fs::remove_dir_all(&workdir);
    }
}
