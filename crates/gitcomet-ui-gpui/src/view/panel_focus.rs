//! Lazygit-style keyboard focus across the four main panels.
//!
//! `1`–`4` pick a panel, `h`/`l` (or Left/Right) step between the ones on
//! screen, `j`/`k` (or Down/Up) move within the focused one, and `enter` opens
//! the selection. Every one of these is inert unless a panel itself holds
//! focus: text inputs, the terminal, menus, popovers, pickers, dialogs and the
//! conflict resolver keep their keys.

use super::*;
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

    fn from_digit_key(key: &str) -> Option<Self> {
        match key {
            "1" => Some(Self::Sidebar),
            "2" => Some(Self::History),
            "3" => Some(Self::Diff),
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
            Self::Sidebar => &[("j/k", "branch"), ("enter", "history"), ("[ ]", "tab")],
            Self::History => &[("j/k", "commit"), ("enter", "files")],
            Self::Diff => &[("j/k", "change"), ("F1/F4", "file"), ("esc", "back")],
            Self::Details => &[("j/k", "file"), ("enter", "diff")],
        }
    }

    /// This panel's own keys, as the `?` list shows them.
    fn help(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Sidebar => &[
                ("j / k", "Next / previous branch, revealed in History"),
                ("enter", "Go to History"),
                ("[ / ]", "Branches / Files tab"),
            ],
            Self::History => &[
                ("j / k", "Next / previous commit"),
                ("enter", "Go to the commit's files in Details"),
            ],
            Self::Diff => &[
                ("j / k", "Next / previous change"),
                ("F1 / F4", "Previous / next file"),
                ("esc", "Close the diff and go back"),
            ],
            Self::Details => &[
                ("j / k", "Next / previous file, opening its diff"),
                ("enter", "Go to the file's diff"),
            ],
        }
    }
}

/// Status bar hints shown after the focused panel's own.
pub(super) const PANEL_STATUS_HINTS: &[(&str, &str)] =
    &[("1-4", "panels"), ("i", "codex"), ("?", "keys")];

/// Keys that work the same in every panel, as the `?` list shows them.
const PANEL_KEYS_HELP: &[(&str, &str)] = &[
    (
        "1 2 3 4",
        "Sidebar, History, Diff, Details; opens a collapsed one",
    ),
    ("h / l", "Previous / next panel"),
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
        if self.state.sidebar_mode == SidebarMode::PullRequests {
            match panel {
                FocusPanel::Sidebar => {
                    return &[
                        ("j/k", "PR"),
                        ("enter", "diff"),
                        ("n", "new"),
                        ("r", "review"),
                        ("o", "GitHub"),
                        ("R", "refresh"),
                    ];
                }
                FocusPanel::Details if self.pull_request_details_active() => {
                    return &[
                        ("j/k", "file"),
                        ("enter", "diff"),
                        ("r", "review"),
                        ("o", "GitHub"),
                    ];
                }
                _ => {}
            }
        }
        panel.status_hints()
    }

    fn key_help(&self, panel: FocusPanel) -> &'static [(&'static str, &'static str)] {
        if self.state.sidebar_mode == SidebarMode::PullRequests {
            match panel {
                FocusPanel::Sidebar => {
                    return &[
                        ("j / k", "Next / previous pull request"),
                        ("enter", "Open its diff"),
                        ("n", "New pull request"),
                        ("r", "Review the selected pull request"),
                        ("o", "Open on GitHub"),
                        ("R", "Refresh the list"),
                        ("[ / ]", "Branches / Files / Pull requests tab"),
                    ];
                }
                FocusPanel::Details if self.pull_request_details_active() => {
                    return &[
                        ("j / k", "Next / previous file"),
                        ("enter", "Open the file's diff"),
                        ("r", "Review"),
                        ("o", "Open on GitHub"),
                    ];
                }
                _ => {}
            }
        }
        panel.help()
    }

    /// Opens a pull request dialog that hands focus back to the panel it was
    /// opened from when it closes.
    pub(super) fn open_pull_request_prompt(
        &mut self,
        kind: PopoverKind,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        self.clear_pull_request_submit_error();
        let request = PopoverRequest::from(kind);
        let request = match window.focused(cx) {
            Some(focus) => request.returning_focus_to(focus),
            None => request,
        };
        self.open_popover_centered(request, window, cx);
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
        if shift {
            return None;
        }
        let selected = self.active_pull_requests().and_then(|prs| prs.selected);
        let in_details = current == Some(FocusPanel::Details) && self.pull_request_details_active();
        let direction = match key {
            "j" | "down" => 1,
            "k" | "up" => -1,
            _ => 0,
        };
        match (current, key) {
            (Some(FocusPanel::Sidebar), _) if direction != 0 => {
                self.select_adjacent_pull_request(direction, cx);
                Some(true)
            }
            (Some(FocusPanel::Details), _) if direction != 0 && in_details => {
                self.select_adjacent_pull_request_file(direction, cx);
                Some(true)
            }
            (Some(from @ (FocusPanel::Sidebar | FocusPanel::Details)), "enter")
                if selected.is_some() =>
            {
                if self.open_pull_request_diff(None, cx) {
                    self.diff_return_panel = from;
                    self.focus_diff_when_open = true;
                }
                Some(true)
            }
            (_, "n") => {
                if self.github_target().is_none() {
                    self.push_toast(
                        components::ToastKind::Warning,
                        "Pull requests need a github.com remote.".to_string(),
                        cx,
                    );
                } else {
                    self.open_pull_request_prompt(
                        PopoverKind::CreatePullRequest { repo_id },
                        window,
                        cx,
                    );
                }
                Some(true)
            }
            (_, "r") => {
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
        if self.codex_menu_open {
            return self.handle_codex_menu_key(keystroke, window, cx);
        }
        if self.keys_help_panel.is_some() {
            return self.handle_keys_help_key(keystroke, window, cx);
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
                self.codex_menu_open = true;
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
        if let Some(handled) = self.handle_pull_request_key(current, key, mods.shift, window, cx) {
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
                self.codex_menu_open = true;
                cx.notify();
                return true;
            }
            _ => {}
        }
        if let Some(panel) = FocusPanel::from_digit_key(key) {
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
        assert_eq!(
            panels,
            [
                Some(Sidebar),
                Some(History),
                Some(Diff),
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
