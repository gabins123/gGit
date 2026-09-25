# GitComet Shortcuts

This file documents the keyboard shortcuts currently wired in the GPUI application.

Source of truth:
- `crates/gitcomet-ui-gpui/src/app.rs`
- `crates/gitcomet-ui-gpui/src/focused_diff.rs`
- `crates/gitcomet-ui-gpui/src/view/terminal_panel.rs`
- `crates/gitcomet-ui-gpui/src/view/panels/main/diff_view.rs`
- `crates/gitcomet-ui-gpui/src/view/conflict_resolver.rs`
- `crates/gitcomet-ui-gpui/src/view/panel_focus.rs`

Notes:
- `Cmd` and `Option` are the macOS names. `Ctrl` and `Alt` are the Windows/Linux equivalents.
- Some controls keep extra compatibility aliases in addition to the primary platform shortcut.
- Context menus also display per-entry shortcuts inline.

## App window shortcuts

These shortcuts apply in the normal GitComet window.

| Action | macOS | Windows / Linux | Notes |
| --- | --- | --- | --- |
| Open a new window | `Cmd-N`, `Cmd-Shift-N` | `Ctrl-N`, `Ctrl-Shift-N` | |
| Open Settings | `Cmd-,` | `Ctrl-,` | |
| Open a repository | `Cmd-O` | `Ctrl-O` | |
| Go to a commit | `Cmd-G` | `Ctrl-G` | Opens the Go to dialog. Accepts a full or short SHA (4+ characters, unique), a branch, a tag, or any revision such as `HEAD~3`; matches are exact. In an embedded terminal the shell keeps `Ctrl-G`. |
| Toggle open and recently closed repositories | `Ctrl-Shift-A`, `Cmd-Shift-O`, `Option-Cmd-O` | `Ctrl-Shift-A`, `Ctrl-Shift-O` | In an embedded terminal on Windows/Linux, `Ctrl-Shift-A` keeps its terminal “Select All” behavior. |
| Open active repository in external code editor | `Cmd-Shift-E` | `Ctrl-Shift-E` | Only active when an external code editor is configured. |
| Show the open file in the file explorer | `Cmd-Shift-L` | `Ctrl-Shift-L` | Switches the sidebar to Files, expands the folders leading to the file, and scrolls it into view. |
| Open the repository's remote in a web browser | `Cmd-K` | `Ctrl-K` | Opens the remote's page on its host (GitHub, GitLab, Bitbucket, Azure DevOps, Gitea/Codeberg, AWS CodeCommit, or a self-hosted forge). With several such remotes a menu lists them, `origin` first; `1`–`9` pick directly. Also in the command palette, the app menu, and a remote's right-click menu in the sidebar. In an embedded terminal on Windows/Linux the shell keeps `Ctrl-K`. |
| Close the active repository tab, or close the window if no repo tab can close | `Cmd-W` | `Ctrl-W` | |
| Close the active window | `Cmd-Shift-W` | `Ctrl-Shift-W` | |
| Previous repository tab | `Cmd-PageUp`, `Cmd-{`, `Option-Cmd-Left` | `Ctrl-PageUp`, `Ctrl-Shift-Tab` | |
| Next repository tab | `Cmd-PageDown`, `Cmd-}`, `Option-Cmd-Right` | `Ctrl-PageDown`, `Ctrl-Tab` | |
| Toggle full screen | `Ctrl-Cmd-F` | `F11` | |
| Quit GitComet | `Cmd-Q` | `Ctrl-Q` | |

macOS-only window-management shortcuts:
- `Cmd-M`: Minimize the active window.
- `Cmd-H`: Hide GitComet.
- `Option-Cmd-H`: Hide other applications.

## Panel navigation

Lazygit-style keyboard focus across the four main panels. The focused panel carries an outline.

These keys only work while a panel itself has focus (or nothing does). They are inert while typing in any text field, in the embedded terminal, and while a menu, popover, picker, dialog, the command palette or the conflict resolver has focus.

