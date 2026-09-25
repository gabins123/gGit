//! The Codex panel: read-only Codex answers about the repository, drawn above
//! the bottom panel. `i` opens the action menu, `0` focuses the panel. How a
//! run is confined is `crate::codex`'s business; this module gathers the
//! material, runs it in the background and shows the answer as a suggestion.
//!
//! Nothing here commits, posts or pushes. The one write is filling the commit
//! message box, and only while that box is empty.

use super::*;
use crate::codex::{self, CodexError, CodexRequest};
use gitcomet_core::services::CancellationToken;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CodexAction {
    CommitMessage,
    ReviewLocal,
    ExplainDiff,
    ExplainCommit,
    ExplainFile,
    ReviewPullRequest,
    Ask,
}

impl CodexAction {
    /// The `i` menu, in order, with each action's key.
    pub(super) const MENU: [(Self, &'static str, &'static str); 7] = [
        (Self::CommitMessage, "m", "Generate commit message"),
        (Self::ReviewLocal, "r", "Review local changes"),
        (Self::ExplainDiff, "d", "Explain current diff"),
        (Self::ExplainCommit, "c", "Explain selected commit"),
        (Self::ExplainFile, "f", "Explain file"),
        (Self::ReviewPullRequest, "p", "Review pull request"),
        (Self::Ask, "q", "Ask about this repo"),
    ];

    fn title(self) -> &'static str {
        Self::MENU
            .iter()
            .find(|(action, ..)| *action == self)
            .map_or("Codex", |(_, _, title)| title)
    }
}

/// Where an answer goes besides the panel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CodexDestination {
    Panel,
    /// The review dialog for this repository's pull request, if it is still
    /// open and empty.
    ReviewDraft(RepoId, u64),
    /// Review mode's suggestions on lines, for this pull request, while the
    /// review is still on the head it had (`suggestion_generation`).
    ReviewSuggestions(RepoId, u64, u64),
}

enum RunState {
    Running,
    Done,
    Failed(CodexError),
}

/// A repository's latest run. Kept per repository and in memory only.
struct CodexRun {
    title: String,
    state: RunState,
    answer: String,
    cancel: CancellationToken,
    seq: u64,
}

pub(super) struct CodexPanel {
    pub(super) focus_handle: FocusHandle,
    result_input: Entity<components::TextInput>,
    result_scroll: ScrollHandle,
    ask_input: Entity<components::TextInput>,
    _ask_subscription: gpui::Subscription,
    _result_subscription: gpui::Subscription,
    runs: FxHashMap<RepoId, CodexRun>,
    pub(super) open: bool,
    /// The run whose answer the result box currently holds.
    shown: Option<(RepoId, u64)>,
    next_seq: u64,
}

/// What a run needs, captured on the main thread before it leaves it.
enum Material {
    Ready(String),
    StagedDiff,
    LocalChanges,
    Commit(String),
    FileAt {
        commit: Option<String>,
        path: std::path::PathBuf,
    },
    PullRequest {
        slug: String,
        number: u64,
    },
    /// The reviewed range of a pull request, from its local commits: the
    /// exact lines review mode shows.
    CommitRange {
        base: String,
        head: String,
    },
    RepoOverview,
}

