//! Lazygit-style keyboard focus across the four main panels.
//!
//! `1`–`4` pick a panel, `h`/`l` (or Left/Right) step between the ones on
//! screen, `j`/`k` (or Down/Up) move within the focused one, and `enter` opens
//! the selection. Every one of these is inert unless a panel itself holds
//! focus: text inputs, the terminal, menus, popovers, pickers, dialogs and the
//! conflict resolver keep their keys.

use super::branch_sidebar::BranchMenuTarget;
use super::panels::{branch_action_reference, can_amend};
use super::*;
use crate::github::{ChecksSummary, ReviewDecision};
use crate::view::pr_symbols;
use gitcomet_core::domain::DiffArea;
use gitcomet_state::model::SidebarMode;

/// The panels plain keys address, left to right.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FocusPanel {
    Sidebar,
    History,
    Diff,
    Details,
}

impl FocusPanel {
    const ORDER: [Self; 4] = [Self::Sidebar, Self::History, Self::Diff, Self::Details];

    /// `2` names the middle area; which of History and the diff that is
    /// depends on what's open, so the caller settles it.
    fn from_digit_key(key: &str) -> Option<Self> {
        match key {
            "1" => Some(Self::Sidebar),
            "2" => Some(Self::History),
            "3" => Some(Self::Details),
            // Details' number before the middle area became one: old habits
            // still land.
            "4" => Some(Self::Details),
            _ => None,
        }
    }

    /// The nearest panel in `direction` that `available` accepts, or `None`
    /// at the edge: focus never wraps.
    fn step(self, direction: i8, available: impl Fn(Self) -> bool) -> Option<Self> {
        let ix = Self::ORDER
            .iter()
            .position(|panel| *panel == self)
            .expect("ORDER lists every panel");
        if direction < 0 {
            Self::ORDER[..ix]
                .iter()
                .rev()
                .copied()
                .find(|panel| available(*panel))
        } else {
            Self::ORDER[ix + 1..]
                .iter()
                .copied()
                .find(|panel| available(*panel))
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Sidebar => "Sidebar",
            Self::History => "History",
            Self::Diff => "Diff",
            Self::Details => "Details",
        }
    }

    /// The few keys the status bar suggests while this panel has focus.
    pub(super) fn status_hints(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Sidebar => &[
                ("j/k", "branch"),
                ("space", "checkout"),
                ("n", "new"),
                ("o", "PR"),
                ("m", "menu"),
            ],
            Self::History => &[
                ("j/k", "commit"),
                ("enter", "files"),
                ("C", "pick"),
                ("t", "revert"),
                ("m", "menu"),
            ],
            Self::Diff => &[
                ("j/k", "change"),
                ("space", "stage"),
                ("F1/F4", "file"),
                ("esc", "back"),
            ],
            Self::Details => &[
                ("j/k", "file"),
                ("space", "stage"),
                ("a", "all"),
                ("d", "discard"),
                ("c", "commit"),
            ],
        }
    }

    /// This panel's own keys, as the `?` list shows them.
    fn help(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Sidebar => &[
                ("j / k", "Next / previous branch, revealed in History"),
                ("enter", "Go to History"),
                ("space", "Check out the branch"),
                ("n", "New branch from it"),
                ("D", "Delete the branch"),
                ("M", "Merge it into the current branch"),
                ("R", "Rebase the current branch onto it"),
                ("o", "Open a pull request for it on GitHub"),
                ("O", "New pull request from it: base, title, body"),
                ("m", "The branch's menu"),
                ("[ / ]", "Branches / Files / Pull requests tab"),
            ],
            Self::History => &[
                ("j / k", "Next / previous commit"),
                ("enter", "Go to the commit's files in Details"),
                ("C", "Cherry-pick the commit"),
                ("t", "Revert it"),
                ("g", "Reset to it (mixed; soft or hard in the menu)"),
                ("T", "Tag it"),
                ("m", "The commit's menu"),
                ("esc", "Back to your changes in Details"),
            ],
            Self::Diff => &[
                ("j / k", "Next / previous change"),
                ("space", "Stage / unstage the file, then the next"),
                ("F1 / F4", "Previous / next file"),
                ("d", "Discard the file's changes"),
                ("m", "The file's menu"),
                ("esc", "Close the diff and go back"),
            ],
            Self::Details => &[
                ("j / k", "Next / previous file, opening its diff"),
                ("enter", "Go to the file's diff"),
                ("space", "Stage / unstage the open file, then the next"),
                ("a", "Stage everything, or unstage it all"),
                ("d", "Discard the open file's changes"),
                ("m", "The open file's menu"),
                ("esc", "From a commit's files back to your changes"),
            ],
        }
    }
}

/// Status bar hints shown after the focused panel's own.
pub(super) const PANEL_STATUS_HINTS: &[(&str, &str)] = &[
    ("1-3", "panels"),
    ("p/P", "pull/push"),
    ("i", "codex"),
    ("?", "keys"),
];

/// Keys that work the same in every panel, as the `?` list shows them.
const PANEL_KEYS_HELP: &[(&str, &str)] = &[
    (
        "1 2 3",
        "Sidebar; the middle (History, or the diff when one is open); Details. Opens a collapsed one",
    ),
    ("h / l", "Previous / next panel"),
    ("c", "Write the commit message"),
    ("A", "Toggle amending the last commit"),
    ("p / P", "Pull / push"),
    ("f", "Fetch all remotes"),
    ("s", "Stash the changes"),
    ("N", "New pull request from the current branch"),
    ("0", "Codex panel"),
    ("i", "Codex actions"),
    ("?", "This list"),
];

fn is_keys_help_key(keystroke: &gpui::Keystroke) -> bool {
    keystroke.key == "?" || (keystroke.key == "/" && keystroke.modifiers.shift)
}

/// The outline drawn over the focused panel. Layered on top rather than taking
/// layout space, so focus never shifts a panel's content.
pub(super) fn panel_focus_ring(theme: AppTheme) -> gpui::Div {
    div()
        .absolute()
        .inset_0()
        .border_2()
        .border_color(theme.colors.interaction.focus_ring)
}

impl GitCometView {
    pub(super) fn diff_is_open(&self) -> bool {
        if self.pull_request_content_active() {
            return false;
        }
        self.active_repo()
            .is_some_and(|repo| repo.diff_state.diff_target.is_some())
    }

    /// History and Diff take turns in the main area, so exactly one of them is
    /// ever available.
    pub(super) fn panel_available(&self, panel: FocusPanel) -> bool {
        match panel {
            FocusPanel::Sidebar => !self.sidebar_collapsed,
            FocusPanel::History => !self.diff_is_open(),
            FocusPanel::Diff => self.diff_is_open(),
            FocusPanel::Details => !self.details_collapsed,
        }
    }

    /// The panel whose own focus handle holds focus. Something focused inside a
    /// panel (a filter box, the commit message, the diff search) is not the
    /// panel, so it maps to `None` and keeps its keys.
    pub(super) fn focused_panel(&self, window: &Window, cx: &App) -> Option<FocusPanel> {
        let main = self.main_pane.read(cx);
        if self
            .sidebar_pane
            .read(cx)
            .panel_focus_handle
            .is_focused(window)
        {
            Some(FocusPanel::Sidebar)
        } else if main
            .history_view
            .read(cx)
            .history_panel_focus_handle
            .is_focused(window)
        {
            Some(FocusPanel::History)
        } else if main.diff_panel_focus_handle.is_focused(window) {
            Some(FocusPanel::Diff)
        } else if self.details_pane.read(cx).owns_panel_focus(window) {
            Some(FocusPanel::Details)
        } else {
            None
        }
    }