| Action | Key | Notes |
| --- | --- | --- |
| Focus Sidebar / History / Diff / Details | `1` / `2` / `3` / `4` | Opens a collapsed Sidebar or Details first. History and the diff share the main area: `2` closes an open diff, and `3` does nothing unless a diff is open. |
| Previous / next panel | `h` / `l`, `Left` / `Right` | Skips collapsed panels and whichever of History or Diff is not showing. Never wraps. |
| Move within the focused panel | `j` / `k`, `Down` / `Up` | `j` moves down and `k` up. Sidebar: next/previous branch, revealed in History. History: next/previous commit. Diff: next/previous change. Details: next/previous file, opening its diff; starts at the first file when none is selected. |
| Open | `Enter` | Sidebar → History. History → Details (the commit's files). Details → the file's diff. |
| Back from a diff | `Escape` | Closes the diff and returns focus to the panel it was opened from. |
| Previous / next sidebar tab | `[` / `]` | Branches, Files and Pull requests. |
| List the focused panel's keys | `?` | Modal; `Escape` or `?` closes it. The status bar also shows the focused panel's main keys. |

When a dialog or menu opened from these keys closes, focus returns to the panel it was opened from. Focus whose element disappears (a dialog confirmed, a row removed) likewise returns to the last focused panel.

### Git actions

Single keys for everyday Git work, lazygit-style. Each one runs the same action (and the same confirmation dialog) as the matching context-menu entry or button. `m` opens the selection's full context menu, driven with the arrow keys and `Enter` (the commit menu's entries also have letter keys), so every row action is reachable without the mouse.

| Action | Key | Where | Notes |
| --- | --- | --- | --- |
| Write the commit message | `c` | Any panel | Opens Details and focuses the message box. A commit selected in History is deselected first so the box shows. |
| Toggle amending the last commit | `Shift+A` | Any panel | Then focuses the message box. Unavailable during a merge or rebase, or before the first commit. |
| Pull / push | `p` / `Shift+P` | Any panel | The main Pull and Push buttons: the default pull mode, and a set-upstream dialog for a branch without one. |
| Fetch all remotes | `f` | Any panel | |
| Stash the changes | `s` | Any panel | Opens the stash dialog. Apply, pop and drop are in the command palette. |
| Stage / unstage the open file, then the next | `Space` | Details, Diff | The diff's existing `Space`, reachable from Details too. |
| Stage everything, or unstage it all | `a` | Details | Stages all changes; with nothing left to stage, unstages everything. |
| Discard the open file's changes | `d` | Details, Diff | Always confirms. Not for conflicted files. |
| The selection's context menu | `m` | Any panel | Sidebar: the branch. History: the commit. Details and Diff: the open file. |
| Check out the branch | `Space` | Sidebar | A remote branch asks for the local branch name. |
| New branch from the selected one | `n` | Sidebar | With no branch selected, from the current one. |
| Delete the branch | `Shift+D` twice | Sidebar | The first press asks for the second. Local branches other than the checked-out one. An unmerged branch then asks before force-deleting. |
| Merge it into the current branch | `Shift+M` twice | Sidebar | The first press asks for the second. |
| Rebase the current branch onto it | `Shift+R` | Sidebar | Confirms first. |
| Open a pull request on GitHub | `o` | Sidebar | lazygit's key: GitHub's new pull request page for the branch, in the browser, against the default branch. The branch must already be on GitHub. |
| Set up a pull request from the branch | `Shift+O` | Sidebar | The New pull request dialog for that branch (base, title, body, draft). `Ctrl+Enter` creates it through gh; `Alt+O` opens GitHub's page with the chosen base instead. |
| Cherry-pick the commit | `Shift+C` | History | Confirms first. Not the HEAD commit. |
| Revert the commit | `t` | History | Confirms first. |
| Reset to the commit | `g` | History | A mixed reset, after confirming. Soft and hard resets are in the commit's menu (`m`). |
| Tag the commit | `Shift+T` | History | |

On the Pull requests tab, its own keys (`n`, `r`, `o`, `Shift+R`) take precedence. While the conflict resolver is open, `a`–`d` (with or without `Shift`) stay its picks.

### Pull requests tab

