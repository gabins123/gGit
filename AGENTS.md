# Project rules

## Keyboard-first

Every user-facing feature ships with keyboard access in the same change. A feature without a
shortcut is not done.

- Every action works without a mouse: a single key in its panel (lazygit-style), an Alt chord inside
  a dialog, or an entry in the Codex menu.
- Panel keys go through `crates/gitcomet-ui-gpui/src/view/panel_focus.rs` (`handle_panel_key`), and
  the new key appears in the status-bar hints (`key_hints`) and the `?` list (`key_help`).
- Keys stay inert while typing and in terminals, menus, popovers, pickers, dialogs and conflict
  editors (`panel_keys_active`).
- Document the key in `docs/shortcuts.md` and drive it in a UI test under
  `crates/gitcomet-ui-gpui/src/view/panels/tests/shortcuts/` with `simulate_keystrokes`.
