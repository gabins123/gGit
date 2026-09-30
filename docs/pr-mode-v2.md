# PR mode v2

Design: the "PR mode v2" and "Reviewer agents" sections of the gGit PR + AI UI canvas. This file
is the implementable copy of it. Paths are relative to `crates/gitcomet-ui-gpui/src/` unless
they start with `crates/` or `docs/`.

Panels are numbered as the app numbers them today: 1 Sidebar, 2 middle (History or diff),
3 Details, 0 bottom panel. Review mode: 1 Files, 2 Diff, 3 Your review.

## Rules that apply to every phase

- Repo rules in `AGENTS.md`: every new action has a key in the same change, handled in
  `view/panel_focus.rs` (`handle_panel_key` / `handle_pull_request_key`), shown in `key_hints`
  and `key_help`, inert while typing (`panel_keys_active`), documented in `docs/shortcuts.md`,
  and driven by a UI test under `view/panels/tests/shortcuts/` with `simulate_keystrokes`.
- Everything GitHub-side stays read-only unless it already exists. No new posting.
- GitHub text is untrusted. Markdown renders with remote images off (show them as links).
  Never render raw HTML from GitHub.
- Colors come from theme tokens (`theme.rs` `Colors`). No new theme-schema fields.
- Icon sizes go through the UI scale (`scaled_px` / `ui_scale.px`); `view/icons.rs` has a test
  that fails on raw `px()` icon sizes.
- One commit per phase on `feat/pr-mode-v2`, one-line message. Do not push.

## Phase 1: the PR itself replaces History

When `pull_request_details_active()` (`view/pull_requests.rs:307`) is true and review mode is not,
the middle panel (2) shows the selected pull request instead of the History graph.

Middle panel (2), header: state icon, title, `#number`, two tabs **Conversation** and **Comments**
(Comments shows its open-thread count), "Open on GitHub" (`o`).

- **Conversation**: the PR description rendered as markdown, then a timeline in time order:
  issue comments and reviews (today's `ConversationEntry` list, `github.rs:267`), review verdicts
  shown with their review glyph, "pushed N commits" events built from the PR's commit list
  (phase 2 fetches it; until then omit the event), and a checks summary line.
  - Render with `view/markdown_preview/document.rs` `parse_markdown` and
    `view/rows/markdown_document.rs` `render_markdown_document` (context with
    `remote_image_access` off and `view: None`). Replace the "plain text on purpose" handling in
    `github.rs:251` for this view only.
- **Comments**: every review thread (`ReviewThread`, fetched in `github.rs:1034`) grouped by file.
  Each thread: file, line, first comment, reply count, status. Open threads show; resolved and
  outdated are hidden until `V` toggles them. Resolved/outdated come from one GraphQL
  `reviewThreads { isResolved isOutdated comments(first:1){ databaseId } }` query per PR, matched
  to the REST threads by root comment id.
- Keys in panel 2: `j`/`k` next/previous entry or thread, `[`/`]` switch tabs (only the Sidebar
  uses `[`/`]` today), `enter` on a thread starts or resumes review mode at that thread's line,
  `V` resolved/outdated, `o` GitHub, `r` review (already PR-tab-wide).
- PR list (1): `enter` now focuses panel 2 instead of opening the whole-PR diff
  (`view/panel_focus.rs` `handle_pull_request_key`, the `"enter"` arm). Reading code is `r`.
- Details (3), `pull_request_details_view` (`view/panes/details.rs:2864`): remove the changed-files
  list and the conversation. Keep state, review decision, mergeable, checks, branch. Add
  **Reviewers**: each requested reviewer and each latest review with its glyph (add
  `reviewRequests` and `latestReviews` to the `gh pr view --json` fields, `github.rs:830`).
  Show "N files · +A −D" as one line where the file list was.
- History returns as soon as the PR tab is left or no PR is selected.

## Phase 2: commits in Details, and reading a commit range

- Fetch the PR's commits: add `commits` to the `gh pr view --json` fields (oid, messageHeadline,
  committedDate, authors). Newest first.
- Details (3) lists them under the facts: an **All commits** row (the default), then one row per
  commit (short sha, headline, age). If the viewer has a last review (`LastReview.commit_id`,
  `github.rs:1058`), draw a "your last review" divider after the newest reviewed commit and mark
  newer commits with a dot.
- Keys in Details: `j`/`k` move the commit cursor; `Shift+J`/`Shift+K` grow an unbroken range
  from it (as the Changes list does), replacing today's Details scroll; `esc` returns to All
  commits; `r` starts review mode on the selection; `space` still checks out the PR.