GitHub pull requests go through the [GitHub CLI](https://cli.github.com) (`gh`), which owns sign-in; GitComet never sees a token. The tab lists the open pull requests of the repository's github.com remote: `upstream` first (a fork's parent, where gh sends pull requests too), then `origin`. A branch pushed to a fork opens its pull request as `owner:branch`.

| Action | Key | Notes |
| --- | --- | --- |
| Next / previous pull request | `j` / `k` | Sidebar. Selecting one shows it in Details. |
| Next / previous changed file | `j` / `k` | Details, while it shows a pull request. Moves the diff along once one is open. |
| Open the diff | `Enter` | The pull request's commits are fetched by object id — no branch, ref or working-tree file changes — and shown as a merge-base..head diff. Pull requests over 100 files or 20,000 changed lines are left to GitHub. |
| New pull request | `n` | From the checked-out branch, which must already be pushed: creating never pushes. In the dialog, `Alt+D` toggles draft, `Alt+P` runs the normal push, `Alt+O` opens GitHub's page for it instead, and `Ctrl+Enter` (`Cmd+Enter`) creates. From another branch: `Shift+O` on it in the Branches tab. |
| Review | `r` | Opens review mode for the pull request (below), picking up a pending review of it where it was left. |
| Your review queue | | The list leads with the pull requests waiting for your review, then the ones with a review of yours pending on this computer (with its count), then the rest; `j` / `k` follow that order. |
| Quick review | `Shift+S` | Just a verdict and a summary, no line comments. `Alt+C` / `Alt+A` / `Alt+X` pick Comment, Approve or Request changes; `Ctrl+Enter` (`Cmd+Enter`) posts. Comment and Request changes need text. |
| Check out locally | `Space` | Sidebar or Details. Runs `gh pr checkout`, which fetches the branch into a local branch of the same name and checks it out. A pull request from a fork (or one whose details haven't loaded) gets its own `pr/<number>` branch instead, so a same-named local branch is never fast-forwarded to someone else's commits. git refuses over conflicting uncommitted changes. |
| Merge on GitHub | `Shift+M` | Confirms first. `Alt+M` / `Alt+S` / `Alt+R` pick merge commit, squash or rebase; `Alt+D` also deletes the branch on GitHub (not offered for forks); `Enter` merges. GitHub refuses if the branch moved since its details loaded, so only the commits you saw land. With a required merge queue, GitHub queues it instead. Local branches are left alone. |
| Scroll the checks and conversation | `Shift+J` / `Shift+K` | Details. Below the changed files it lists each check (failing first) and the comments and reviews, oldest first, as plain text. Hidden comments are left out. |
| Open on GitHub | `o` | The selected pull request, or the repository's pull request list. |
| Refresh | `Shift+R` | The list loads when the tab first shows; there is no polling. |

### Review mode

`r` on a pull request reviews it file by file. Line comments wait as a pending review, saved on this computer (per repository and pull request) until `S` posts them all together as one GitHub review, through `gh api`, on the head commit the diff shows. If the pull request gets new commits, the review moves to them and asks you to check your comments' lines. Nothing reaches GitHub before that. The Sidebar lists the pull request's files, the main area shows the file's diff, and Details becomes Your review.

| Action | Key | Where | Notes |
| --- | --- | --- | --- |
| Move the line cursor | `j` / `k`, `Down` / `Up` | Diff | The cursor starts on the file's first change. |
| Select lines | `Shift+J` / `Shift+K`, `Shift+Down` / `Shift+Up` | Diff | Grows a selection from the cursor; a plain move drops it, as does `Escape`. No modes. |
| Comment | `c` | Diff | On the line or the selected lines. `Ctrl+Enter` adds it to the review; `Escape` closes the box and keeps the text for those lines. GitHub only takes comments on changed lines and the 3 lines around them. |
| Suggest a change | `Alt+S` | Comment box | Puts the selected lines in a GitHub suggestion block, after what's typed, to edit into the fix. GitHub then offers to commit it. Not on removed lines. |
| Next / previous thread | `t` / `Shift+T` | Diff | Threads already on GitHub, marked in blue (your pending comments are amber). Details shows the thread under the cursor. |
| Reply | `r` | Diff | To the thread on the line under the cursor. The reply waits with the rest of the review and posts right after it. |
| Codex suggestions | `i` then `p` | Any panel | In review mode, Codex reviews the pull request's patch as suggested line comments, marked in grey; `t` steps to them too and Details shows the one under the cursor. Nothing is posted: `a` adopts it as your pending comment (edit or delete it like any other), `x` drops it. |
| Next / previous change | `}` / `{` | Diff | |
| Next / previous file | `]` / `[` | Any panel | `j` / `k` in the Sidebar too. |
| Mark the file viewed | `Space` | Diff, Sidebar | Then goes to the next file not yet viewed. `Space` again unmarks it. |
| Go to a pending comment | `Enter` | Details | `j` / `k` pick one; `e` edits it, `d` `d` deletes it. |
| Submit | `Shift+S` | Any panel | The review dialog, with the verdict and summary, and the pending line comments going up with it; replies post right after. A Comment review with line comments needs no summary; Request changes always does, and pending replies alone post without a review. Whatever reaches GitHub leaves the draft, even if something after it fails; review mode closes once nothing is left. |
| Leave | `q` | Any panel | Back to the pull request list. The pending review stays saved; `r` picks it up again. |

### Codex

Read-only suggestions from Codex through the `codex` CLI on your ChatGPT subscription; sign-in stays with the CLI (`codex login`). Each run is `codex exec` with its shell tool off, a read-only sandbox, no saved session and in an empty temporary directory with none of your Codex config, rules, hooks, apps or plugins, and with shell, browser, computer-use, image and web-search tools off. Your global Codex instructions (`~/.codex/AGENTS.md`) may still apply. GitComet gathers the material itself and pipes it in, marked as data; pull request content is also marked as untrusted. Nothing is posted, committed or pushed. The one write is filling the commit message box, and only while it is empty.

| Action | Key | Notes |
| --- | --- | --- |
| Codex actions | `i` | A menu: `m` commit message, `r` review local changes, `d` explain the open diff, `c` explain the selected commit, `f` explain the open file, `p` review the selected pull request, `q` ask about the repository. |
| Focus the Codex panel | `0` | The answer is editable. In the panel: `y` copy, `u` use as the selected pull request's review, `e` edit, `a` ask, `s` stop a run, `x` or `Escape` close. |
| Draft a review with Codex | `Alt+G` | In the review dialog. The draft fills the box when it is still empty. |

## Text input shortcuts

These shortcuts apply when a GitComet text input has focus.

### Editing

| Action | macOS | Windows / Linux | Notes |
| --- | --- | --- | --- |
| Select all | `Cmd-A` | `Ctrl-A` | `Ctrl-A` is also accepted on macOS. |
| Copy | `Cmd-C` | `Ctrl-C` | `Ctrl-C` is also accepted on macOS. |
| Paste | `Cmd-V` | `Ctrl-V` | `Ctrl-V` is also accepted on macOS. |
| Cut | `Cmd-X` | `Ctrl-X` | `Ctrl-X` is also accepted on macOS. |
| Undo | `Cmd-Z` | `Ctrl-Z` | |
| Redo | `Cmd-Shift-Z` | `Ctrl-Shift-Z` | |
| Show the character palette | `Ctrl-Cmd-Space` | None | macOS only. |

### Cursor movement and selection

| Action | macOS | Windows / Linux | Notes |
| --- | --- | --- | --- |
| Move by character or line | Arrow keys | Arrow keys | `Left`, `Right`, `Up`, `Down` |
| Select by character or line | `Shift` + arrow keys | `Shift` + arrow keys | |
| Move to document start / end | `Cmd-Home`, `Cmd-End` | `Ctrl-Home`, `Ctrl-End` | Picker search inputs also select the first / last result. |
| Move to line start / end | `Cmd-Left`, `Cmd-Right`, `Home`, `End` | `Home`, `End` | |
| Select to line start / end | `Cmd-Shift-Left`, `Cmd-Shift-Right`, `Shift-Home`, `Shift-End` | `Shift-Home`, `Shift-End` | |
| Move by page | `PageUp`, `PageDown` | `PageUp`, `PageDown` | |
| Select by page | `Shift-PageUp`, `Shift-PageDown` | `Shift-PageUp`, `Shift-PageDown` | |

### Word movement and word deletion

| Action | macOS | Windows / Linux | Notes |
| --- | --- | --- | --- |
| Move left / right by word | `Option-Left`, `Option-Right` | `Ctrl-Left`, `Ctrl-Right` | |
| Select left / right by word | `Option-Shift-Left`, `Option-Shift-Right` | `Ctrl-Shift-Left`, `Ctrl-Shift-Right` | |
| Delete word to the left / right | `Option-Backspace`, `Option-Delete` | `Ctrl-Backspace`, `Ctrl-Delete` | |

Compatibility note:
- GitComet also keeps the opposite modifier family wired in text inputs where practical, so `Alt`-based word movement and `Ctrl`-based editing aliases remain available as portability fallbacks.
- Diff-navigation fallbacks stay active from focused GitComet text inputs for `F1`, `F4`, `F2`, `F3`, `F7`, `Shift-F7`, `Alt-Up`, and `Alt-Down` when the input does not handle those keys itself.

### Commit composer

| Action | macOS | Windows / Linux | Notes |
| --- | --- | --- | --- |
| Commit staged changes | `Cmd-Enter` | `Ctrl-Enter` | Commit message input only, and only when the Commit action is enabled. |

## Picker shortcuts

These shortcuts apply while a picker's search input has focus, including file history.

| Action | macOS | Windows / Linux | Notes |
| --- | --- | --- | --- |
| Previous / next result | `Up`, `Down` | `Up`, `Down` | `Shift-Tab` / `Tab` also navigate. |
| First / last result | `Cmd-Home`, `Cmd-End` | `Ctrl-Home`, `Ctrl-End` | `Ctrl-Home` / `Ctrl-End` also work on macOS. |
| Previous / next page | `PageUp`, `PageDown` | `PageUp`, `PageDown` | Moves by one visible page and stops at the first / last result. |
| Activate selected result | `Enter` | `Enter` | In file history, opens the file at that commit, following renames. |
| Close menu or picker | `Escape` | `Escape` | Closes a row's context menu first, keeping the picker open. |

File history filters by SHA, summary, and author while preserving history order. Right-click a row to open a revision or its parent, show the file's changes, reveal the commit in history, copy its SHA, or use the standard commit actions. Copying keeps the history picker open.

## Embedded terminal shortcuts

These shortcuts apply when the embedded terminal has focus.

| Action | macOS | Windows / Linux | Notes |
| --- | --- | --- | --- |
| Copy terminal selection | `Cmd-C` | `Ctrl-Shift-C` | Disabled/no-op without a terminal selection. |
| Paste clipboard text | `Cmd-V` | `Ctrl-Shift-V` | Clipboard CRLF/CR line endings are normalized to LF. Bracketed paste is used when the shell enables it. |
| Select all terminal buffer | `Cmd-A` | `Ctrl-Shift-A` | Selects the scrollback buffer plus the visible terminal grid. |
| Scroll terminal history | `Shift-PageUp`, `Shift-PageDown`, `Shift-Home`, `Shift-End` | `Shift-PageUp`, `Shift-PageDown`, `Shift-Home`, `Shift-End` | Only in the normal screen buffer. |

Shell-input note:
- Plain `Ctrl-C`, `Ctrl-V`, and `Ctrl-A` keep going to the shell instead of GitComet clipboard handling.

Mouse and menu behavior:
- Left-drag selects visible terminal text.
- Right-click focuses the terminal and opens a menu without clearing the current selection.
- The terminal menu includes Copy, Paste, Select All, Clear, and Open in External Terminal.
- Clear sends `Ctrl-L` to the shell and is disabled when the embedded terminal is disconnected.

## Diff view shortcuts

These shortcuts apply in the main diff panel, including conflict resolution views where noted.

| Action | macOS | Windows / Linux | Scope / notes |
| --- | --- | --- | --- |
| Open file history | `Ctrl-H` | `Ctrl-H` | Working-tree files and files viewed at a commit; not while a text field has focus. |
| Search the current diff | `Cmd-F` | `Ctrl-F` | If rendered markdown preview is open, GitComet switches back to source mode before opening search. |
| Insert a newline in diff search | `Shift-Enter` | `Shift-Enter` | Diff search only. The search box also has Match Case, Whole Word, and Regex toggles. |
| Previous search match | `F2` | `F2` | While diff search is open. |
| Next search match | `F3` | `F3` | While diff search is open. |
| Close search, clear selection, or close the current diff | `Escape` | `Escape` | Exact behavior depends on the current diff state. |
| Previous file in the status list | `F1` | `F1` | Working tree and conflict-oriented diff flows. |
| Next file in the status list | `F4` | `F4` | Working tree and conflict-oriented diff flows. |
| Previous change | `F2`, `Shift-F7`, `Option-Up` | `F2`, `Shift-F7`, `Alt-Up` | Moves one change block (a run of consecutive changed lines) at a time, landing on its first line, in Full, Collapsed, and whole-commit diffs alike. The conflict resolver moves by conflict instead. |
| Next change | `F3`, `F7`, `Option-Down` | `F3`, `F7`, `Alt-Down` | Same unit as Previous change. With nothing selected, goes to the first change block. |
| Switch to inline diff | `Option-I` | `Alt-I` | Raw file diff only. Conflict resolver keeps split layout. |
| Enter or leave the file editor | `Option-E` | `Alt-E` | Not while a text field has focus. Escape also leaves the editor. |
| Save the edited file | `Cmd-S` | `Ctrl-S` | Only while the editor's buffer has focus; outside it the same chord stages the file. |
| Switch to split diff | `Option-S` | `Alt-S` | Raw file diff only. |
| Toggle whitespace characters | `Option-W` | `Alt-W` | Text diff / conflict diff only. |
| Stage or unstage the current working-tree file and advance to the adjacent file | `Space` | `Space` | Raw working-tree file diff only, and not while the diff search input has focus. |
| Select all diff text | `Cmd-A` | `Ctrl-A` | File preview and text-selection flows. |
| Copy selected diff text | `Cmd-C` | `Ctrl-C` | File preview and text-selection flows. |
| Pick conflict result `Base / Ours / Theirs / Both` | `A`, `B`, `C`, `D` | `A`, `B`, `C`, `D` | Conflict resolver only. |
| Previous unresolved conflict | `Shift-F2` | `Shift-F2` | Conflict resolver only. Skips conflicts you have already resolved; works while the resolved-output editor has focus. |
| Next unresolved conflict | `Shift-F3` | `Shift-F3` | Conflict resolver only. Skips conflicts you have already resolved; works while the resolved-output editor has focus. |
| First / last delta | `Cmd-Home` / `Cmd-End` | `Ctrl-Home` / `Ctrl-End` | Conflict resolver only. Not active while the resolved-output editor has focus, which keeps them for cursor movement. |
| Align selected lines manually | `Cmd-Y` | `Ctrl-Y` | Conflict resolver only (kdiff3 manual diff help). |
| Clear all manual alignments | `Cmd-Shift-Y` | `Ctrl-Shift-Y` | Conflict resolver only. |

Preview-mode note:
- Rendered markdown preview hides the raw diff navigation controls and ignores the raw-diff-only view toggles, whitespace toggle, and conflict navigation hotkeys until you return to source mode.
- The diff navigation keys above still work while a GitComet text input has focus, but search activation, `Escape`, view toggles, and staging `Space` remain tied to the active diff surface rather than text inputs.

## Context menu shortcuts

Context-menu keyboard behavior is the same on every platform:
- `Up` / `Down`: move the selection.
- `Enter`: activate the selected item, or the first enabled item if nothing is selected.
- `Escape`: close the menu.
- Single-letter or digit shortcuts shown inline activate the matching entry.

Additional note:
- Some menus also show clipboard-style shortcuts such as `Ctrl+C`; those reflect the underlying view shortcut rather than the menu's generic single-letter dispatcher.

## Focused diff window

These shortcuts apply in the standalone focused diff window opened for difftool-style flows.

| Action | macOS | Windows / Linux | Notes |
| --- | --- | --- | --- |
| Close the window | `Cmd-W`, `Ctrl-W`, `Escape`, `Q` | `Ctrl-W`, `Escape`, `Q` | `Ctrl-W` remains accepted on macOS as an extra alias. |
| Previous change | `F2`, `Shift-F7`, `Option-Up` | `F2`, `Shift-F7`, `Alt-Up` | One change block at a time, as in the main diff view. |
| Next change | `F3`, `F7`, `Option-Down` | `F3`, `F7`, `Alt-Down` | One change block at a time, as in the main diff view. |
