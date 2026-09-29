//! `.reviewer/` folder support for Codex-based PR review (PR mode v2, phase
//! 6): parsing what a repository authors under `.reviewer/`, and the small
//! JSON shapes Codex is asked to answer in for the checklist review and the
//! "brief me" summary.
//!
//! Everything here is read from the pull request's **base commit**, never
//! its head or the worktree: a PR cannot rewrite the rules it is reviewed
//! against. The text this module produces is trusted (it goes in a Codex
//! prompt's instructions); PR material (diffs, descriptions, threads) stays
//! untrusted and out of this module's business (`codex.rs` fences it).

use std::collections::BTreeSet;
use std::path::Path;

/// Total `.reviewer` text sent to Codex in one request is capped here, so a
/// large folder can't blow the prompt budget on its own; `codex.rs` still
/// caps the whole prompt (instructions + material) on top of this.
pub(crate) const REVIEWER_TEXT_CAP: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AgentScope {
    Lines,
    File,
    Commits,
    Pr,
}

impl AgentScope {
    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "lines" => Some(Self::Lines),
            "file" => Some(Self::File),
            "commits" => Some(Self::Commits),
            "pr" => Some(Self::Pr),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct AreaDoc {
    /// `"areas/rust.md"`.
    pub(crate) name: String,
    pub(crate) paths: Vec<String>,
    pub(crate) body: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentDoc {
    /// `"agents/security.md"`.
    pub(crate) name: String,
    pub(crate) title: String,
    /// `1`-`9`; a malformed or missing key drops the agent (nothing to bind
    /// it to a menu row).
    pub(crate) key: char,
    pub(crate) scope: Option<AgentScope>,
    pub(crate) paths: Vec<String>,
    pub(crate) body: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ReviewerConfig {
    pub(crate) readme: Option<String>,
    /// One rule per `- ` line of `checklist.md`.
    pub(crate) checklist: Vec<String>,
    pub(crate) areas: Vec<AreaDoc>,
    pub(crate) agents: Vec<AgentDoc>,
    /// The 64 KB cap cut something.
    pub(crate) truncated: bool,
}

impl ReviewerConfig {
    /// True when `.reviewer/` is missing or empty: the menu falls back to
    /// the built-in reviewer.
    pub(crate) fn is_builtin(&self) -> bool {
        self.readme.is_none()
            && self.checklist.is_empty()
            && self.areas.is_empty()
            && self.agents.is_empty()
    }

    /// The `.reviewer` files whose content applies to `paths` (an empty
    /// slice means "the whole PR": every area applies). Areas are filtered
    /// by their `paths` globs; README and the checklist always apply. What
    /// the menu's chips list and what `instructions_text` actually sends.
    pub(crate) fn files_for(&self, paths: &[String]) -> Vec<&str> {
        let mut names = Vec::new();
        if self.readme.is_some() {
            names.push("README.md");
        }
        if !self.checklist.is_empty() {
            names.push("checklist.md");
        }
        for area in &self.areas {
            if paths.is_empty() || paths.iter().any(|path| area_matches(&area.paths, path)) {
                names.push(area.name.as_str());
            }
        }
        names
    }

    /// The trusted instructions text built from README, checklist and the
    /// areas that apply to `paths`. Agent bodies are added by the caller
    /// (only one agent ever runs per request).
    pub(crate) fn instructions_text(&self, paths: &[String]) -> String {
        let mut parts = Vec::new();
        if let Some(readme) = &self.readme {
            parts.push(format!("From .reviewer/README.md:\n{readme}"));
        }
        if !self.checklist.is_empty() {
            let rules: Vec<String> = self
                .checklist
                .iter()
                .enumerate()
                .map(|(ix, rule)| format!("{}. {rule}", ix + 1))
                .collect();
            parts.push(format!(
                "From .reviewer/checklist.md, the review checklist:\n{}",
                rules.join("\n")
            ));
        }
        for area in &self.areas {
            if paths.is_empty() || paths.iter().any(|path| area_matches(&area.paths, path)) {
                parts.push(format!("From .reviewer/{}:\n{}", area.name, area.body));
            }
        }
        parts.join("\n\n")
    }
}

/// The pull request's own changed files that fall under `.reviewer/` — "This
/// PR changes .reviewer/<file>". Computed fresh at dispatch from the PR's
/// *current* full changed-file list, never cached alongside a loaded
/// [`ReviewerConfig`]: that cache is keyed by base commit, and two pull
/// requests can share one while touching different files.
pub(crate) fn touched_reviewer_files(pr_changed_files: &[String]) -> Vec<String> {
    pr_changed_files
        .iter()
        .filter(|path| path.starts_with(".reviewer/"))
        .cloned()
        .collect()
}

/// Used for `r` (review against the rules) when no `.reviewer/checklist.md`
/// exists, so its JSON prompt's "against the checklist above" always has one.
pub(crate) const BUILTIN_CHECKLIST: &[&str] = &[
    "Tests cover the change",
    "No obvious bugs or missed edge cases",
    "Errors are handled, not swallowed or unwrapped away",
    "No secrets, credentials or debug output left in",
    "Naming and structure match the surrounding code",
];

/// The built-in checklist, formatted the same way
/// [`ReviewerConfig::instructions_text`] formats a real `checklist.md`.
pub(crate) fn builtin_checklist_text() -> String {
    let rules: Vec<String> = BUILTIN_CHECKLIST
        .iter()
        .enumerate()
        .map(|(ix, rule)| format!("{}. {rule}", ix + 1))
        .collect();
    format!(
        "No .reviewer/checklist.md was found; here is a general checklist:\n{}",
        rules.join("\n")
    )
}

/// `require_literal_separator` so a single-component glob like `*.toml`
/// matches only a top-level `Cargo.toml`, not `crates/foo/Cargo.toml` — `**`
/// still crosses `/` when that's genuinely wanted.
pub(crate) fn area_matches(globs: &[String], path: &str) -> bool {
    let options = glob::MatchOptions {
        require_literal_separator: true,
        ..Default::default()
    };
    globs.iter().any(|glob| {
        glob::Pattern::new(glob)
            .map(|pattern| pattern.matches_with(path, options))
            .unwrap_or(false)
    })
}

/// One rule per non-empty `- ` line; anything else in the file is ignored.
pub(crate) fn parse_checklist(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("- "))
        .map(str::trim)
        .filter(|rule| !rule.is_empty())
        .map(str::to_string)
        .collect()
}

/// Splits `---\nkey: value\n...\n---\n<body>` front matter off the top of a
/// `.reviewer` file. Deliberately not a YAML parser: only scalar and
/// one-line-list values for the handful of keys these files use.
fn split_front_matter(text: &str) -> (Vec<(String, String)>, &str) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = text
        .strip_prefix("---\r\n")
        .or_else(|| text.strip_prefix("---\n"))
    else {
        return (Vec::new(), text);
    };
    let Some(end) = rest.find("\n---") else {
        return (Vec::new(), text);
    };
    let header = &rest[..end];
    let after = &rest[end + "\n---".len()..];
    let body = after
        .strip_prefix("\r\n")
        .or_else(|| after.strip_prefix('\n'))
        .unwrap_or(after);
    let fields = header
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let (key, value) = line.split_once(':')?;
            Some((key.trim().to_string(), value.trim().to_string()))
        })
        .collect();
    (fields, body)
}