- Review mode scoped to a range reuses the `Shift+L` machinery (`view/review.rs:319`, `:471`,
  `:801`): base = parent of the oldest selected commit, head = newest selected commit. Comments
  stay limited to lines in the PR's own hunks (`pr_hunk_ranges`, `github.rs:1629`). The Files
  panel lists only files the range touches (`git diff --name-only base..head`) and says how many
  are hidden.
- Review-mode header shows the scope: "N of M commits  <old>..<new>".
- `Shift+C` in review mode opens a **Show changes from** picker: All changes, Since your last review
  (same as `L`), then the commits as checkboxes. `j`/`k`, `Shift+J`/`Shift+K` range, `enter` apply,
  `esc` close. A non-contiguous pick is not possible: selecting extends the run.

## Phase 3: PR row symbols and kind tag

Rows in the PR list (`view/panes/sidebar.rs:3712-3894`) and the Details header get one shape per
meaning. Words move to tooltips.

- State (start of row): open (success color, pull-request icon), draft (secondary color, dashed
  pull-request icon, title dimmed), merged (purple, merge icon; use the existing
  `historical_outline()` purples in `theme.rs:1830`), closed (danger, pull-request-closed icon).
  The list holds open PRs only, so merged/closed appear in Details.
- Review (right of the meta line): review required (warning, dotted ring), approved (success,
  ringed check), changes requested (danger, plus/minus page), commented only (secondary, bubble).
- Checks: passing check, failing cross, running open ring (spinner).
- You: a filled accent `@` pill on "Waiting for your review" rows and section header; a person
  icon before your own PRs; the pencil + pending count for your review in progress (exists).
- Kind tag from the title prefix: `feat`, `fix`, `refactor`, `docs`, and `deps` for
  `deps`/`chore`/`ci`/`build`. Strip the prefix (`feat:`, `fix(scope):`) from the shown title.
  No prefix, no tag.
- New SVG assets live beside the existing ones (`assets/icons/`); there is no pull-request icon
  yet. The `?` list for the PR tab gains a short legend.

## Phase 4: stacked pull requests

Match GitHub's stacked pull requests (public preview, July 2026):
https://docs.github.com/en/pull-requests/get-started/about-stacked-prs and
https://docs.github.com/en/pull-requests/how-tos/merge-and-close-pull-requests/merging-stacked-pull-requests

- **What a stack is**: PR B is stacked on PR A when B's base branch is A's head branch, in the
  same repository (cross-fork stacks don't exist on GitHub; skip `isCrossRepository` PRs).
  Where the repository has GitHub's native stacks, read the read-only GraphQL `stack` field on
  `PullRequest` (check the live schema with introspection; it is new) and prefer it; otherwise,
  or if the field is missing or errors, build stacks from the base-branch chain. A PR with two
  children makes a tree; show it as one.
- **PR list** (`view/panes/sidebar.rs`): a stack renders as a unit, bottom PR first, the PRs above
  indented under it with a connector and their position ("2/3"). The unit sits where its
  highest-ranked member would sit in the inbox order; each row keeps its own symbols.
- **Details (3)**: a **Stack** section above Commits, like GitHub's stack map: every PR in the
  stack from the base branch up, each with its state, review and checks glyphs, the current one
  highlighted. `<` and `>` move to the PR below and above (in the list, panel 2 and Details).
- **Review**: unchanged; a stacked PR's diff is already against its base (the parent's branch).
  The review-mode header says "stacked on #N".
