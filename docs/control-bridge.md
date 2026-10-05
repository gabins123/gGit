# Dev control bridge

A debug-only hook that lets an agent (or a script) drive a running gGit and
capture it, without OS input, focus changes or touching the desktop. It is
development tooling, not a user-facing feature, so the keyboard-first rule in
`AGENTS.md` does not apply: there is no shortcut, menu entry or setting.

## Gating

- Compiled only when `debug_assertions` is on, so release builds never
  contain it. No cargo feature is needed.
- Starts only when `GITCOMET_CONTROL_DIR` is set to a directory (created if
  missing). Unset, nothing runs: no thread, no timer, no polling.
- Only the window startup lands in is bridged (with several workspaces
  restored, the last-activated one). It takes typing even in the background:
  text fields there accept the bridge's keys without the window being active.

## Launch

```sh
cargo build -j 8 -p gitcomet
# A per-user folder: %LOCALAPPDATA%\Temp\gitcomet-ctl (or $XDG_RUNTIME_DIR/gitcomet-ctl off Windows).
GITCOMET_CONTROL_DIR="$LOCALAPPDATA/Temp/gitcomet-ctl" target/debug/gitcomet.exe
```

## Protocol

Files only. The app polls the directory every ~100 ms and runs `*.cmd` files in
name order on the UI thread against the main window. For each one it writes
`<same stem>.result` (temp name then rename, so a reader never sees a partial
file) and then deletes the `.cmd`. Other files are ignored. Write commands the
same way (temp name, then rename to `.cmd`); the helper does.

The first result line is `ok ...` or `error: <message>`; payload lines follow.
Unknown commands and malformed input give `error:`, never a crash.

| Command | Result |
| --- | --- |
| `keys <tokens...>` | `ok dispatched N` (+ ` (unhandled: a b)` for tokens nothing handled). |
| `screenshot <path>` | `ok <w>x<h> <path>`; PNG saved to `<path>` (absolute, or relative to the control dir; no spaces). |
| `state` | `ok`, then one JSON line. |
| `wait <ms>` | `ok waited Nms`; capped at 30000. |

**keys**: whitespace-separated tokens, each parsed like gpui's test
`simulate_keystrokes` (`Keystroke::parse`): `1`, `]`, `shift-j`, `enter`,
`escape`, `alt-p`, `ctrl-shift-p`, `space`, `tab`. Modifiers are `ctrl`, `alt`,
`shift`, `cmd`/`super`/`win`, `fn`, `secondary`. A bad token rejects the whole
command before anything is sent. Keys go through the window's normal dispatch,
with a ~75 ms pause after each so the store and UI can apply its effect before
the next key reads it. Use `wait` for slower async loads (`gh`, git).

**screenshot**: forces a redraw and waits for that frame to be presented, then
captures the window's own pixels with Win32 `PrintWindow(PW_RENDERFULLCONTENT)`
into an off-screen bitmap and saves a PNG. It works while other windows cover
the app and sends no input or focus change. Windows only: elsewhere it returns
`error: screenshot is only implemented on Windows`. A minimized window returns
`error: window is minimized; restore it to capture`, and no frame within 10 s
returns an `error:` too.

**state** fields: `repo_workdir`, `sidebar_mode` (`branches`/`files`/
`pull_requests`), `focused_panel` (`sidebar`/`history`/`diff`/`details`/null),
`selected_pr`, `pr_content_tab` (`conversation`/`comments`/null),
`review_active`, `review_file`, `file_list_layout` (`flat`/`tree`),
`window_size` (`[w, h]`, logical px), `ui_scale_percent`.

## Helper

```sh
export GITCOMET_CONTROL_DIR="$LOCALAPPDATA/Temp/gitcomet-ctl"
scripts/dev/gitcomet-ctl.py keys 1 ] ]
scripts/dev/gitcomet-ctl.py wait 1500
scripts/dev/gitcomet-ctl.py screenshot shot.png
scripts/dev/gitcomet-ctl.py state
```

`gitcomet-ctl.py [--dir DIR] [--timeout S] <command...>` writes a uniquely named
command, waits for its result (default 30 s), prints and removes it, and exits
non-zero on `error:` or timeout.

## Security

Any local process running as the same user can write to the control directory
and so press keys in the app and read its screen: the same trust as the user
themselves. Whoever can write the folder can drive the app and have `screenshot` write a
PNG to any path the user can write, so the folder must be private to the user
(not a shared location such as `/tmp`, where another user can pre-create it).
Never enable it in release builds (it is compiled out there).