fn is_object_id(value: &str) -> bool {
    (4..=64).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Just over the most Codex is sent: reading more would only be cut later.
const READ_LIMIT: u64 = codex::MAX_CONTEXT_BYTES as u64 + 1;

/// git's stdout, stopped at `READ_LIMIT` so a huge diff is never held whole.
fn git_output(workdir: &std::path::Path, args: &[&str]) -> Result<String, String> {
    use std::io::Read as _;
    let mut child = gitcomet_core::process::git_command()
        .current_dir(workdir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|err| err.to_string())?;
    let mut stdout = Vec::new();
    if let Some(pipe) = child.stdout.take() {
        let _ = pipe.take(READ_LIMIT).read_to_end(&mut stdout);
    }
    let cut = stdout.len() as u64 >= READ_LIMIT;
    if cut {
        let _ = child.kill();
    }
    let output = child.wait_with_output().map_err(|err| err.to_string())?;
    if !cut && !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

/// A working-tree file for Codex. A symlink is described, never followed:
/// following one could send any file on disk.
fn read_worktree_file(path: &std::path::Path) -> Result<String, String> {
    use std::io::Read as _;
    let metadata = std::fs::symlink_metadata(path).map_err(|err| err.to_string())?;
    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(path).map_err(|err| err.to_string())?;
        return Ok(format!("(a symbolic link to {})", target.display()));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|err| err.to_string())?
        .take(READ_LIMIT)
        .read_to_end(&mut bytes)
        .map_err(|err| err.to_string())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Runs on a background thread: git and gh reads only.
fn gather(workdir: &std::path::Path, material: Material) -> Result<String, String> {
    match material {
        Material::Ready(text) => Ok(text),
        Material::StagedDiff => {
            let diff = git_output(
                workdir,
                &["diff", "--cached", "--no-color", "--no-ext-diff"],
            )?;
            if diff.trim().is_empty() {
                return Err("Nothing is staged.".to_string());
            }
            Ok(diff)
        }
        Material::LocalChanges => {
            let diff = git_output(workdir, &["diff", "HEAD", "--no-color", "--no-ext-diff"])?;
            let untracked = git_output(workdir, &["ls-files", "--others", "--exclude-standard"])?;
            if diff.trim().is_empty() && untracked.trim().is_empty() {
                return Err("There are no local changes.".to_string());
            }
            Ok(format!("{diff}\nUntracked files:\n{untracked}"))
        }
        Material::Commit(sha) => git_output(
            workdir,
            &[
                "show",
                "--no-color",
                "--no-ext-diff",
                "--stat",
                "--patch",
                "--format=fuller",
                &sha,
                "--",
            ],
        ),
        Material::FileAt { commit, path } => {
            let label = path.to_string_lossy();
            let contents = match commit {
                Some(sha) => {
                    let spec_path = if cfg!(windows) {
                        label.replace('\\', "/")
                    } else {
                        label.to_string()
                    };
                    git_output(
                        workdir,
                        &["show", "--no-color", &format!("{sha}:{spec_path}")],
                    )?
                }
                None => read_worktree_file(&workdir.join(&path))?,
            };
            // The name stays inside the data: a crafted filename is not an
            // instruction.
            Ok(format!("File: {label}\n\n{contents}"))
        }
        Material::PullRequest { slug, number } => {
            crate::github::diff(workdir, &slug, number).map_err(|err| err.to_string())
        }
        Material::CommitRange { base, head } => {
            if !is_object_id(&base) || !is_object_id(&head) {
                return Err("The reviewed commits aren't known yet.".to_string());
            }
            git_output(
                workdir,
                &[
                    "diff",
                    "--no-color",
                    "--no-ext-diff",
                    &format!("{base}..{head}"),
                ],
            )
        }
        Material::RepoOverview => {
            // The log first: a long file list is what gets cut.
            let log = git_output(workdir, &["log", "--oneline", "-30", "--no-color"])?;
            let files = git_output(workdir, &["ls-files"])?;
            Ok(format!("Recent commits:\n{log}\nFiles:\n{files}"))
        }
    }
}

fn instructions(action: CodexAction, question: &str) -> String {
    match action {
        CodexAction::CommitMessage => "Write a git commit message for the staged changes below: a summary line under 72 characters, then a blank line and a short body only if the change needs explaining. Output only the message, without code fences.".to_string(),
        CodexAction::ReviewLocal => "Review these uncommitted changes as a careful senior reviewer: bugs, risky edge cases, missing tests, unclear code. Be specific and brief, and cite files and lines where you can.".to_string(),
        CodexAction::ExplainDiff => "Explain what this diff changes and the likely reason, for a developer reading it for the first time.".to_string(),
        CodexAction::ExplainCommit => "Explain this commit: what it changes, the likely intent, and anything that looks risky.".to_string(),
        CodexAction::ExplainFile => "Explain the file named at the top of the material: its purpose, its main parts and how they fit together.".to_string(),
        CodexAction::ReviewPullRequest => "Write a code review of this pull request's patch, as a comment its author will read: the important issues first, then smaller ones. Be specific and constructive.".to_string(),
        CodexAction::Ask => format!(
            "Answer this question about the repository: {question}\nThe material lists its files and recent commits; say what you can't tell from it."
        ),
    }
}

impl GitCometView {
    fn codex_panel(
        &mut self,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> &mut CodexPanel {
        if self.codex.is_none() {
            let theme = self.theme;
            let result_scroll = ScrollHandle::new();
            let result_input = cx.new(|cx| {
                let mut input = components::TextInput::new(
                    components::TextInputOptions {
                        placeholder: "Codex's answer appears here. It is yours to edit.".into(),
                        multiline: true,
                        soft_wrap: true,
                        min_lines: 6,
                        ..Default::default()
                    },
                    window,
                    cx,
                );
                input.set_vertical_scroll_handle(Some(result_scroll.clone()));
                input.set_theme(theme, cx);
                input
            });
            let ask_input = cx.new(|cx| {
                let mut input = components::TextInput::new(
                    components::TextInputOptions {
                        placeholder: "Ask about this repo, then press Enter".into(),
                        ..Default::default()
                    },
                    window,
                    cx,
                );
                input.set_theme(theme, cx);
                input
            });
            let ask_subscription = cx.observe_in(&ask_input, window, |this, input, window, cx| {
                let enter = input.update(cx, |input, _| input.take_enter_pressed());
                let escape = input.update(cx, |input, _| input.take_escape_pressed());
                if enter {
                    this.start_codex(CodexAction::Ask, CodexDestination::Panel, window, cx);
                } else if escape && let Some(panel) = this.codex.as_ref() {
                    let handle = panel.focus_handle.clone();
                    window.focus(&handle, cx);
                }
            });
            let focus_handle = cx.focus_handle().tab_index(0).tab_stop(false);
            let result_subscription = cx.observe_in(&result_input, window, {
                let focus_handle = focus_handle.clone();
                move |_this, input, window, cx| {
                    if input.update(cx, |input, _| input.take_escape_pressed()) {
                        window.focus(&focus_handle, cx);
                    }
                }
            });
            self.codex = Some(CodexPanel {
                focus_handle,
                result_input,
                result_scroll,
                ask_input,
                _ask_subscription: ask_subscription,
                _result_subscription: result_subscription,
                runs: FxHashMap::default(),
                open: false,
                shown: None,
                next_seq: 0,
            });
        }
        self.codex.as_mut().expect("created above")
    }

    /// `0`: opens the panel and focuses it.
    pub(super) fn focus_codex_panel(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        if let Some(from) = self.focused_panel(window, cx) {
            self.codex_return_panel = from;
        }
        let panel = self.codex_panel(window, cx);
        panel.open = true;
        let handle = panel.focus_handle.clone();
        window.focus(&handle, cx);
        cx.notify();
    }

    fn close_codex_panel(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        if let Some(panel) = self.codex.as_mut() {
            panel.open = false;
        }
        let back = Some(self.codex_return_panel)
            .filter(|panel| self.panel_available(*panel))
            .unwrap_or_else(|| self.default_panel());
        self.focus_panel(back, window, cx);
    }

    pub(super) fn codex_panel_focused(&self, window: &Window) -> bool {
        self.codex
            .as_ref()
            .is_some_and(|panel| panel.open && panel.focus_handle.is_focused(window))
    }

    /// Runs `action` for the active repository. Refuses up front, with a
    /// toast, when its material isn't there (nothing staged, no diff open…).
    pub(super) fn start_codex(
        &mut self,
        action: CodexAction,
        destination: CodexDestination,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(repo) = self.active_repo() else {
            return;
        };
        let repo_id = repo.id;
        let workdir = repo.spec.workdir.clone();
        let material = match action {
            CodexAction::CommitMessage => Ok(Material::StagedDiff),
            CodexAction::ReviewLocal => Ok(Material::LocalChanges),
            CodexAction::ExplainDiff => match &repo.diff_state.diff {
                Loadable::Ready(diff) if repo.diff_state.diff_target.is_some() => {
                    // Built only up to what Codex is sent, on this thread.
                    let mut text = String::new();
                    for line in &diff.lines {
                        let line: &str = line.text.as_ref();
                        if text.len() + line.len() + 1 > codex::MAX_CONTEXT_BYTES {
                            break;
                        }
                        text.push_str(line);
                        text.push('\n');
                    }
                    Ok(Material::Ready(text))
                }
                _ => Err("Open a diff first."),
            },
            CodexAction::ExplainCommit => match &repo.history_state.selected_commit {
                Some(commit) if is_object_id(&commit.0) => {
                    Ok(Material::Commit(commit.0.to_string()))
                }
                _ => Err("Select a commit first."),
            },
            CodexAction::ExplainFile => match &repo.diff_state.diff_target {
                Some(DiffTarget::WorkingTree { path, .. }) => Ok(Material::FileAt {
                    commit: None,
                    path: path.clone(),
                }),
                Some(DiffTarget::Commit {
                    commit_id,
                    path: Some(path),
                })
                | Some(DiffTarget::CommitRange {
                    to_commit_id: Some(commit_id),
                    path: Some(path),
                    ..
                }) if is_object_id(&commit_id.0) => Ok(Material::FileAt {
                    commit: Some(commit_id.0.to_string()),
                    path: path.clone(),
                }),
                _ => Err("Open a file's diff first."),
            },
            CodexAction::ReviewPullRequest => {
                // The dialog's own pull request, else the selected one.
                let prs = self.active_pull_requests();
                let number = match destination {
                    CodexDestination::ReviewDraft(_, number)
                    | CodexDestination::ReviewSuggestions(_, number, _) => Some(number),
                    CodexDestination::Panel => prs.and_then(|prs| prs.selected),
                };
                let too_large = prs
                    .and_then(|prs| prs.detail.ready())
                    .is_some_and(|detail| {
                        Some(detail.number) == number && detail.too_large_for_app()
                    });
                // Review mode reads the reviewed head's own diff, so Codex's
                // line numbers are the ones on screen.
                let reviewed = match destination {
                    CodexDestination::ReviewSuggestions(..) => Some((
                        prs.and_then(|prs| prs.diff_base.ready().cloned()),
                        self.active_review()
                            .map(|review| review.draft.head_oid.clone()),
                    )),
                    _ => None,
                };
                match (number, self.github_target()) {
                    _ if too_large => Err("This pull request is too large to review here."),
                    _ if matches!(reviewed, Some((None, _) | (_, None))) => {
                        Err("Open a file of the review first, so its commits are here.")
                    }
                    (Some(_), Some(_)) if reviewed.is_some() => {
                        let Some((Some(base), Some(head))) = reviewed else {
                            unreachable!("checked just above");
                        };
                        Ok(Material::CommitRange { base, head })
                    }
                    (Some(number), Some(target)) => Ok(Material::PullRequest {
                        slug: target.slug,
                        number,
                    }),
                    _ => Err("Select a pull request first."),
                }
            }
            CodexAction::Ask => Ok(Material::RepoOverview),
        };
        let material = match material {
            Ok(material) => material,
            Err(reason) => {
                self.push_toast(components::ToastKind::Warning, reason.to_string(), cx);
                return;
            }
        };
        let question = self
            .codex
            .as_ref()
            .map(|panel| panel.ask_input.read(cx).text().trim().to_string())
            .unwrap_or_default();
        if action == CodexAction::Ask && question.is_empty() {
            self.focus_codex_panel(window, cx);
            if let Some(panel) = self.codex.as_ref() {
                let handle = panel.ask_input.read(cx).focus_handle();
                window.focus(&handle, cx);
            }
            return;
        }

        let instructions = match destination {
            CodexDestination::ReviewSuggestions(..) => {
                super::review::SUGGESTION_INSTRUCTIONS.to_string()
            }
            _ => instructions(action, &question),
        };
        let title = match action {
            CodexAction::Ask => format!("Ask: {question}"),
            _ => action.title().to_string(),
        };

        let panel = self.codex_panel(window, cx);
        if let Some(previous) = panel.runs.get(&repo_id) {
            // One run at a time per repository.
            previous.cancel.cancel();
        }
        panel.next_seq += 1;
        let seq = panel.next_seq;
        let cancel = CancellationToken::new();
        panel.runs.insert(
            repo_id,
            CodexRun {
                title,
                state: RunState::Running,
                answer: String::new(),
                cancel: cancel.clone(),
                seq,
            },
        );
        panel.open = true;
        // The box holds this run's answer or nothing, never a stale one.
        panel.shown = None;
        panel
            .result_input
            .update(cx, |input, cx| input.set_text("", cx));
        if action == CodexAction::Ask {
            panel
                .ask_input
                .update(cx, |input, cx| input.set_text("", cx));
        }

        // A run takes minutes; it gets its own thread, not a pool worker.
        let task = cx.background_spawn(smol::unblock(move || {
            let material = gather(&workdir, material).map_err(CodexError::Failed)?;
            codex::run(
                CodexRequest {
                    instructions,
                    material,
                },
                &cancel,
            )
        }));
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let _ = view.update(cx, |this, cx| {
                this.finish_codex(repo_id, seq, action, destination, result, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn finish_codex(
        &mut self,
        repo_id: RepoId,
        seq: u64,
        action: CodexAction,
        destination: CodexDestination,
        result: Result<String, CodexError>,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(run) = self
            .codex
            .as_mut()
            .and_then(|panel| panel.runs.get_mut(&repo_id))
            .filter(|run| run.seq == seq)
        else {
            return;
        };
        match result {
            Ok(answer) => {
                run.state = RunState::Done;
                run.answer = answer.clone();
                if action == CodexAction::CommitMessage && repo_id_is_active(self, repo_id) {
                    // The user's own text is never replaced.
                    let input = self.details_pane.read(cx).commit_message_input.clone();
                    if input.read(cx).text().trim().is_empty() {
                        input.update(cx, |input, cx| input.set_text(answer.clone(), cx));
                    }
                }
                match destination {
                    CodexDestination::ReviewDraft(draft_repo, number) => {
                        self.popover_host.update(cx, |host, cx| {
                            host.fill_pull_request_review_draft(draft_repo, number, answer, cx)
                        });
                    }
                    CodexDestination::ReviewSuggestions(review_repo, number, generation) => {
                        self.add_review_suggestions(review_repo, number, generation, &answer, cx);
                    }
                    CodexDestination::Panel => {}
                }
            }
            Err(CodexError::Cancelled) => run.state = RunState::Failed(CodexError::Cancelled),
            Err(err) => run.state = RunState::Failed(err),
        }
        cx.notify();
    }

    /// Keys while the Codex panel itself has focus. `true` when consumed.
    pub(super) fn handle_codex_panel_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let Some(repo_id) = self.active_repo_id() else {
            return false;
        };
        let Some(panel) = self.codex.as_ref() else {
            return false;
        };
        let text = panel.result_input.read(cx).text().to_string();
        match key {
            "y" => {
                if !text.trim().is_empty() {
                    crate::clipboard::write_text(
                        cx,
                        text,
                        crate::clipboard::CopySource::ContextMenu,
                    );
                    self.push_toast(components::ToastKind::Success, "Copied".to_string(), cx);
                }
            }
            "u" => {
                let Some(number) = self.active_pull_requests().and_then(|prs| prs.selected) else {
                    self.push_toast(
                        components::ToastKind::Warning,
                        "Select a pull request first.".to_string(),
                        cx,
                    );
                    return true;
                };
                self.open_pull_request_prompt(
                    PopoverKind::PullRequestReview {
                        repo_id,
                        number,
                        kind: crate::github::ReviewKind::Comment,
                    },
                    window,
                    cx,
                );
                self.popover_host.update(cx, |host, cx| {
                    host.fill_pull_request_review_draft(repo_id, number, text, cx)
                });
            }
            "e" => {
                let handle = panel.result_input.read(cx).focus_handle();
                window.focus(&handle, cx);
            }
            "a" => {
                let handle = panel.ask_input.read(cx).focus_handle();
                window.focus(&handle, cx);
            }
            "s" => {
                if let Some(run) = panel.runs.get(&repo_id) {
                    run.cancel.cancel();
                }
            }
            "x" | "escape" => self.close_codex_panel(window, cx),
            _ => return false,
        }
        true
    }

    /// The panel above the bottom panel, when open for the active repository.
    pub(super) fn render_codex_panel(
        &mut self,
        theme: AppTheme,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Option<AnyElement> {
        let repo_id = self.active_repo_id()?;
        let focused = self.codex_panel_focused(window);
        let panel = self.codex.as_mut().filter(|panel| panel.open)?;
        let scale = crate::ui_scale::UiScale::current(cx);

        // Put the active repository's answer in the box once, so the user's
        // edits survive re-renders.
        let run = panel.runs.get(&repo_id);
        let wanted = run
            .filter(|run| matches!(run.state, RunState::Done))
            .map(|run| (repo_id, run.seq));
        if wanted.is_none() && panel.shown.is_some() {
            // Another repository, a new run or a failed one: nothing to show.
            panel.shown = None;
            panel
                .result_input
                .update(cx, |input, cx| input.set_text("", cx));
        } else if wanted.is_some() && panel.shown != wanted {
            panel.shown = wanted;
            let answer = run.map(|run| run.answer.clone()).unwrap_or_default();
            panel
                .result_input
                .update(cx, |input, cx| input.set_text(answer, cx));
        }

        let (title, status) = match run {
            None => (
                "Codex".to_string(),
                "Press i for actions, or a to ask.".to_string(),
            ),
            Some(run) => (
                run.title.clone(),
                match &run.state {
                    RunState::Running => "Running… s stops".to_string(),
                    RunState::Done => {
                        "Suggestion only: edit, copy or use it; nothing is applied.".to_string()
                    }
                    RunState::Failed(CodexError::Cancelled) => "Stopped.".to_string(),
                    RunState::Failed(err) => format!("Failed: {err}"),
                },
            ),
        };
        let failed = matches!(
            run.map(|run| &run.state),
            Some(RunState::Failed(err)) if *err != CodexError::Cancelled
        );

        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .child(
                div()
                    .text_size(theme.ui_text(12.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("Codex · {title}")),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .text_size(theme.ui_text(12.0))
                    .text_color(if failed {
                        theme.colors.status.danger.foreground
                    } else {
                        theme.colors.foreground.secondary
                    })
                    .child(status),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(theme.ui_text(11.0))
                    .text_color(theme.colors.foreground.secondary)
                    .child("y copy · u review · e edit · a ask · x close"),
            );

        let body = div().px_2().flex_1().min_h(px(0.0)).child(
            components::ScrollContainer::vertical(
                "codex_result_scroll_surface",
                "codex_result_scrollbar",
                panel.result_scroll.clone(),
                scale.px(200.0),
            )
            .render(theme, panel.result_input.clone()),
        );

        let element = div()
            .id("codex_panel")
            .debug_selector(|| "codex_panel".to_string())
            .relative()
            .track_focus(&panel.focus_handle)
            .flex()
            .flex_col()
            .h(scale.px(300.0))
            .border_t_1()
            .border_color(theme.colors.stroke.default)
            .bg(theme.colors.surface.panel)
            .child(header)
            .child(body)
            .child(div().px_2().py_1().child(panel.ask_input.clone()))
            .child(
                div()
                    .px_2()
                    .pb_1()
                    .text_size(theme.ui_text(11.0))
                    .text_color(theme.colors.foreground.secondary)
                    .child("Codex reads what GitComet gives it and can't run commands. Nothing is posted or committed."),
            )
            .when(focused, |d| d.child(super::panel_focus::panel_focus_ring(theme)));
        Some(element.into_any_element())
    }

    pub(super) fn handle_codex_menu_key(
        &mut self,
        keystroke: &gpui::Keystroke,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if !self.codex_menu_open {
            return false;
        }
        // Something else took over (a chord opened the palette or a dialog):
        // the menu steps aside instead of swallowing that surface's typing.
        if !self.panel_keys_active(window, cx) && !self.codex_panel_focused(window) {
            self.codex_menu_open = false;
            cx.notify();
            return false;
        }
        let mods = keystroke.modifiers;
        if mods.control || mods.alt || mods.platform || mods.function {
            return false;
        }
        let key = keystroke.key.as_str();
        if key == "escape" || key == "i" {
            self.codex_menu_open = false;
            cx.notify();
            return true;
        }
        if let Some(&(action, ..)) = CodexAction::MENU.iter().find(|(_, k, _)| *k == key) {
            self.codex_menu_open = false;
            // In review mode, reviewing the pull request means suggestions on
            // its lines rather than a text review.
            let destination = match (action, self.active_review()) {
                (CodexAction::ReviewPullRequest, Some(review)) => {
                    CodexDestination::ReviewSuggestions(
                        review.repo_id,
                        review.number,
                        review.suggestion_generation,
                    )
                }
                _ => CodexDestination::Panel,
            };
            self.start_codex(action, destination, window, cx);
        }
        // The menu is modal: other plain keys go nowhere.
        true
    }

    pub(super) fn render_codex_menu(&self, cx: &mut gpui::Context<Self>) -> AnyElement {
        let theme = self.theme;
        let scale = crate::ui_scale::UiScale::current(cx);
        let rows = CodexAction::MENU.iter().map(|&(_, key, label)| {
            div()
                .flex()
                .items_center()
                .gap(scale.px(12.0))
                .py(scale.px(3.0))
                .child(
                    div()
                        .w(scale.px(32.0))
                        .flex_shrink_0()
                        .child(components::shortcut_keys(key, theme, scale)),
                )
                .child(
                    div()
                        .text_size(scale.ui_text(13.0))
                        .text_color(theme.colors.foreground.primary)
                        .child(label),
                )
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
                    .child("Codex"),
            )
            .children(rows)
            .child(
                div()
                    .pt(scale.px(6.0))
                    .text_size(scale.ui_text(12.0))
                    .text_color(theme.colors.foreground.secondary)
                    .child("Suggestions only: nothing is posted, committed or pushed. esc closes."),
            );
        let scrim = components::modal_scrim(theme)
            .id("codex_menu_scrim")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                    this.codex_menu_open = false;
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
                    .child(
                        div()
                            .debug_selector(|| "codex_menu".to_string())
                            .w(scale.px(360.0))
                            .child(body),
                    ),
            )
            .into_any_element()
    }
}

fn repo_id_is_active(view: &GitCometView, repo_id: RepoId) -> bool {
    view.active_repo_id() == Some(repo_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_keys_are_unique_and_titles_resolve() {
        let mut keys: Vec<&str> = CodexAction::MENU.iter().map(|(_, key, _)| *key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), CodexAction::MENU.len());
        assert_eq!(CodexAction::ExplainDiff.title(), "Explain current diff");
    }

    #[test]
    fn only_hex_ids_reach_git() {
        assert!(is_object_id("3f9c21a"));
        assert!(!is_object_id("--output=x"));
        assert!(!is_object_id("HEAD"));
    }
}