- **Merge** (`Shift+M`, `PopoverKind::MergePullRequest`), following GitHub's rules:
  - Native stack: merging a PR merges it and every unmerged PR below it, bottom-up, in one
    operation, through GitHub's asynchronous stack merge API (poll until it finishes). The dialog
    lists which PRs merge and which stay open. It refuses locally, naming the PR, when any PR
    below isn't approved or has failing checks; a non-linear stack is left to GitHub's own
    refusal at merge time (`merge_stack`'s `failed` outcome), shown the same way, never
    re-derived here. Merge commit, squash and rebase all stay available. Afterwards refresh:
    GitHub rebases the next PR onto the stack's base.
  - Base-branch chain only (no native stack): merge works as today, into the PR's own base. The
    dialog says where that is ("into feat/a, not dev"); for the bottom PR it notes that deleting
    the branch (`Alt+D`) makes GitHub retarget the next PR to the base.
  - No command that merges a whole stack by waiting on checks between layers.

## Phase 5: how changed files appear

### Markdown files in PR diffs

- `diff_target_rendered_preview_kind` (`view/mod_helpers/mod.rs`) only accepts `WorkingTree` and
  `Commit`. Accept `CommitRange { path: Some(_), .. }` too, so the `enter` diff, review mode and
  commit ranges get the Preview / Text switch. Check `main_diff_rendered_preview_toggle_kind` and
  the image base dir (`view/panes/main/preview.rs`) for the range case.
- The Preview / Text switch (`view/panels/main/diff_view.rs`, `RenderedPreviewKind` toggle) is
  mouse-only. Add `Alt+P` in the diff view to flip it, everywhere the switch shows.
- Review mode in Preview: `j`/`k` move by rendered block, `}`/`{` jump between changed blocks,
  `space`, `]`/`[`, `t` keep working. `c` switches to Text with the cursor on the block's first
  source line (rows carry `source_line_range`).

### Image diffs

- The side-by-side image diff exists (`view/panels/main/diff.rs` ~150,
  `view/panes/main/diff_cache/image_cache.rs`) and already loads for `CommitRange` targets
  (`crates/gitcomet-state/src/store/reducer/util.rs` `selected_diff_load_plan`). Confirm it shows
  in review mode and the `enter` diff; fix it if not.
- Add GitHub's other two modes: **Swipe** (old and new split by a divider) and **Onion skin**
  (new over old at an adjustable opacity). Side by side stays the default. `Alt+V` cycles the
  modes; `,` and `.` move the divider or the opacity by 10%. Everywhere the image diff shows
  (working tree, commit, PR).
- Show each side's pixel size and file size. An added or deleted image shows its one side.

### Generated files

- Match GitHub's `linguist-generated`
  (https://docs.github.com/en/repositories/working-with-files/managing-files/customizing-how-changed-files-appear-on-github):
  a path is generated when `.gitattributes` at the PR head sets `linguist-generated`, or when it
  is on a short built-in list modeled on GitHub Linguist (lockfiles: `Cargo.lock`,
  `package-lock.json`, `yarn.lock`, `pnpm-lock.yaml`, `poetry.lock`, `Gemfile.lock`,
  `composer.lock`, `go.sum`; minified `*.min.js`, `*.min.css`). `-linguist-generated` or
  `linguist-generated=false` un-marks a path. Read attributes with gix, not a subprocess.
- Review mode Files list: generated files are hidden like viewed files, with a
  "N generated files hidden" line; `Shift+G` shows them. They don't count toward viewed progress.
  Opening one shows "Generated file" and `enter` loads its diff.
- Details size line: "6 files (2 generated)". Reviewer agents (Phase 6) leave generated files out
  of their material unless the scope is exactly that file.

## Phase 6: reviewer agents

- **.reviewer loader**: read `.reviewer/` from where the PR leaves the default branch (the merge
  base of its base commit and the default branch's tip, `git show <merge_base>:<path>`), never
  from the PR head. `README.md` is always sent; `checklist.md` is one rule per line;
  `areas/*.md` carry front matter `paths: [globs]` and are sent only when the scope touches a
  matching path; `agents/*.md` carry front matter `title`, `key` (1-9), `scope`
  (`lines|file|commits|pr`), optional `paths`, and their body is the agent's instructions.
  Missing or empty folder: the built-in reviewer, and the menu says so. If the PR diff touches
  `.reviewer/`, the result header says so.
- Codex keeps running in its empty temp dir (`codex.rs:118-122`). `.reviewer` text goes into the
  instructions part of the prompt; PR material stays inside the untrusted data fences
  (`codex.rs:96`).
- **`i` menu on a PR** (PR tab or review mode; elsewhere the menu is unchanged): a scope row
  (Lines / File / Commits / Whole PR) starting at what the user is on (selection, else file,
  else thread, else whole PR); `tab` widens it. Chips list the `.reviewer` files that will be sent.
  Actions: `b` brief me (summary, reading order, spots to look at), `e` explain this, `h` thread
  (summarize and draft a reply — checking later commits for a fix isn't available yet),
  `r` review against the rules
  (a verdict per checklist line; findings become the existing Codex suggestions, `a` adopt /
  `x` drop, `view/review.rs:2546`), `t` test gaps, `v` description vs code, `s` draft my review
  summary (fills the Submit dialog like `Alt+G`), `q` ask, `1`-`9` agents from `.reviewer/agents`.
- Results go to the Codex panel (0) as today. `b` rows (files, spots) jump there with `enter`.
- The 100-file / 20k-line refusal (`github.rs:22`) applies to the material actually sent, so a
  narrower scope works on a large PR.

## Out of scope

Posting comments or replies from the PR view, resolving threads, closed/merged PRs in the list,
labels, running agents without a keypress, risk scores. For stacks: creating, extending or
dissolving them (use `gh stack` or GitHub), restacking locally, cross-fork stacks.