    pub(super) fn panel_keys_active(&self, window: &Window, cx: &App) -> bool {
        if !renders_full_chrome(self.view_mode)
            || self.active_repo_id().is_none()
            || self.command_palette_open
            || self.reveal_commit_open
            || self.is_overlay_open(cx)
        {
            return false;
        }
        match self.focused_panel(window, cx) {
            // The conflict resolver claims plain letters for its own picks.
            Some(FocusPanel::Diff) => !self.main_pane.read(cx).is_conflict_resolver_active(),
            Some(_) => true,
            // The Codex panel counts as a place panel keys work from.
            None => window.focused(cx).is_none() || self.codex_panel_focused(window),
        }
    }

    /// The panel whose keys are live right now, for the status bar hints.
    pub(super) fn key_hint_panel(&self, window: &Window, cx: &App) -> Option<FocusPanel> {
        if !self.panel_keys_active(window, cx) {
            return None;
        }
        self.focused_panel(window, cx)
    }

    pub(super) fn default_panel(&self) -> FocusPanel {
        if self.diff_is_open() {
            FocusPanel::Diff
        } else {
            FocusPanel::History
        }
    }

    /// Runs `action` on `pane` once the current root update ends. Pane code
    /// reaches back into the root view (`root_view.update`), which panics while
    /// the root is mid-update, as it is inside a key handler.
    pub(super) fn defer_pane_action<V: 'static>(
        &self,
        pane: Entity<V>,
        cx: &mut gpui::Context<Self>,
        action: impl FnOnce(&mut V, &mut Window, &mut gpui::Context<V>) -> bool + 'static,
    ) {
        let window_handle = self.window_handle;
        cx.defer(move |cx| {
            let _ = window_handle.update(cx, |_, window, cx| {
                pane.update(cx, |pane, cx| {
                    if action(pane, window, cx) {
                        cx.notify();
                        window.refresh();
                    }
                });
            });
        });
    }

    fn focus_diff_panel_handle(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        let handle = self.main_pane.read(cx).diff_panel_focus_handle.clone();
        window.focus(&handle, cx);
    }

    /// Focuses `panel`, opening it first when collapsed. `2` over an open diff
    /// closes it, since History and the diff share the main area.
    pub(super) fn focus_panel(
        &mut self,
        panel: FocusPanel,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if panel != FocusPanel::Diff {
            // The user moved on before a pending diff arrived.
            self.focus_diff_when_open = false;
        }
        self.focus_commit_requested = None;
        match panel {
            FocusPanel::Sidebar => {
                if self.sidebar_collapsed {
                    self.set_sidebar_collapsed(false, cx);
                }
                let handle = self.sidebar_pane.read(cx).panel_focus_handle.clone();
                window.focus(&handle, cx);
            }
            FocusPanel::History => {
                if self.diff_is_open()
                    && let Some(repo_id) = self.active_repo_id()
                {
                    self.main_pane.update(cx, |pane, cx| {
                        pane.clear_diff_selection_or_exit(repo_id, cx);
                    });
                }
                let handle = self
                    .main_pane
                    .read(cx)
                    .history_view
                    .read(cx)
                    .history_panel_focus_handle
                    .clone();
                window.focus(&handle, cx);
            }
            FocusPanel::Diff => {
                if !self.diff_is_open() {
                    return;
                }
                if let Some(from) = self
                    .focused_panel(window, cx)
                    .filter(|from| *from != FocusPanel::Diff)
                {
                    self.diff_return_panel = from;
                }
                self.focus_diff_panel_handle(window, cx);
            }
            FocusPanel::Details => {
                if self.details_collapsed {
                    self.set_details_collapsed(false, cx);
                }
                let handle = self.details_pane.read(cx).panel_focus_handle.clone();
                window.focus(&handle, cx);
            }
        }
        cx.notify();
    }

    /// Where focus goes when the diff it sat on closes (esc, `2`, a click
    /// elsewhere): back to the panel the diff was entered from.
    pub(super) fn diff_return_target(&self) -> FocusPanel {
        match self.diff_return_panel {
            FocusPanel::Sidebar if !self.sidebar_collapsed => FocusPanel::Sidebar,
            FocusPanel::Details if !self.details_collapsed => FocusPanel::Details,
            _ => FocusPanel::History,
        }
    }

    fn move_in_panel(&mut self, panel: FocusPanel, direction: i8, cx: &mut gpui::Context<Self>) {
        match panel {
            FocusPanel::Sidebar => {
                self.defer_pane_action(self.sidebar_pane.clone(), cx, move |pane, _, cx| {
                    pane.select_adjacent_branch(direction, cx)
                });
            }
            FocusPanel::History => {
                let history = self.main_pane.read(cx).history_view.clone();
                self.defer_pane_action(history, cx, move |history, _, cx| {
                    history.history_select_adjacent_commit(direction, cx)
                });
            }
            FocusPanel::Diff => {
                self.defer_pane_action(self.main_pane.clone(), cx, move |pane, _, cx| {
                    if direction < 0 {
                        pane.navigate_prev_diff_change(cx)
                    } else {
                        pane.navigate_next_diff_change(cx)
                    }
                });
            }
            FocusPanel::Details => {
                self.defer_pane_action(self.main_pane.clone(), cx, move |pane, window, cx| {
                    let Some(repo_id) = pane.active_repo_id() else {
                        return false;
                    };
                    // The pane's own snapshot, which is newer than a decision
                    // taken before the deferral.
                    if pane.diff_is_open_for_navigation() {
                        pane.try_select_adjacent_diff_file_preserving_focus(
                            repo_id, direction, window, cx,
                        )
                    } else {
                        pane.try_select_first_diff_file(repo_id, cx)
                    }
                });
            }
        }
    }

    fn open_in_panel(
        &mut self,
        panel: FocusPanel,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        match panel {
            // The branch is already revealed in History; enter moves there.
            FocusPanel::Sidebar => self.focus_panel(FocusPanel::History, window, cx),
            // A commit's files live in Details; enter on one opens its diff.
            FocusPanel::History => self.focus_panel(FocusPanel::Details, window, cx),
            FocusPanel::Diff => return false,
            FocusPanel::Details => {
                if self.diff_is_open() {
                    self.focus_panel(FocusPanel::Diff, window, cx);
                } else {
                    // The diff arrives with a later store snapshot; focus it as
                    // soon as the file is picked so it is ready when it renders.
                    self.diff_return_panel = FocusPanel::Details;
                    self.defer_pane_action(self.main_pane.clone(), cx, |pane, window, cx| {
                        let Some(repo_id) = pane.active_repo_id() else {
                            return false;
                        };
                        let opened = pane.try_select_first_diff_file(repo_id, cx);
                        if opened {
                            window.focus(&pane.diff_panel_focus_handle, cx);
                        }
                        opened
                    });
                }
            }
        }
        true
    }

    /// The hint bar's keys for `panel`, which differ on the Pull requests tab.
    pub(super) fn key_hints(&self, panel: FocusPanel) -> &'static [(&'static str, &'static str)] {
        if matches!(panel, FocusPanel::Diff | FocusPanel::History)
            && self
                .active_review()
                .is_some_and(super::review::ReviewMode::historical_range)
        {
            return &[
                ("j/k", "line"),
                ("shift+j/k", "select"),
                ("}/{", "change"),
                ("]/[", "file"),
                ("space", "viewed"),
                ("/", "filter"),
                ("V", "show viewed"),
                ("G", "show generated"),
                ("alt+p", "preview"),
                ("alt+v", "image mode"),
                (",/.", "divider/opacity"),
                ("L", "changed since"),
                ("C", "commits"),
                ("S", "submit"),
                ("q", "leave"),
            ];
        }
        if self.active_review().is_some() {
            return match panel {
                FocusPanel::Sidebar => &[
                    ("j/k", "file"),
                    ("space", "viewed"),
                    ("/", "filter"),
                    ("V", "show viewed"),
                    ("G", "show generated"),
                    ("L", "changed since"),
                    ("C", "commits"),
                    ("enter", "diff"),
                    ("S", "submit"),
                    ("q", "leave"),
                ],
                FocusPanel::Diff | FocusPanel::History => &[
                    ("j/k", "line"),
                    ("shift+j/k", "select"),
                    ("c", "comment"),
                    ("t/T", "thread"),
                    ("r", "reply"),
                    ("a/x", "suggestion"),
                    ("}/{", "change"),
                    ("]/[", "file"),
                    ("space", "viewed"),
                    ("alt+p", "preview"),
                    ("alt+v", "image mode"),
                    (",/.", "divider/opacity"),
                    ("L", "changed since"),
                    ("C", "commits"),
                    ("S", "submit"),
                ],
                FocusPanel::Details => &[
                    ("j/k", "comment"),
                    ("enter", "go to"),
                    ("e", "edit"),
                    ("d d", "delete"),
                    ("r", "reply outdated"),
                    ("C", "commits"),
                    ("S", "submit"),
                ],
            };
        }
        if panel == FocusPanel::Sidebar && self.state.sidebar_mode == SidebarMode::Files {
            return &[("[ ]", "tab")];
        }
        if self.state.sidebar_mode == SidebarMode::PullRequests {
            match panel {
                FocusPanel::Sidebar => {
                    return &[
                        ("j/k", "PR"),
                        ("</>", "stack"),
                        ("enter", "read"),
                        ("space", "checkout"),
                        ("r", "review"),
                        ("M", "merge"),
                        ("o", "GitHub"),
                    ];
                }
                FocusPanel::History if self.pull_request_content_active() => {
                    return &[
                        ("j/k", "entry"),
                        ("</>", "stack"),
                        ("[/]", "tab"),
                        ("enter", "thread"),
                        ("V", "resolved"),
                        ("o", "GitHub"),
                        ("r", "review"),
                    ];
                }
                FocusPanel::Details if self.pull_request_details_active() => {
                    return &[
                        ("j/k", "commit"),
                        ("J/K", "range"),
                        ("</>", "stack"),
                        ("esc", "all"),
                        ("enter", "read"),
                        ("space", "checkout"),
                        ("r", "review"),
                        ("M", "merge"),
                    ];
                }
                _ => {}
            }
        }
        if self.changes_list_shown() {
            match panel {
                FocusPanel::Details => {
                    return &[
                        ("j/k", "file"),
                        ("J/K", "select"),
                        ("space", "stage"),
                        ("a", "all"),
                        ("/", "filter"),
                    ];
                }
                FocusPanel::Diff => {
                    return &[
                        ("j/k", "change"),
                        ("space", "stage"),
                        ("F1/F4", "file"),
                        ("esc", "back"),
                    ];
                }
                _ => {}
            }
        }
        panel.status_hints()
    }

    pub(super) fn key_help(&self, panel: FocusPanel) -> &'static [(&'static str, &'static str)] {
        if matches!(panel, FocusPanel::Diff | FocusPanel::History)
            && self
                .active_review()
                .is_some_and(super::review::ReviewMode::historical_range)
        {
            return &[
                ("j / k", "Line cursor down / up"),
                ("shift+j / k", "Select lines from the cursor"),
                ("} / {", "Next / previous change"),
                ("] / [", "Next / previous file"),
                ("space", "Mark viewed"),
                (
                    "/",
                    "Filter the file list: fuzzy words, .rs for a type; esc clears",
                ),
                ("V", "Show / hide viewed files (hidden by default)"),
                (
                    "Shift+G",
                    "Show / hide generated files (hidden by default; don't count toward viewed progress)",
                ),
                (
                    "Alt+P",
                    "Toggle the rendered Preview / Text switch, where it shows",
                ),
                ("Alt+V", "Cycle an image diff's view mode"),
                (",/.", "Move the Swipe divider or the Onion skin opacity"),
                ("L", "Changes since your last review"),
                ("C", "Pick a commit range"),
                ("S", "Submit the review"),
                ("q", "Leave review mode"),
            ];
        }
        if self.active_review().is_some() {
            return match panel {
                FocusPanel::Sidebar => &[
                    ("j / k", "Next / previous file"),
                    (
                        "space",
                        "Mark viewed (on GitHub too), then the next unviewed file",
                    ),
                    (
                        "L",
                        "Changes since your last review (files and diff) / the whole PR",
                    ),
                    (
                        "/",
                        "Filter the file list: fuzzy words, .rs for a type; esc clears",
                    ),
                    ("V", "Show / hide viewed files (hidden by default)"),
                    (
                        "Shift+G",
                        "Show / hide generated files (hidden by default; don't count toward viewed progress)",
                    ),
                    (
                        "C",
                        "Pick all changes, since last review, or a commit range",
                    ),
                    ("R", "Retry the files that failed to list"),
                    ("enter", "Go to the diff"),
                    ("S", "Submit the review"),
                    ("q", "Leave review mode; pending comments stay"),
                ],
                FocusPanel::Diff | FocusPanel::History => &[
                    ("j / k", "Line cursor down / up; by rendered block in Preview"),
                    ("shift+j / k", "Select lines from the cursor"),
                    (
                        "c",
                        "Comment on the line or selection (alt+s suggests a change); in Preview, switches to Text at the block's first line",
                    ),
                    (
                        "t / T",
                        "Next / previous thread or Codex suggestion; switch to Text first in Preview",
                    ),
                    (
                        "r",
                        "Reply to the thread on this line; switch to Text first in Preview",
                    ),
                    (
                        "a / x",
                        "Adopt / drop the Codex suggestion on this line; switch to Text first in Preview",
                    ),
                    (
                        "} / {",
                        "Next / previous change; by rendered block in Preview",
                    ),
                    ("] / [", "Next / previous file"),
                    (
                        "space",
                        "Mark viewed (on GitHub too), then the next unviewed file",
                    ),
                    (
                        "Alt+P",
                        "Toggle the rendered Preview / Text switch, where it shows",
                    ),
                    (
                        "Alt+V",
                        "Cycle Side by side / Swipe / Onion skin, where an image diff shows",
                    ),
                    (
                        ", / .",
                        "Move the Swipe divider or the Onion skin opacity by 10%",
                    ),
                    (
                        "L",
                        "Changes since your last review (files and diff) / the whole PR",
                    ),
                    (
                        "/",
                        "Filter the file list: fuzzy words, .rs for a type; esc clears",
                    ),
                    ("V", "Show / hide viewed files (hidden by default)"),
                    (
                        "Shift+G",
                        "Show / hide generated files (hidden by default; don't count toward viewed progress)",
                    ),
                    (
                        "C",
                        "Pick all changes, since last review, or a commit range",
                    ),
                    ("esc", "Drop the selection"),
                    ("S", "Submit the review"),
                    ("q", "Leave review mode; pending comments stay"),
                ],
                FocusPanel::Details => &[
                    (
                        "j / k",
                        "Next / previous pending comment or outdated conversation",
                    ),
                    ("enter", "Go to its line"),
                    ("e", "Edit it"),
                    ("d d", "Delete it"),
                    ("r", "Reply to the picked outdated conversation"),
                    (
                        "L",
                        "Changes since your last review (files and diff) / the whole PR",
                    ),
                    ("V", "Show / hide viewed files (hidden by default)"),
                    (
                        "Shift+G",
                        "Show / hide generated files (hidden by default; don't count toward viewed progress)",
                    ),
                    (
                        "C",
                        "Pick all changes, since last review, or a commit range",
                    ),
                    ("S", "Submit the review"),
                    ("q", "Leave review mode; pending comments stay"),
                ],
            };
        }
        if panel == FocusPanel::Sidebar && self.state.sidebar_mode == SidebarMode::Files {
            return &[("[ / ]", "Branches / Files / Pull requests tab")];
        }
        if self.state.sidebar_mode == SidebarMode::PullRequests {
            match panel {
                FocusPanel::Sidebar => {
                    return &[
                        ("j / k", "Next / previous pull request"),
                        ("< / >", "Pull request below / above it in its stack"),
                        ("enter", "Read the pull request in the middle panel"),
                        ("space", "Check it out locally"),
                        ("n", "New pull request"),
                        ("r", "Review it: line comments, one submit"),
                        ("S", "Quick review: just a verdict and summary"),
                        ("M", "Merge it on GitHub"),
                        ("o", "Open on GitHub"),
                        ("R", "Refresh the list; retry files that failed to list"),
                        ("[ / ]", "Branches / Files / Pull requests tab"),
                    ];
                }
                FocusPanel::History if self.pull_request_content_active() => {
                    return &[
                        ("j / k", "Next / previous entry or thread"),
                        ("< / >", "Pull request below / above it in its stack"),
                        ("[ / ]", "Conversation / Comments tab"),
                        ("enter", "Review at the selected thread's line"),
                        ("V", "Show / hide resolved and outdated threads"),
                        ("o", "Open on GitHub"),
                        ("r", "Review the pull request"),
                    ];
                }
                FocusPanel::Details if self.pull_request_details_active() => {
                    return &[
                        ("j / k", "Next / previous PR commit"),
                        ("J / K", "Extend the contiguous commit range"),
                        ("< / >", "Pull request below / above it in its stack"),
                        ("esc", "Select all commits"),
                        ("enter", "Read the pull request in the middle panel"),
                        ("space", "Check it out locally"),
                        ("r", "Review"),
                        ("M", "Merge it on GitHub"),
                        ("o", "Open on GitHub"),
                    ];
                }
                _ => {}
            }
        }
        if self.changes_list_shown() {
            match panel {
                FocusPanel::Details => {
                    return &[
                        ("j / k", "Next / previous file, opening its diff"),
                        ("enter", "Go to the file's diff"),
                        (
                            "J / K",
                            "Select a range of files from the open one; shift-click too",
                        ),
                        (
                            "space",
                            "Stage / unstage the open file, or the selected range; they keep their place",
                        ),
                        ("a", "Stage what the list shows, or unstage it all"),
                        ("shift+space", "Stage / unstage the open file's folder"),
                        (
                            "/",
                            "Filter by path, fuzzy; .rs keeps a file type. Enter keeps it",
                        ),
                        ("esc", "Drop the selection, then the / filter"),
                        ("F", "Show all, unstaged, staged or untracked"),
                        ("`", "Tree or flat list"),
                        ("o", "Sort the list"),
                        ("d", "Discard the open file's changes, or the selection's"),
                        ("m", "The open file's menu"),
                    ];
                }
                FocusPanel::Diff => {
                    return &[
                        ("j / k", "Next / previous change"),
                        ("space", "Stage / unstage the file; it keeps its place"),
                        ("F1 / F4", "Previous / next file"),
                        ("d", "Discard the file's changes"),
                        ("m", "The file's menu"),
                        ("esc", "Close the diff and go back"),
                    ];
                }
                _ => {}
            }
        }
        panel.help()
    }

    /// With a range selected in the Changes list, hands its files with
    /// worktree changes to the discard dialog, which reads them from the
    /// status selection. `false` without a range.
    fn discard_changes_selection(&mut self, repo_id: RepoId, cx: &mut gpui::Context<Self>) -> bool {
        let Some(selection) = self.details_pane.read(cx).changes_selection(repo_id) else {
            return false;
        };
        let paths: Vec<std::path::PathBuf> = selection
            .into_iter()
            .filter(|(_, lanes)| lanes.unstaged.is_some() && !lanes.conflicted())
            .map(|(path, _)| path)
            .collect();
        if paths.is_empty() {
            return false;
        }
        self.details_pane.update(cx, |pane, _| {
            pane.status_multi_selection.insert(
                repo_id,
                StatusMultiSelection {
                    explicit_section: Some(StatusSection::CombinedUnstaged),
                    unstaged: paths,
                    ..Default::default()
                },
            );
        });
        true
    }

    /// Details shows the one Changes list rather than sections, a pull
    /// request or a commit.
    fn changes_list_shown(&self) -> bool {
        self.change_tracking_view == ChangeTrackingView::Unified
            && !self.pull_request_details_active()
            && self
                .active_repo()
                .is_some_and(|repo| repo.history_state.selected_commit.is_none())
    }

    /// Opens a dialog or menu from a key; dismissing it hands focus back to
    /// the panel it was opened from.
    pub(super) fn open_popover_from_key(
        &mut self,
        kind: PopoverKind,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let request = PopoverRequest::from(kind);
        let request = match window.focused(cx) {
            Some(focus) => request.returning_focus_to(focus),
            None => request,
        };
        self.open_popover_centered(request, window, cx);
    }

    pub(super) fn open_pull_request_prompt(
        &mut self,
        kind: PopoverKind,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        self.clear_pull_request_submit_error();
        self.open_popover_from_key(kind, window, cx);
    }

    /// Focus whose element went away (a dialog or menu that closed, a row that
    /// vanished) goes back to the panel the keyboard was last in, so panel keys
    /// keep working without a click. An explicit blur is left alone, and so is
    /// focus that just moved: an element focused ahead of its first render (a
    /// diff or file editor still loading) is on its way, not gone.
    pub(super) fn restore_panel_focus(
        &mut self,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let focused = window.focused(cx);
        if focused.is_none()
            || focused != self.focus_prev_render
            || !renders_full_chrome(self.view_mode)
            || self.active_repo_id().is_none()
            || self.command_palette_open
            || self.reveal_commit_open
            || self.is_overlay_open(cx)
        {
            return;
        }
        let panel = self
            .last_focused_panel
            .filter(|panel| self.panel_available(*panel))
            .unwrap_or_else(|| self.default_panel());
        self.focus_panel(panel, window, cx);
    }

    /// Puts the keyboard in the commit message. A commit selected in History
    /// has Details showing it instead, so that selection is dropped first and
    /// the box is focused once it is back on screen.
    fn focus_commit_message(
        &mut self,
        repo_id: RepoId,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.details_collapsed {
            self.set_details_collapsed(false, cx);
        }
        self.focus_diff_when_open = false;
        if self
            .active_repo()
            .is_some_and(|repo| repo.history_state.selected_commit.is_some())
        {
            self.store.dispatch(Msg::ClearCommitSelection { repo_id });
        }
        self.focus_commit_requested = Some(std::time::Instant::now());
        cx.notify();
        window.refresh();
    }

    /// The commit message input, once Details shows the working tree again.
    pub(super) fn commit_message_focus_handle(&self, cx: &App) -> Option<FocusHandle> {
        let showing_status = !self.details_collapsed
            && self
                .active_repo()
                .is_some_and(|repo| repo.history_state.selected_commit.is_none());
        showing_status.then(|| {
            self.details_pane
                .read(cx)
                .commit_message_input
                .read(cx)
                .focus_handle()
        })
    }

    fn stage_or_unstage_all(&mut self, window: &mut Window, cx: &mut gpui::Context<Self>) {
        if self.change_tracking_view == ChangeTrackingView::Unified {
            // Deferred: staging may raise the conflict-markers dialog, which
            // needs the root this key handler is still inside.
            self.defer_pane_action(self.details_pane.clone(), cx, |pane, window, cx| {
                pane.toggle_all_changes(None, window, cx);
                true
            });
            return;
        }
        let unstaged = self
            .active_repo()
            .and_then(|repo| repo.worktree_status_entries())
            .is_some_and(|entries| !entries.is_empty());
        let command = if unstaged { "stage-all" } else { "unstage-all" };
        self.execute_command(command, Some(window), cx);
    }

    /// Whether the file menu would offer "Discard changes" for this file.
    fn can_discard(&self, area: DiffArea, path: &std::path::Path) -> bool {
        use gitcomet_core::domain::FileStatusKind::{Added, Conflicted};
        let Some(repo) = self.active_repo() else {
            return false;
        };
        let unstaged = repo.status_entry_for_path(DiffArea::Unstaged, path);
        let staged = repo.status_entry_for_path(DiffArea::Staged, path);
        if [unstaged, staged]
            .iter()
            .flatten()
            .any(|entry| entry.kind == Conflicted)
        {
            return false;
        }
        match area {
            DiffArea::Unstaged => true,
            DiffArea::Staged => {
                unstaged.is_some() || staged.is_some_and(|entry| entry.kind == Added)
            }
        }
    }

    /// The working-tree file the open diff shows.
    fn open_worktree_file(&self) -> Option<(DiffArea, std::path::PathBuf)> {
        match self.active_repo()?.diff_state.diff_target.as_ref()? {
            DiffTarget::WorkingTree { path, area } => Some((*area, path.clone())),
            _ => None,
        }
    }

    fn selected_branch_target(&self, repo_id: RepoId, cx: &App) -> Option<BranchMenuTarget> {
        self.sidebar_pane
            .read(cx)
            .selected_branch()
            .filter(|selected| selected.repo_id == repo_id)
            .map(|selected| selected.target.clone())
    }

    /// `m`: the context menu of whatever the panel has selected.
    fn open_selection_menu(
        &mut self,
        panel: FocusPanel,
        repo_id: RepoId,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let kind = match panel {
            FocusPanel::Sidebar if self.state.sidebar_mode == SidebarMode::Branches => self
                .selected_branch_target(repo_id, cx)
                .map(|target| PopoverKind::BranchMenu { repo_id, target }),
            FocusPanel::Sidebar => None,
            FocusPanel::History => self
                .active_repo()
                .and_then(|repo| repo.history_state.selected_commit.clone())
                .map(|commit_id| PopoverKind::CommitMenu { repo_id, commit_id }),
            FocusPanel::Details | FocusPanel::Diff => {
                self.open_worktree_file()
                    .map(|(area, path)| PopoverKind::StatusFileMenu {
                        repo_id,
                        area,
                        path,
                    })
            }
        };
        if let Some(kind) = kind {
            self.open_popover_from_key(kind, window, cx);
        }
    }

    /// Branch keys, mirroring the branch menu's entries and their guards.
    fn branch_action(
        &mut self,
        repo_id: RepoId,
        key: &str,
        armed: Option<(String, BranchMenuTarget)>,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(target) = self.selected_branch_target(repo_id, cx) else {
            if key == "n" {
                self.execute_command("create-branch", Some(window), cx);
            }
            return;
        };
        let repo = self.active_repo();
        let reference = branch_action_reference(repo, &target);
        let busy = repo.is_some_and(|repo| repo.history_rewrite_busy());
        let is_current = match (&target, repo.map(|repo| &repo.head_branch)) {
            (BranchMenuTarget::Local { name }, Some(Loadable::Ready(head))) => name == head,
            _ => false,
        };
        // D and M act on a second press: one stray keystroke never deletes or
        // merges.
        let deletable = matches!(target, BranchMenuTarget::Local { .. }) && !is_current;
        if (key == "D" && deletable) || (key == "M" && !is_current) {
            let press = (key.to_string(), target.clone());
            if armed.as_ref() != Some(&press) {
                let what = if key == "D" { "delete" } else { "merge" };
                self.push_toast(
                    components::ToastKind::Warning,
                    format!(
                        "Press Shift+{key} again to {what} {}.",
                        target.display_name()
                    ),
                    cx,
                );
                self.armed_branch_key = Some(press);
                return;
            }
        }
        let kind = match (key, &target) {
            ("space", BranchMenuTarget::Local { name }) => {
                if !is_current {
                    self.store.dispatch(Msg::CheckoutBranch {
                        repo_id,
                        name: name.clone(),
                    });
                }
                return;
            }
            ("space", BranchMenuTarget::Remote { .. }) => {
                target.remote_parts().map(|(remote, branch)| {
                    PopoverKind::CheckoutRemoteBranchPrompt {
                        repo_id,
                        remote: remote.to_string(),
                        branch: branch.to_string(),
                    }
                })
            }
            ("n", _) => Some(PopoverKind::CreateBranchFromRefPrompt {
                repo_id,
                target: reference,
                source_selectable: false,
                name_prefix: String::new(),
            }),
            ("D", BranchMenuTarget::Local { name }) if !is_current => {
                // Git refuses an unmerged branch; the force-delete dialog that
                // follows then opens centered, as it does from the palette.
                self.pending_force_delete_branch_centered = true;
                self.store.dispatch(Msg::DeleteBranch {
                    repo_id,
                    name: name.clone(),
                });
                return;
            }
            ("M", _) if !is_current => {
                self.store.dispatch(Msg::MergeRef { repo_id, reference });
                return;
            }
            ("R", _) if !is_current && !busy => Some(PopoverKind::RebaseOntoConfirm {
                repo_id,
                onto: reference,
            }),
            // lazygit's pull request keys: `o` opens GitHub's page for it,
            // `O` sets it up here first (base, title, body; or GitHub after all).
            ("o", _) => {
                self.open_pull_request_compare(&target, cx);
                return;
            }
            ("O", BranchMenuTarget::Local { name }) => {
                self.open_create_pull_request(repo_id, Some(name.clone()), window, cx);
                return;
            }
            _ => None,
        };
        if let Some(kind) = kind {
            self.open_popover_from_key(kind, window, cx);
        }
    }

    /// Commit keys, mirroring the commit menu's entries and their guards.
    fn commit_action(
        &mut self,
        repo_id: RepoId,
        key: &str,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(repo) = self.active_repo() else {
            return;
        };
        let Some(commit_id) = repo.history_state.selected_commit.clone() else {
            return;
        };
        let busy = repo.history_rewrite_busy();
        let is_head = repo.head_commit_id().is_some_and(|head| head == commit_id);
        let sha = commit_id.as_ref().to_string();
        let kind = match key {
            "C" if !busy && !is_head => PopoverKind::CherryPickCommitConfirm { repo_id, commit_id },
            "t" if !busy => PopoverKind::RevertCommitConfirm { repo_id, commit_id },
            "g" => PopoverKind::ResetPrompt {
                repo_id,
                target: sha,
                mode: ResetMode::Mixed,
            },
            "T" => PopoverKind::CreateTagPrompt {
                repo_id,
                target: sha,
            },
            _ => return,
        };
        self.open_popover_from_key(kind, window, cx);
    }

    /// lazygit-style action keys. `None` leaves the key to panel navigation
    /// and, past that, to the diff's own keys, which is where `space` stages
    /// the open file from Details as well as from the diff.
    fn handle_action_key(
        &mut self,
        current: Option<FocusPanel>,
        key: &str,
        shift: bool,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Option<bool> {
        // Any other key disarms a pending Shift+D / Shift+M.
        let armed = self.armed_branch_key.take();
        let repo_id = self.active_repo_id()?;
        // The conflict resolver's a–d picks (shifted or not) work from Details
        // too.
        if matches!(key, "a" | "b" | "c" | "d")
            && self.main_pane.read(cx).is_conflict_resolver_active()
        {
            return None;
        }
        let key = if shift {
            key.to_ascii_uppercase()
        } else {
            key.to_string()
        };
        // Details shows a pull request instead of the working tree...
        let status_shown = !self.pull_request_details_active();
        // ...or the commit selected in History.
        let worktree_shown = status_shown
            && self
                .active_repo()
                .is_some_and(|repo| repo.history_state.selected_commit.is_none());
        let branches = self.state.sidebar_mode == SidebarMode::Branches;
        match (current, key.as_str()) {
            (_, "p") => self.execute_command("pull", Some(window), cx),
            (_, "P") => self.execute_command("push", Some(window), cx),
            (_, "f") => self.execute_command("fetch-all", Some(window), cx),
            (_, "s") => self.open_popover_from_key(PopoverKind::StashPrompt, window, cx),
            // From anywhere: the current branch's pull request, set up here.
            (_, "N") => self.open_create_pull_request(repo_id, None, window, cx),
            (_, "c") if status_shown => self.focus_commit_message(repo_id, window, cx),
            (_, "A") if status_shown => {
                // Like the menu: turning amend off is always allowed.
                let enabled = self.details_pane.read(cx).commit_amend_enabled;
                if enabled || can_amend(self.active_repo()) {
                    self.set_commit_amend_enabled(!enabled, cx);
                }
                self.focus_commit_message(repo_id, window, cx);
            }
            (Some(panel), "m") => self.open_selection_menu(panel, repo_id, window, cx),
            (Some(FocusPanel::Details), "a") if worktree_shown => {
                self.stage_or_unstage_all(window, cx)
            }
            // From any panel, or none: the Changes list is the one list
            // it filters, and it's on screen whenever this is true.
            (_, "/") if self.changes_list_shown() => {
                if self.details_collapsed {
                    self.set_details_collapsed(false, cx);
                }
                self.details_pane
                    .update(cx, |pane, cx| pane.open_changes_query(cx));
            }
            // Esc peels one layer: the range selection, then the filter.
            (Some(FocusPanel::Details), "escape")
                if self.changes_list_shown()
                    && self
                        .details_pane
                        .read(cx)
                        .changes_selection(repo_id)
                        .is_some() =>
            {
                self.details_pane
                    .update(cx, |pane, cx| pane.clear_changes_range(cx));
            }
            (Some(FocusPanel::Details), "escape")
                if worktree_shown
                    && self.change_tracking_view == ChangeTrackingView::Unified
                    && self.details_pane.read(cx).changes_query_active() =>
            {
                self.details_pane
                    .update(cx, |pane, cx| pane.clear_changes_query(window, cx));
            }
            // Out of a commit's details and back to your changes.
            (Some(FocusPanel::History | FocusPanel::Details), "escape")
                if status_shown
                    && self
                        .active_repo()
                        .is_some_and(|repo| repo.history_state.selected_commit.is_some()) =>
            {
                // A file of the commit may be open in the diff: it goes too,
                // or it would stay up with nothing listing it.
                if matches!(
                    self.active_repo()
                        .and_then(|repo| repo.diff_state.diff_target.as_ref()),
                    Some(DiffTarget::Commit { .. })
                ) {
                    self.store.dispatch(Msg::ClearDiffSelection { repo_id });
                }
                self.store.dispatch(Msg::ClearCommitSelection { repo_id });
            }
            (Some(FocusPanel::Details), "J" | "K" | "DOWN" | "UP") if self.changes_list_shown() => {
                let direction = if matches!(key.as_str(), "J" | "DOWN") {
                    1
                } else {
                    -1
                };
                self.defer_pane_action(self.main_pane.clone(), cx, move |pane, window, cx| {
                    pane.extend_changes_selection(repo_id, direction, window, cx)
                });
            }
            (Some(FocusPanel::Details), "SPACE")
                if worktree_shown && self.change_tracking_view == ChangeTrackingView::Unified =>
            {
                // Deferred: staging may raise the conflict-markers dialog.
                self.defer_pane_action(self.details_pane.clone(), cx, |pane, window, cx| {
                    pane.toggle_open_change_folder(window, cx);
                    true
                });
            }
            (Some(FocusPanel::Details), "F" | "`" | "o")
                if worktree_shown && self.change_tracking_view == ChangeTrackingView::Unified =>
            {
                let list = crate::view::rows::FileListId::Changes;
                match key.as_str() {
                    "F" => self
                        .details_pane
                        .update(cx, |pane, cx| pane.cycle_changes_filter(cx)),
                    "`" => self.details_pane.update(cx, |pane, cx| {
                        pane.toggle_file_list_layout(repo_id, list, cx)
                    }),
                    _ => self.open_popover_from_key(
                        PopoverKind::CommitFileSortMenu { list },
                        window,
                        cx,
                    ),
                }
            }
            // With a range selected, `d` is about the range, never quietly
            // about just the open file.
            (Some(FocusPanel::Details | FocusPanel::Diff), "d")
                if self.changes_list_shown()
                    && self
                        .details_pane
                        .read(cx)
                        .changes_selection(repo_id)
                        .is_some() =>
            {
                if self.discard_changes_selection(repo_id, cx) {
                    self.open_popover_from_key(
                        PopoverKind::DiscardChangesConfirm {
                            repo_id,
                            area: DiffArea::Unstaged,
                            path: None,
                        },
                        window,
                        cx,
                    );
                } else {
                    self.push_toast(
                        components::ToastKind::Warning,
                        "Nothing selected has working-tree changes to discard.".to_string(),
                        cx,
                    );
                }
            }
            (Some(FocusPanel::Details | FocusPanel::Diff), "d") if worktree_shown => {
                if let Some((area, path)) = self
                    .open_worktree_file()
                    .filter(|(area, path)| self.can_discard(*area, path))
                {
                    self.open_popover_from_key(
                        PopoverKind::DiscardChangesConfirm {
                            repo_id,
                            area,
                            path: Some(path),
                        },
                        window,
                        cx,
                    );
                }
            }
            (Some(FocusPanel::Sidebar), "space" | "n" | "D" | "M" | "R" | "o" | "O")
                if branches =>
            {
                self.branch_action(repo_id, &key, armed, window, cx)
            }
            (Some(FocusPanel::History), "C" | "t" | "g" | "T") => {
                self.commit_action(repo_id, &key, window, cx)
            }
            _ => return None,
        }
        Some(true)
    }

    /// The Pull requests tab's keys. `None` leaves the key to the general
    /// panel handling.
    fn handle_pull_request_key(
        &mut self,
        current: Option<FocusPanel>,
        key: &str,
        shift: bool,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Option<bool> {
        if self.state.sidebar_mode != SidebarMode::PullRequests {
            return None;
        }
        let repo_id = self.active_repo_id()?;
        if (key == "r" && shift) || key == "R" {
            self.refresh_pull_requests(cx);
            return Some(true);
        }
        let selected = self.active_pull_requests().and_then(|prs| prs.selected);
        let in_details = current == Some(FocusPanel::Details) && self.pull_request_content_active();
        let in_content = current == Some(FocusPanel::History) && self.pull_request_content_active();
        let in_sidebar = current == Some(FocusPanel::Sidebar) && self.active_review().is_none();
        // `<`/`>` move to the pull request below/above the selected one in
        // its stack, in the list, panel 2 and Details. US layouts send them
        // as shift+`,`/`.`; ISO/DE layouts have `<`/`>` as their own key
        // (there, shift+`,` sends `;`), so both are matched.
        let stack_step = match key {
            "," if shift => Some(-1),
            "." if shift => Some(1),
            "<" => Some(-1),
            ">" => Some(1),
            _ => None,
        };
        if let Some(direction) = stack_step
            && (in_sidebar || in_content || in_details)
        {
            return Some(self.select_pull_request_stack_neighbor(direction, cx));
        }
        if shift {
            return match key.to_ascii_lowercase().as_str() {
                "j" | "k" if in_details => {
                    self.step_pull_request_commit(
                        if key.eq_ignore_ascii_case("j") { 1 } else { -1 },
                        true,
                        cx,
                    );
                    Some(true)
                }
                "c" | "t" if in_content => Some(true),
                "v" if in_content => {
                    self.toggle_pull_request_hidden_threads(cx);
                    Some(true)
                }
                // Submit a review with no line comments: the quick verdict.
                "s" => {
                    let number = selected?;
                    self.open_pull_request_prompt(
                        PopoverKind::PullRequestReview {
                            repo_id,
                            number,
                            kind: crate::github::ReviewKind::Comment,
                        },
                        window,
                        cx,
                    );
                    Some(true)
                }
                "m" => {
                    let number = selected?;
                    self.open_pull_request_prompt(
                        PopoverKind::MergePullRequest {
                            repo_id,
                            number,
                            method: crate::github::MergeMethod::Merge,
                        },
                        window,
                        cx,
                    );
                    Some(true)
                }
                _ => None,
            };
        }
        let direction = match key {
            "j" | "down" => 1,
            "k" | "up" => -1,
            _ => 0,
        };
        match (current, key) {
            (Some(FocusPanel::Details), _) if direction != 0 && in_details => {
                self.step_pull_request_commit(direction, false, cx);
                Some(true)
            }
            (Some(FocusPanel::Details), "escape") if in_details => {
                self.clear_pull_request_commit_selection(cx);
                Some(true)
            }
            (Some(FocusPanel::History), "t" | "g" | "m" | "escape") if in_content => Some(true),
            (Some(FocusPanel::History), _) if direction != 0 && in_content => {
                self.step_pull_request_content(direction, cx);
                Some(true)
            }
            (Some(FocusPanel::History), "[") if in_content => {
                self.set_pull_request_content_tab(
                    super::pull_requests::PrContentTab::Conversation,
                    cx,
                );
                Some(true)
            }
            (Some(FocusPanel::History), "]") if in_content => {
                self.set_pull_request_content_tab(super::pull_requests::PrContentTab::Comments, cx);
                Some(true)
            }
            (Some(FocusPanel::History), "enter") if in_content => {
                if self.active_pull_requests().is_some_and(|prs| {
                    prs.content_tab == super::pull_requests::PrContentTab::Comments
                }) && let Some(thread) = self.selected_pull_request_thread()
                {
                    self.start_review_at_thread(&thread, cx);
                }
                Some(true)
            }
            (Some(FocusPanel::Sidebar), _) if direction != 0 => {
                self.select_adjacent_pull_request(direction, cx);
                Some(true)
            }
            (Some(FocusPanel::Sidebar | FocusPanel::Details), "space") if selected.is_some() => {
                self.checkout_pull_request(cx);
                Some(true)
            }
            (Some(FocusPanel::Sidebar), "enter") if selected.is_some() => {
                self.focus_panel(FocusPanel::History, window, cx);
                Some(true)
            }
            (Some(FocusPanel::Details), "enter") if in_details => {
                self.focus_panel(FocusPanel::History, window, cx);
                Some(true)
            }
            (_, "n") => {
                self.open_create_pull_request(repo_id, None, window, cx);
                Some(true)
            }
            (_, "r") => {
                selected?;
                self.start_review(cx);
                Some(true)
            }
            (_, "o") => Some(self.open_pull_request_on_github(cx)),
            _ => None,
        }
    }

    fn cycle_sidebar_tab(&mut self, direction: i8) -> bool {
        const TABS: [SidebarMode; 3] = [
            SidebarMode::Branches,
            SidebarMode::Files,
            SidebarMode::PullRequests,
        ];
        let Some(ix) = TABS
            .iter()
            .position(|mode| *mode == self.state.sidebar_mode)
        else {
            return false;
        };
        let next = if direction < 0 {
            ix.checked_sub(1)
        } else {
            Some(ix + 1).filter(|next| *next < TABS.len())
        };
        let Some(next) = next else {
            return false;
        };
        self.store
            .dispatch(Msg::SetSidebarMode { mode: TABS[next] });
        true
    }

    /// Handles one plain keystroke for panel navigation. Returns whether it was
    /// consumed.
    pub(crate) fn handle_panel_key(
        &mut self,
        keystroke: &gpui::Keystroke,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        // With nothing focused the root's capture listener never runs, so the
        // open `?` list and Codex menu are served from here too.
        if self.reviewer_menu.is_some() {
            return self.handle_reviewer_menu_key(keystroke, window, cx);
        }
        if self.codex_menu_open {
            return self.handle_codex_menu_key(keystroke, window, cx);
        }
        if self.keys_help_panel.is_some() {
            return self.handle_keys_help_key(keystroke, window, cx);
        }
        if self.commit_scope_picker.is_some() {
            return self.handle_commit_scope_picker_key(keystroke, cx);
        }
        let mods = keystroke.modifiers;
        if mods.control || mods.alt || mods.platform || mods.function {
            return false;
        }
        if self.codex_panel_focused(window)
            && !self.command_palette_open
            && !self.reveal_commit_open
            && !self.is_overlay_open(cx)
            && !mods.shift
        {
            let key = keystroke.key.as_str();
            if key == "i" {
                if self.pr_reviewer_context_active() {
                    self.open_reviewer_menu(cx);
                } else {
                    self.codex_menu_open = true;
                }
                cx.notify();
                return true;
            }
            if self.handle_codex_panel_key(key, window, cx) {
                return true;
            }
            // Anything else works as it does from a panel (`1`–`4`, `?`, …).
        }
        if !self.panel_keys_active(window, cx) {
            return false;
        }
        if is_keys_help_key(keystroke) {
            let panel = self
                .focused_panel(window, cx)
                .unwrap_or_else(|| self.default_panel());
            self.keys_help_panel = Some(panel);
            cx.notify();
            return true;
        }
        let key = keystroke.key.as_str();
        // A focused panel that is no longer on screen (collapsed, or History
        // behind a diff) is treated as no focus at all.
        let current = self
            .focused_panel(window, cx)
            .filter(|panel| *panel == FocusPanel::Diff || self.panel_available(*panel));
        if let Some(handled) = self.handle_review_key(current, key, mods.shift, window, cx) {
            return handled;
        }
        if let Some(handled) = self.handle_pull_request_key(current, key, mods.shift, window, cx) {
            return handled;
        }
        if let Some(handled) = self.handle_action_key(current, key, mods.shift, window, cx) {
            return handled;
        }
        if mods.shift {
            return false;
        }
        match key {
            "0" => {
                self.focus_codex_panel(window, cx);
                return true;
            }
            "i" => {
                if self.pr_reviewer_context_active() {
                    self.open_reviewer_menu(cx);
                } else {
                    self.codex_menu_open = true;
                }
                cx.notify();
                return true;
            }
            _ => {}
        }
        if let Some(panel) = FocusPanel::from_digit_key(key) {
            // The middle area shows History or the diff, one at a time.
            let panel = if panel == FocusPanel::History && self.diff_is_open() {
                FocusPanel::Diff
            } else {
                panel
            };
            // `3` without a diff has nothing to focus; still consumed so the
            // digit never leaks into anything behind the panels.
            self.focus_panel(panel, window, cx);
            return true;
        }
        let direction = match key {
            "h" | "left" | "k" | "up" | "[" => -1,
            "l" | "right" | "j" | "down" | "]" => 1,
            "enter" => 0,
            _ => return false,
        };
        let Some(current) = current else {
            // Nothing focused yet: the first navigation key lands on the main area.
            if key == "enter" {
                return false;
            }
            let panel = self.default_panel();
            self.focus_panel(panel, window, cx);
            return true;
        };
        match key {
            "h" | "left" | "l" | "right" => {
                if let Some(next) = current.step(direction, |panel| self.panel_available(panel)) {
                    self.focus_panel(next, window, cx);
                }
                true
            }
            "j" | "down" | "k" | "up" => {
                self.move_in_panel(current, direction, cx);
                true
            }
            "[" | "]" => current == FocusPanel::Sidebar && self.cycle_sidebar_tab(direction),
            "enter" => self.open_in_panel(current, window, cx),
            _ => false,
        }
    }

    /// Runs in the capture phase so the open `?` list sees keys before any
    /// panel does: esc must close the list, not the diff underneath it.
    pub(super) fn handle_keys_help_key(
        &mut self,
        keystroke: &gpui::Keystroke,
        window: &Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if self.keys_help_panel.is_none() {
            return false;
        }
        // Something else took over (a chord opened the palette or a dialog):
        // the list steps aside instead of swallowing that surface's typing.
        if !self.panel_keys_active(window, cx) {
            self.keys_help_panel = None;
            cx.notify();
            return false;
        }
        let mods = keystroke.modifiers;
        if mods.control || mods.alt || mods.platform || mods.function {
            return false;
        }
        if keystroke.key == "escape" || is_keys_help_key(keystroke) {
            self.keys_help_panel = None;
            cx.notify();
        }
        // The list is modal: every other plain key goes nowhere.
        true
    }

    pub(super) fn render_keys_help(
        &self,
        panel: FocusPanel,
        cx: &mut gpui::Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let scale = crate::ui_scale::UiScale::current(cx);
        let row = |keys: &'static str, label: &'static str| {
            div()
                .flex()
                .items_center()
                .gap(scale.px(12.0))
                .py(scale.px(3.0))
                .child(
                    div()
                        .w(scale.px(96.0))
                        .flex_shrink_0()
                        .child(components::shortcut_keys(keys, theme, scale)),
                )
                .child(
                    div()
                        .text_size(scale.ui_text(13.0))
                        .text_color(theme.colors.foreground.primary)
                        .child(label),
                )
        };
        let section = |title: &'static str, rows: &'static [(&'static str, &'static str)]| {
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .pb(scale.px(4.0))
                        .text_size(scale.ui_text(11.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.colors.foreground.secondary)
                        .child(title),
                )
                .children(rows.iter().map(|&(keys, label)| row(keys, label)))
        };
        let legend_row = |id: &'static str, icon: AnyElement, label: &'static str| {
            div()
                .debug_selector(move || id.to_string())
                .flex()
                .items_center()
                .gap(scale.px(8.0))
                .py(scale.px(2.0))
                .child(
                    div()
                        .w(scale.px(20.0))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .child(icon),
                )
                .child(
                    div()
                        .text_size(scale.ui_text(13.0))
                        .text_color(theme.colors.foreground.primary)
                        .child(label),
                )
        };
        // A section of its own rather than more `key_help` rows: those render
        // through `shortcut_keys`, which turns any left column text into
        // keycap chips, and "Symbols" / "Review" aren't keys.
        let pr_legend = || {
            let size = scale.px(14.0);
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .pb(scale.px(4.0))
                        .text_size(scale.ui_text(11.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.colors.foreground.secondary)
                        .child("Pull request symbols"),
                )
                .child(legend_row(
                    "legend_open",
                    pr_symbols::state("OPEN", false, theme).render(
                        "legend_open".to_string(),
                        theme,
                        size,
                    ),
                    "Open",
                ))
                .child(legend_row(
                    "legend_draft",
                    pr_symbols::state("OPEN", true, theme).render(
                        "legend_draft".to_string(),
                        theme,
                        size,
                    ),
                    "Draft",
                ))
                .child(legend_row(
                    "legend_merged",
                    pr_symbols::state("MERGED", false, theme).render(
                        "legend_merged".to_string(),
                        theme,
                        size,
                    ),
                    "Merged",
                ))
                .child(legend_row(
                    "legend_closed",
                    pr_symbols::state("CLOSED", false, theme).render(
                        "legend_closed".to_string(),
                        theme,
                        size,
                    ),
                    "Closed",
                ))
                .child(legend_row(
                    "legend_review_required",
                    pr_symbols::review(ReviewDecision::ReviewRequired, theme).render(
                        "legend_review_required".to_string(),
                        theme,
                        size,
                    ),
                    "Review required",
                ))
                .child(legend_row(
                    "legend_review_approved",
                    pr_symbols::review(ReviewDecision::Approved, theme).render(
                        "legend_review_approved".to_string(),
                        theme,
                        size,
                    ),
                    "Approved",
                ))
                .child(legend_row(
                    "legend_review_changes",
                    pr_symbols::review(ReviewDecision::ChangesRequested, theme).render(
                        "legend_review_changes".to_string(),
                        theme,
                        size,
                    ),
                    "Changes requested",
                ))
                .child(legend_row(
                    "legend_review_commented",
                    pr_symbols::review(ReviewDecision::Commented, theme).render(
                        "legend_review_commented".to_string(),
                        theme,
                        size,
                    ),
                    "Commented",
                ))
                .child(legend_row(
                    "legend_checks_failing",
                    pr_symbols::checks(
                        ChecksSummary {
                            failing: 1,
                            pending: 0,
                            passing: 0,
                        },
                        theme,
                    )
                    .expect("failing > 0 always renders a symbol")
                    .render("legend_checks_failing".to_string(), theme, size),
                    "Checks failing",
                ))
                .child(legend_row(
                    "legend_checks_pending",
                    pr_symbols::checks(
                        ChecksSummary {
                            failing: 0,
                            pending: 1,
                            passing: 0,
                        },
                        theme,
                    )
                    .expect("pending > 0 always renders a symbol")
                    .render("legend_checks_pending".to_string(), theme, size),
                    "Checks running",
                ))
                .child(legend_row(
                    "legend_checks_passing",
                    pr_symbols::checks(
                        ChecksSummary {
                            failing: 0,
                            pending: 0,
                            passing: 1,
                        },
                        theme,
                    )
                    .expect("passing > 0 always renders a symbol")
                    .render("legend_checks_passing".to_string(), theme, size),
                    "Checks passing",
                ))
                .child(legend_row(
                    "legend_person",
                    pr_symbols::person(theme).render("legend_person".to_string(), theme, size),
                    "Your pull request",
                ))
                .child(legend_row(
                    "legend_pending",
                    pr_symbols::pending(1, theme).render(
                        "legend_pending".to_string(),
                        theme,
                        size,
                    ),
                    "Pending review comments",
                ))
                .child(legend_row(
                    "legend_at_pill",
                    pr_symbols::at_pill(theme, "legend_at_pill"),
                    "Your review is requested",
                ))
                .child(legend_row(
                    "legend_kind_tag",
                    pr_symbols::kind_tag("feat", theme, scale.ui_text(11.0)),
                    "feat / fix / refactor / docs / deps from the title",
                ))
        };
        let body = components::modal_surface(theme)
            .p(scale.px(14.0))
            .flex()
            .flex_col()
            .gap(scale.px(12.0))
            .child(
                div()
                    .text_size(scale.ui_text(14.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.colors.foreground.primary)
                    .child(format!("Keys · {}", panel.name())),
            )
            .child(section(panel.name(), self.key_help(panel)))
            .when(self.state.sidebar_mode == SidebarMode::PullRequests, |body| {
                body.child(pr_legend())
            })
            .child(section("Every panel", PANEL_KEYS_HELP))
            .child(
                div()
                    .text_size(scale.ui_text(12.0))
                    .text_color(theme.colors.foreground.secondary)
                    .child(
                        "esc or ? closes this list. Panel keys are off while typing, \
                         in the terminal, and in menus and dialogs.",
                    ),
            );
        let scrim = components::modal_scrim(theme)
            .id("keys_help_scrim")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                    this.keys_help_panel = None;
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
                            .debug_selector(|| "keys_help".to_string())
                            .w(scale.px(460.0))
                            .child(body),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::FocusPanel::{self, *};

    fn all(_: FocusPanel) -> bool {
        true
    }

    #[test]
    fn digits_address_panels_left_to_right() {
        let panels: Vec<_> = ["1", "2", "3", "4", "5", "0", "h"]
            .into_iter()
            .map(FocusPanel::from_digit_key)
            .collect();
        // `2` is the middle area; the key handler turns it into the diff
        // while one is open. `4` is Details' old number.
        assert_eq!(
            panels,
            [
                Some(Sidebar),
                Some(History),
                Some(Details),
                Some(Details),
                None,
                None,
                None
            ]
        );
    }

    #[test]
    fn step_moves_one_panel_and_never_wraps() {
        assert_eq!(History.step(1, all), Some(Diff));
        assert_eq!(History.step(-1, all), Some(Sidebar));
        assert_eq!(Sidebar.step(-1, all), None);
        assert_eq!(Details.step(1, all), None);
    }

    #[test]
    fn step_skips_unavailable_panels() {
        // History and Diff take turns in the main area; collapsed panels drop out.
        let diff_shown = |panel| panel != History;
        assert_eq!(Sidebar.step(1, diff_shown), Some(Diff));
        assert_eq!(Details.step(-1, diff_shown), Some(Diff));
        let history_shown_sidebar_collapsed = |panel| panel != Diff && panel != Sidebar;
        assert_eq!(
            History.step(1, history_shown_sidebar_collapsed),
            Some(Details)
        );
        assert_eq!(History.step(-1, history_shown_sidebar_collapsed), None);
        assert_eq!(Details.step(-1, |panel| panel == Details), None);
    }
}