/// `[a, "b/**", 'c']` (or a single bare value) to a list of globs.
fn parse_path_list(value: &str) -> Vec<String> {
    let inner = value
        .trim()
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(value.trim());
    inner
        .split(',')
        .map(|item| item.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

fn field<'a>(fields: &'a [(String, String)], key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

fn parse_area(name: &str, text: &str) -> AreaDoc {
    let (fields, body) = split_front_matter(text);
    let paths = field(&fields, "paths").map(parse_path_list).unwrap_or_default();
    AreaDoc {
        name: name.to_string(),
        paths,
        body: body.trim().to_string(),
    }
}

/// `None` when the file has no usable `key` (1-9): there is nothing to bind
/// it to a menu row, so it's dropped rather than shown unreachable.
fn parse_agent(name: &str, text: &str) -> Option<AgentDoc> {
    let (fields, body) = split_front_matter(text);
    let key = field(&fields, "key")?
        .trim()
        .chars()
        .next()
        .filter(|c| c.is_ascii_digit() && *c != '0')?;
    let title = field(&fields, "title")
        .map(str::to_string)
        .unwrap_or_else(|| {
            name.trim_start_matches("agents/")
                .trim_end_matches(".md")
                .to_string()
        });
    let scope = field(&fields, "scope").and_then(AgentScope::parse);
    let paths = field(&fields, "paths").map(parse_path_list).unwrap_or_default();
    Some(AgentDoc {
        name: name.to_string(),
        title,
        key,
        scope,
        paths,
        body: body.trim().to_string(),
    })
}

/// Where `.reviewer/` bytes come from: git at a trusted commit in production
/// (`GitReviewerSource`), an in-memory map in tests. Both fallible: a
/// `list()` that can't run (the commit isn't fetched locally yet, say) must
/// say so rather than silently reporting an empty folder, which would look
/// exactly like "no `.reviewer/`" and get cached as the built-in reviewer.
pub(crate) trait ReviewerSource {
    /// Every path under `.reviewer/`, relative to it (`"README.md"`,
    /// `"areas/rust.md"`), in any order.
    fn list(&self) -> Result<Vec<String>, String>;
    fn read(&self, path: &str) -> Result<String, String>;
}

/// Keeps `.reviewer` text within [`REVIEWER_TEXT_CAP`] across every file,
/// truncating (and flagging it) rather than dropping a file outright once
/// the budget runs out.
struct Budget {
    remaining: usize,
    truncated: bool,
}

impl Budget {
    fn take(&mut self, mut text: String) -> Option<String> {
        if text.is_empty() {
            return None;
        }
        if self.remaining == 0 {
            self.truncated = true;
            return None;
        }
        if crate::codex::truncate_to(&mut text, self.remaining) {
            self.truncated = true;
        }
        self.remaining -= text.len();
        Some(text)
    }
}

/// Reads and parses `.reviewer/` from `source`. `Err` only when `source`
/// itself couldn't be listed (its commit isn't available, a git failure,
/// …) — never cache that as "no `.reviewer/` folder". An individual file
/// that lists but fails to read is skipped rather than failing the whole
/// load: rarer, and not the folder-vs-error confusion this guards against.
pub(crate) fn load_reviewer_config(source: &dyn ReviewerSource) -> Result<ReviewerConfig, String> {
    let files = source.list()?;
    let mut budget = Budget {
        remaining: REVIEWER_TEXT_CAP,
        truncated: false,
    };

    let readme = files
        .iter()
        .find(|path| path.as_str() == "README.md")
        .and_then(|path| source.read(path).ok())
        .and_then(|text| budget.take(text));

    let checklist = files
        .iter()
        .find(|path| path.as_str() == "checklist.md")
        .and_then(|path| source.read(path).ok())
        .and_then(|text| budget.take(text))
        .map(|text| parse_checklist(&text))
        .unwrap_or_default();

    let mut areas: Vec<AreaDoc> = files
        .iter()
        .filter(|path| path.starts_with("areas/") && path.ends_with(".md"))
        .filter_map(|path| Some((path, source.read(path).ok()?)))
        .map(|(path, text)| parse_area(path, &text))
        .filter_map(|mut area| {
            area.body = budget.take(area.body)?;
            Some(area)
        })
        .collect();
    areas.sort_by(|a, b| a.name.cmp(&b.name));

    let mut agents: Vec<AgentDoc> = files
        .iter()
        .filter(|path| path.starts_with("agents/") && path.ends_with(".md"))
        .filter_map(|path| Some((path, source.read(path).ok()?)))
        .filter_map(|(path, text)| parse_agent(path, &text))
        .filter_map(|mut agent| {
            agent.body = budget.take(agent.body)?;
            Some(agent)
        })
        .collect();
    agents.sort_by_key(|agent| agent.key);
    agents.dedup_by_key(|agent| agent.key);

    Ok(ReviewerConfig {
        readme,
        checklist,
        areas,
        agents,
        truncated: budget.truncated,
    })
}

/// `.reviewer/` read from git at one commit, via subprocess (`git ls-tree`,
/// `git show`); never the working tree, and never the PR head as such (the
/// caller is responsible for passing a commit it trusts — see
/// `reviewer_menu::reviewer_trusted_base_commit`).
pub(crate) struct GitReviewerSource<'a> {
    pub(crate) workdir: &'a Path,
    pub(crate) base_oid: &'a str,
}

impl ReviewerSource for GitReviewerSource<'_> {
    fn list(&self) -> Result<Vec<String>, String> {
        // `-z`: NUL-separated, so a non-ASCII or otherwise unusual path
        // comes back raw instead of `core.quotepath`-quoted (and possibly
        // silently dropped by a naive line-based parse).
        let output = gitcomet_core::process::git_command()
            .current_dir(self.workdir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .args([
                "ls-tree", "-r", "-z", "--name-only", self.base_oid, "--", ".reviewer",
            ])
            .output()
            .map_err(|err| err.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        Ok(output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .filter_map(|path| std::str::from_utf8(path).ok())
            .filter_map(|path| path.strip_prefix(".reviewer/"))
            .map(|path| path.replace('\\', "/"))
            .collect())
    }

    fn read(&self, path: &str) -> Result<String, String> {
        let spec = format!("{}:.reviewer/{path}", self.base_oid);
        let output = gitcomet_core::process::git_command()
            .current_dir(self.workdir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["show", "--no-color", &spec])
            .output()
            .map_err(|err| err.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// Finds the hunk(s) of one file's unified diff (`file_diff`, e.g. `git diff
/// base..head -- path`) that cover new-file lines `lo..=hi`, kept with the
/// diff's own file header for context. `None` when no hunk overlaps.
pub(crate) fn extract_hunk_for_lines(file_diff: &str, lo: u32, hi: u32) -> Option<String> {
    let lines: Vec<&str> = file_diff.lines().collect();
    let mut header_end = 0usize;
    while header_end < lines.len() && !lines[header_end].starts_with("@@") {
        header_end += 1;
    }
    let preamble = &lines[..header_end];
    let mut ix = header_end;
    while ix < lines.len() {
        if !lines[ix].starts_with("@@") {
            ix += 1;
            continue;
        }
        let Some((new_start, new_len)) = parse_hunk_new_range(lines[ix]) else {
            ix += 1;
            continue;
        };
        let hunk_start = ix;
        ix += 1;
        while ix < lines.len() && !lines[ix].starts_with("@@") {
            ix += 1;
        }
        let new_end = new_start + new_len.saturating_sub(1);
        if new_start <= hi && lo <= new_end {
            let mut out = preamble.join("\n");
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&lines[hunk_start..ix].join("\n"));
            return Some(out);
        }
    }
    None
}

/// Parses `@@ -old_start,old_len +new_start,new_len @@ context` into
/// `(new_start, new_len)`. `new_len` defaults to 1 when omitted, as `diff`
/// itself does for a one-line hunk.
fn parse_hunk_new_range(header: &str) -> Option<(u32, u32)> {
    let plus = header.split_whitespace().find(|part| part.starts_with('+'))?;
    let spec = plus.trim_start_matches('+');
    let mut parts = spec.splitn(2, ',');
    let start: u32 = parts.next()?.parse().ok()?;
    let len: u32 = match parts.next() {
        Some(len) => len.parse().ok()?,
        None => 1,
    };
    Some((start, len.max(1)))
}

/// Drops the unified-diff sections (each starting at its own `diff --git`
/// line) of paths in `generated`, except `keep` — a reviewer scope's own
/// exactly-this-file exemption ("leave generated files out of their
/// material unless the scope is exactly that file").
pub(crate) fn filter_generated_sections(
    diff: &str,
    generated: &BTreeSet<String>,
    keep: Option<&str>,
) -> String {
    if generated.is_empty() {
        return diff.to_string();
    }
    let mut out = String::new();
    let mut skip = false;
    for line in diff.split_inclusive('\n') {
        if let Some(header) = line.strip_prefix("diff --git ") {
            skip = diff_git_path(header)
                .is_some_and(|path| generated.contains(&path) && Some(path.as_str()) != keep);
        }
        if !skip {
            out.push_str(line);
        }
    }
    out
}

/// `a/<path> b/<path>` (a `diff --git` header's tail) to `<path>`, the `b/`
/// (new-file) side. `git diff` (without `-z`) C-quotes a path with
/// non-ASCII or otherwise unusual bytes (`"caf\303\251.rs"`); unquoted here
/// so such a path is still recognized as generated, not silently kept.
fn diff_git_path(header: &str) -> Option<String> {
    let header = header.trim_end_matches(['\n', '\r']);
    let ix = header.find(" b/")?;
    Some(unquote_git_path(&header[ix + " b/".len()..]))
}

/// Git's C-style path quoting: a leading/trailing `"` wraps `\\`, `\"` and
/// `\NNN` octal-byte escapes. A path with none of that (the common case) is
/// returned as is.
fn unquote_git_path(raw: &str) -> String {
    let Some(inner) = raw.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')) else {
        return raw.to_string();
    };
    let bytes = inner.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut ix = 0;
    while ix < bytes.len() {
        if bytes[ix] != b'\\' || ix + 1 >= bytes.len() {
            out.push(bytes[ix]);
            ix += 1;
            continue;
        }
        match bytes[ix + 1] {
            b'\\' => {
                out.push(b'\\');
                ix += 2;
            }
            b'"' => {
                out.push(b'"');
                ix += 2;
            }
            b't' => {
                out.push(b'\t');
                ix += 2;
            }
            b'n' => {
                out.push(b'\n');
                ix += 2;
            }
            first @ b'0'..=b'7' => {
                let mut value = u32::from(first - b'0');
                let mut consumed = 1;
                while consumed < 3
                    && ix + 1 + consumed < bytes.len()
                    && (b'0'..=b'7').contains(&bytes[ix + 1 + consumed])
                {
                    value = value * 8 + u32::from(bytes[ix + 1 + consumed] - b'0');
                    consumed += 1;
                }
                out.push(value as u8);
                ix += 1 + consumed;
            }
            other => {
                out.push(other);
                ix += 2;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether a diff's own file and changed-line counts pass the same limit
/// [`crate::github::PullRequestDetail::too_large_for_codex`] applies to a
/// whole pull request, applied instead to one reviewer scope's assembled
/// material — so a narrow scope still works on an otherwise huge PR.
pub(crate) fn diff_too_large(diff: &str) -> bool {
    let mut files = 0u64;
    let mut changed = 0u64;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            files += 1;
        } else if (line.starts_with('+') && !line.starts_with("+++"))
            || (line.starts_with('-') && !line.starts_with("---"))
        {
            changed += 1;
        }
    }
    files > crate::github::MAX_CODEX_FILES || changed > crate::github::MAX_CODEX_CHANGED_LINES
}

/// One row of a `b` (brief me) answer: a spot to look at, with an optional
/// anchor in the diff to jump to.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub(crate) struct BriefRow {
    pub(crate) text: String,
    #[serde(default)]
    pub(crate) path: Option<String>,
    #[serde(default)]
    pub(crate) line: Option<u32>,
}

pub(crate) const BRIEF_JSON_INSTRUCTIONS: &str = "Reply with only a JSON array, no prose and no code fences. Each item is one point, in reading order: {\"text\": the point, a sentence or two, \"path\": optional, the file path as in the diff, \"line\": optional, a line number in the new file}. Put the most important spots to look at first.";

/// One checklist rule's verdict, from an `r` (review against the rules)
/// answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VerdictKind {
    Pass,
    Flag,
    NotApplicable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChecklistVerdict {
    pub(crate) rule: String,
    pub(crate) verdict: VerdictKind,
    pub(crate) note: String,
}

/// A finding an `r` answer attaches to a line, shaped exactly like
/// `review::SUGGESTION_INSTRUCTIONS`'s items so it can become a
/// [`crate::github::ReviewComment`] suggestion the same way.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct RuleFinding {
    pub(crate) path: String,
    pub(crate) line: u32,
    pub(crate) side: String,
    pub(crate) body: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RuleReviewResult {
    pub(crate) verdicts: Vec<ChecklistVerdict>,
    pub(crate) findings: Vec<RuleFinding>,
}

pub(crate) const RULE_REVIEW_JSON_INSTRUCTIONS: &str = "Review the material below against the checklist above, one verdict per checklist rule, in the same order. Reply with only JSON, no prose and no code fences: {\"verdicts\": [{\"rule\": the checklist line's text, \"verdict\": \"pass\", \"flag\" or \"na\", \"note\": a short reason, required for \"flag\" or \"na\"}], \"findings\": [{\"path\": the file path as in the diff, \"line\": a line number, \"side\": \"RIGHT\" for an added or unchanged line (its number in the new file) or \"LEFT\" for a removed line (its number in the old file), \"body\": the comment}]}. Only comment on lines inside the diff's hunks. At most 15 findings, the important issues first.";

/// Strips a wrapping ```json fence, if Codex added one despite being asked
/// not to.
fn strip_code_fence(text: &str) -> &str {
    let text = text.trim();
    let text = text
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .map(str::trim_start)
        .unwrap_or(text);
    text.strip_suffix("```").map(str::trim_end).unwrap_or(text)
}

/// Parses a `b` answer defensively: malformed JSON or an answer that isn't
/// an array of the expected shape yields an empty list, so the caller falls
/// back to showing the raw text.
pub(crate) fn parse_brief_json(text: &str) -> Vec<BriefRow> {
    serde_json::from_str(strip_code_fence(text)).unwrap_or_default()
}

#[derive(serde::Deserialize)]
struct RawVerdict {
    rule: String,
    verdict: String,
    #[serde(default)]
    note: String,
}

#[derive(Default, serde::Deserialize)]
struct RawRuleReview {
    #[serde(default)]
    verdicts: Vec<RawVerdict>,
    #[serde(default)]
    findings: Vec<RuleFinding>,
}

/// Parses an `r` answer defensively: an unknown `verdict` string is dropped
/// rather than failing the whole answer, and malformed JSON is `None` — told
/// apart from "parsed fine with nothing to say", so a caller never mistakes
/// a parse failure for zero findings and clears suggestions that were never
/// actually re-sent.
pub(crate) fn parse_rule_review_json(text: &str) -> Option<RuleReviewResult> {
    let raw = serde_json::from_str::<RawRuleReview>(strip_code_fence(text)).ok()?;
    let verdicts = raw
        .verdicts
        .into_iter()
        .filter_map(|raw| {
            let verdict = match raw.verdict.trim().to_ascii_lowercase().as_str() {
                "pass" => VerdictKind::Pass,
                "flag" => VerdictKind::Flag,
                "na" | "n/a" | "not_applicable" => VerdictKind::NotApplicable,
                _ => return None,
            };
            Some(ChecklistVerdict {
                rule: raw.rule,
                verdict,
                note: raw.note,
            })
        })
        .collect();
    Some(RuleReviewResult {
        verdicts,
        findings: raw.findings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct MapSource(BTreeMap<&'static str, &'static str>);

    impl ReviewerSource for MapSource {
        fn list(&self) -> Result<Vec<String>, String> {
            Ok(self.0.keys().map(|key| key.to_string()).collect())
        }
        fn read(&self, path: &str) -> Result<String, String> {
            self.0
                .get(path)
                .map(|text| text.to_string())
                .ok_or_else(|| "not found".to_string())
        }
    }

    /// A source whose `list()` fails outright — the "base commit isn't
    /// fetched locally yet" case a missing/empty folder must never be
    /// confused with.
    struct FailingSource;

    impl ReviewerSource for FailingSource {
        fn list(&self) -> Result<Vec<String>, String> {
            Err("fatal: bad object base_oid".to_string())
        }
        fn read(&self, _path: &str) -> Result<String, String> {
            unreachable!("list() fails first")
        }
    }

    #[test]
    fn missing_folder_is_the_builtin_reviewer() {
        let config = load_reviewer_config(&MapSource(BTreeMap::new())).expect("empty is ok");
        assert!(config.is_builtin());
    }

    #[test]
    fn a_source_that_cant_be_listed_is_an_error_not_a_missing_folder() {
        let err = load_reviewer_config(&FailingSource).expect_err("list() failed");
        assert!(err.contains("base_oid"));
    }

    #[test]
    fn checklist_takes_one_rule_per_dash_line() {
        let text = "Intro text, not a rule.\n- Tests cover the change\n- No secrets in diffs\n\n- \nplain line";
        assert_eq!(
            parse_checklist(text),
            vec!["Tests cover the change", "No secrets in diffs"]
        );
    }

    #[test]
    fn area_front_matter_paths_gate_a_touched_scope() {
        let mut files = BTreeMap::new();
        files.insert(
            "areas/rust.md",
            "---\npaths: [\"crates/**/*.rs\", *.toml]\n---\nMind ownership and error types.",
        );
        let config = load_reviewer_config(&MapSource(files)).expect("valid source");
        assert_eq!(config.areas.len(), 1);
        assert_eq!(config.areas[0].paths, vec!["crates/**/*.rs", "*.toml"]);
        assert_eq!(config.areas[0].body, "Mind ownership and error types.");
        assert!(area_matches(&config.areas[0].paths, "crates/foo/src/lib.rs"));
        assert!(!area_matches(&config.areas[0].paths, "docs/readme.md"));
        assert_eq!(
            config.files_for(&["crates/foo/src/lib.rs".to_string()]),
            vec!["areas/rust.md"]
        );
        assert!(config.files_for(&["docs/readme.md".to_string()]).is_empty());
        // Whole-PR scope (no path filter) includes every area.
        assert_eq!(config.files_for(&[]), vec!["areas/rust.md"]);
    }

    #[test]
    fn a_single_component_glob_does_not_cross_directories() {
        // `*.toml` (no `require_literal_separator` override) would match a
        // nested Cargo.toml too; it must not once real path separators are
        // treated as literal.
        assert!(area_matches(&["*.toml".to_string()], "Cargo.toml"));
        assert!(!area_matches(&["*.toml".to_string()], "crates/foo/Cargo.toml"));
        // `**` is still allowed to cross directories.
        assert!(area_matches(&["**/*.toml".to_string()], "crates/foo/Cargo.toml"));
    }

    #[test]
    fn agent_front_matter_is_parsed_and_missing_key_drops_it() {
        let mut files = BTreeMap::new();
        files.insert(
            "agents/security.md",
            "---\ntitle: Security\nkey: 3\nscope: pr\npaths: [\"**/*.rs\"]\n---\nLook for injection.",
        );
        files.insert("agents/no_key.md", "---\ntitle: Nothing\n---\nDropped.");
        let config = load_reviewer_config(&MapSource(files)).expect("valid source");
        assert_eq!(config.agents.len(), 1);
        let agent = &config.agents[0];
        assert_eq!(agent.title, "Security");
        assert_eq!(agent.key, '3');
        assert_eq!(agent.scope, Some(AgentScope::Pr));
        assert_eq!(agent.paths, vec!["**/*.rs"]);
        assert_eq!(agent.body, "Look for injection.");
    }

    #[test]
    fn pr_touching_reviewer_folder_is_reported() {
        let touched = touched_reviewer_files(&[
            "src/lib.rs".to_string(),
            ".reviewer/checklist.md".to_string(),
        ]);
        assert_eq!(touched, vec![".reviewer/checklist.md"]);
    }

    #[test]
    fn total_reviewer_text_is_capped_and_flagged() {
        let mut files = BTreeMap::new();
        let big: String = "x".repeat(REVIEWER_TEXT_CAP);
        let leaked: &'static str =
            Box::leak(format!("---\npaths: [a]\n---\n{big}").into_boxed_str());
        files.insert("README.md", &leaked[..20]);
        files.insert("areas/big.md", leaked);
        let config = load_reviewer_config(&MapSource(files)).expect("valid source");
        assert!(config.truncated);
        // The README (read first) still fits, unclipped.
        assert_eq!(config.readme.as_deref(), Some(&leaked[..20]));
    }

    #[test]
    fn hunk_extraction_finds_the_overlapping_hunk_only() {
        let diff = "diff --git a/f.rs b/f.rs\n--- a/f.rs\n+++ b/f.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n context\n@@ -10,1 +10,3 @@\n context\n+added one\n+added two\n";
        let hunk = extract_hunk_for_lines(diff, 11, 11).expect("line 11 is in the second hunk");
        assert!(hunk.contains("@@ -10,1 +10,3 @@"));
        assert!(!hunk.contains("@@ -1,2 +1,2 @@"));
        assert!(hunk.contains("diff --git a/f.rs b/f.rs"));
        assert!(extract_hunk_for_lines(diff, 50, 60).is_none());
    }

    #[test]
    fn brief_json_parses_and_falls_back_to_empty_on_garbage() {
        let rows = parse_brief_json(
            "```json\n[{\"text\": \"Start here\", \"path\": \"a.rs\", \"line\": 5}, {\"text\": \"Then this\"}]\n```",
        );
        assert_eq!(
            rows,
            vec![
                BriefRow {
                    text: "Start here".to_string(),
                    path: Some("a.rs".to_string()),
                    line: Some(5),
                },
                BriefRow {
                    text: "Then this".to_string(),
                    path: None,
                    line: None,
                },
            ]
        );
        assert!(parse_brief_json("not json at all").is_empty());
        assert!(parse_brief_json("{\"not\": \"an array\"}").is_empty());
    }

    #[test]
    fn rule_review_json_drops_unknown_verdicts_and_keeps_findings() {
        let result = parse_rule_review_json(
            r#"{"verdicts": [
                {"rule": "Tests exist", "verdict": "pass"},
                {"rule": "No TODOs", "verdict": "flag", "note": "left one in"},
                {"rule": "Weird", "verdict": "bogus"}
            ], "findings": [
                {"path": "a.rs", "line": 3, "side": "RIGHT", "body": "check this"}
            ]}"#,
        )
        .expect("valid JSON");
        assert_eq!(result.verdicts.len(), 2);
        assert_eq!(result.verdicts[0].verdict, VerdictKind::Pass);
        assert_eq!(result.verdicts[1].verdict, VerdictKind::Flag);
        assert_eq!(result.verdicts[1].note, "left one in");
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].path, "a.rs");
    }

    /// Malformed JSON is `None`, never `Some(RuleReviewResult::default())`:
    /// a caller that only checked `.findings.is_empty()` would otherwise
    /// mistake "couldn't read this" for "genuinely nothing to say" and wipe
    /// suggestions that were never actually re-sent.
    #[test]
    fn malformed_rule_review_json_is_none_not_an_empty_result() {
        assert_eq!(parse_rule_review_json("this is not json"), None);
    }

    #[test]
    fn generated_sections_are_dropped_except_the_kept_one() {
        let diff = "diff --git a/gen.lock b/gen.lock\n--- a/gen.lock\n+++ b/gen.lock\n@@ -1 +1 @@\n-x\n+y\ndiff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-a\n+b\n";
        let generated: BTreeSet<String> = ["gen.lock".to_string()].into_iter().collect();
        let filtered = filter_generated_sections(diff, &generated, None);
        assert!(!filtered.contains("gen.lock"));
        assert!(filtered.contains("src/lib.rs"));
        // The exactly-this-file exemption.
        let kept = filter_generated_sections(diff, &generated, Some("gen.lock"));
        assert!(kept.contains("gen.lock"));
        assert!(kept.contains("src/lib.rs"));
        // No generated files at all: unchanged (and no allocation-shape surprises).
        assert_eq!(filter_generated_sections(diff, &BTreeSet::new(), None), diff);
    }

    #[test]
    fn diff_size_limit_counts_files_and_changed_lines() {
        let small = "diff --git a/f.rs b/f.rs\n--- a/f.rs\n+++ b/f.rs\n@@ -1 +1 @@\n-a\n+b\n";
        assert!(!diff_too_large(small));
        let many_files: String = (0..101)
            .map(|ix| format!("diff --git a/f{ix}.rs b/f{ix}.rs\n"))
            .collect();
        assert!(diff_too_large(&many_files));
        let many_lines: String = "diff --git a/f.rs b/f.rs\n".to_string()
            + &"+line\n".repeat(20_001);
        assert!(diff_too_large(&many_lines));
    }
}
