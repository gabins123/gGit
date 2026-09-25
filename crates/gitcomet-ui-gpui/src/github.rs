//! GitHub pull requests through the `gh` CLI.
//!
//! gh owns authentication: nothing here reads, stores or passes a token. Every
//! call is non-interactive and names its repository (`--repo owner/name`), so
//! gh never prompts or guesses between remotes. Values that come from GitHub
//! are treated as untrusted: object ids are checked before git sees them, and
//! user text reaches gh as `--flag=value` or through stdin, never as a bare
//! argument that could parse as an option.

use serde::Deserialize;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// A pull request past either limit is reviewed on GitHub instead of in the app.
/// `gh pr view --json files` lists at most 100 files, so past that the files
/// list would be silently incomplete.
pub(crate) const MAX_IN_APP_FILES: u64 = 100;
pub(crate) const MAX_IN_APP_CHANGED_LINES: u64 = 20_000;

/// How far `gh pr list` looks. Open PRs past this are on GitHub.
const LIST_LIMIT: u32 = 100;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PrError {
    /// `gh` is not on PATH.
    GhMissing,
    /// gh runs but has no signed-in account.
    GhSignedOut,
    /// Anything else gh or git reported, verbatim.
    Failed(String),
}

impl std::fmt::Display for PrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GhMissing => f.write_str("GitHub CLI (gh) not found"),
            Self::GhSignedOut => f.write_str("gh isn't signed in; run `gh auth login`"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub(crate) struct Author {
    pub(crate) login: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
}

impl ReviewDecision {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "APPROVED" => Some(Self::Approved),
            "CHANGES_REQUESTED" => Some(Self::ChangesRequested),
            "REVIEW_REQUIRED" => Some(Self::ReviewRequired),
            _ => None,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Approved => "Approved",
            Self::ChangesRequested => "Changes requested",
            Self::ReviewRequired => "Review required",
        }
    }
}

/// One entry of gh's `statusCheckRollup`: a check run (`status` +
/// `conclusion`) or a commit status (`state`).
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
struct CheckEntry {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ChecksSummary {
    pub(crate) passing: u32,
    pub(crate) failing: u32,
    pub(crate) pending: u32,
}

impl ChecksSummary {
    fn from_entries(entries: &[CheckEntry]) -> Self {
        let mut summary = Self::default();
        for entry in entries {
            let outcome = match (&entry.state, &entry.status, &entry.conclusion) {
                (Some(state), _, _) => match state.as_str() {
                    "SUCCESS" => Some(true),
                    "PENDING" | "EXPECTED" => None,
                    _ => Some(false),
                },
                (None, Some(status), _) if status != "COMPLETED" => None,
                (None, _, Some(conclusion)) => Some(matches!(
                    conclusion.as_str(),
                    "SUCCESS" | "NEUTRAL" | "SKIPPED"
                )),
                (None, _, None) => None,
            };
            match outcome {
                Some(true) => summary.passing += 1,
                Some(false) => summary.failing += 1,
                None => summary.pending += 1,
            }
        }
        summary
    }

    pub(crate) fn total(self) -> u32 {
        self.passing + self.failing + self.pending
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSummary {
    number: u64,
    title: String,
    #[serde(default)]
    author: Author,
    head_ref_name: String,
    base_ref_name: String,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    review_decision: String,
    #[serde(default)]
    status_check_rollup: Vec<CheckEntry>,
}

/// A row of the open pull request list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PullRequestSummary {
    pub(crate) number: u64,
    pub(crate) title: String,
    pub(crate) author: String,
    pub(crate) head: String,
    pub(crate) base: String,
    pub(crate) is_draft: bool,
    pub(crate) review: Option<ReviewDecision>,
    pub(crate) checks: ChecksSummary,
}

impl From<RawSummary> for PullRequestSummary {
    fn from(raw: RawSummary) -> Self {
        Self {
            number: raw.number,
            title: raw.title,
            author: raw.author.login,
            head: raw.head_ref_name,
            base: raw.base_ref_name,
            is_draft: raw.is_draft,
            review: ReviewDecision::parse(&raw.review_decision),
            checks: ChecksSummary::from_entries(&raw.status_check_rollup),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct PullRequestFile {
    pub(crate) path: String,
    #[serde(default)]
    pub(crate) additions: u64,
    #[serde(default)]
    pub(crate) deletions: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDetail {
    number: u64,
    title: String,
    #[serde(default)]
    body: String,
    url: String,
    #[serde(default)]
    author: Author,
    head_ref_name: String,
    head_ref_oid: String,
    base_ref_name: String,
    base_ref_oid: String,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    state: String,
    #[serde(default)]
    review_decision: String,
    #[serde(default)]
    mergeable: String,
    #[serde(default)]
    additions: u64,
    #[serde(default)]
    deletions: u64,
    #[serde(default)]
    changed_files: u64,
    #[serde(default)]
    files: Vec<PullRequestFile>,
    #[serde(default)]
    status_check_rollup: Vec<CheckEntry>,
}

/// Everything the Details panel shows for one pull request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PullRequestDetail {
    pub(crate) number: u64,
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) url: String,
    pub(crate) author: String,
    pub(crate) head: String,
    pub(crate) head_oid: String,
    pub(crate) base: String,
    pub(crate) base_oid: String,
    pub(crate) is_draft: bool,
    /// gh's `OPEN`, `CLOSED` or `MERGED`.
    pub(crate) state: String,
    pub(crate) review: Option<ReviewDecision>,
    /// `Some(false)` when GitHub reports conflicts; `None` while it is still
    /// computing mergeability.
    pub(crate) mergeable: Option<bool>,
    pub(crate) additions: u64,
    pub(crate) deletions: u64,
    pub(crate) changed_files: u64,
    pub(crate) files: Vec<PullRequestFile>,
    pub(crate) checks: ChecksSummary,
}

impl PullRequestDetail {
    /// Whether the diff is past the in-app limits and belongs on GitHub.
    pub(crate) fn too_large_for_app(&self) -> bool {
        self.changed_files > MAX_IN_APP_FILES
            || self.additions + self.deletions > MAX_IN_APP_CHANGED_LINES
    }
}

impl From<RawDetail> for PullRequestDetail {
    fn from(raw: RawDetail) -> Self {
        Self {
            number: raw.number,
            title: raw.title,
            body: raw.body,
            url: raw.url,
            author: raw.author.login,
            head: raw.head_ref_name,
            head_oid: raw.head_ref_oid,
            base: raw.base_ref_name,
            base_oid: raw.base_ref_oid,
            is_draft: raw.is_draft,
            state: raw.state,
            review: ReviewDecision::parse(&raw.review_decision),
            mergeable: match raw.mergeable.as_str() {
                "MERGEABLE" => Some(true),
                "CONFLICTING" => Some(false),
                _ => None,
            },
            additions: raw.additions,
            deletions: raw.deletions,
            changed_files: raw.changed_files,
            files: raw.files,
            checks: ChecksSummary::from_entries(&raw.status_check_rollup),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReviewKind {
    Comment,
    Approve,
    RequestChanges,
}

impl ReviewKind {
    /// Comment and Request changes are rejected by GitHub without a body.
    pub(crate) fn needs_body(self) -> bool {
        !matches!(self, Self::Approve)
    }

    fn flag(self) -> &'static str {
        match self {
            Self::Comment => "--comment",
            Self::Approve => "--approve",
            Self::RequestChanges => "--request-changes",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NewPullRequest {
    pub(crate) base: String,
    pub(crate) head: String,
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) draft: bool,
}

/// `--repo` with the host spelled out, so a `GH_HOST` default (GitHub
/// Enterprise) can never send reads or reviews to another server.
fn repo_flag(repo: &str) -> String {
    format!("--repo=github.com/{repo}")
}

/// A gh command with every interactive or decorative behaviour turned off.
fn gh(workdir: &Path) -> Command {
    let mut command = gitcomet_core::process::background_command("gh");
    command
        .current_dir(workdir)
        .env_remove("GH_HOST")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GH_PAGER", "")
        .env("NO_COLOR", "1")
        .env("CLICOLOR", "0");
    command
}

/// Runs `command`, feeding `stdin` when given. gh has its own network
/// timeouts, so none is layered on here.
// ponytail: no cancellation; a stale result is dropped by the caller's
// sequence check. Add a kill path if gh is ever seen hanging.
fn run(mut command: Command, stdin: Option<&str>) -> Result<Vec<u8>, PrError> {
    command
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => PrError::GhMissing,
        _ => PrError::Failed(err.to_string()),
    })?;
    if let Some(text) = stdin
        && let Some(mut pipe) = child.stdin.take()
    {
        // A write error surfaces as the child's own failure below.
        let _ = pipe.write_all(text.as_bytes());
    }
    let output = child
        .wait_with_output()
        .map_err(|err| PrError::Failed(err.to_string()))?;
    into_result(output)
}

fn into_result(output: Output) -> Result<Vec<u8>, PrError> {
    if output.status.success() {
        return Ok(output.stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(classify_failure(stderr.trim()))
}

fn classify_failure(stderr: &str) -> PrError {
    // gh names its own fix whenever no account is configured.
    if stderr.contains("gh auth login") {
        PrError::GhSignedOut
    } else if stderr.is_empty() {
        PrError::Failed("the command failed without saying why".to_string())
    } else {
        PrError::Failed(stderr.to_string())
    }
}

fn parse_json<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, PrError> {
    serde_json::from_slice(bytes)
        .map_err(|err| PrError::Failed(format!("unexpected gh output: {err}")))
}

pub(crate) fn list_open(workdir: &Path, repo: &str) -> Result<Vec<PullRequestSummary>, PrError> {
    let mut command = gh(workdir);
    command.args([
        "pr",
        "list",
        &repo_flag(repo),
        "--state=open",
        &format!("--limit={LIST_LIMIT}"),
        "--json=number,title,author,headRefName,baseRefName,isDraft,reviewDecision,statusCheckRollup",
    ]);
    let raw: Vec<RawSummary> = parse_json(&run(command, None)?)?;
    Ok(raw.into_iter().map(Into::into).collect())
}

pub(crate) fn view(workdir: &Path, repo: &str, number: u64) -> Result<PullRequestDetail, PrError> {
    let mut command = gh(workdir);
    command.args([
        "pr",
        "view",
        &number.to_string(),
        &repo_flag(repo),
        "--json=number,title,body,url,author,headRefName,headRefOid,baseRefName,baseRefOid,\
         isDraft,state,reviewDecision,mergeable,additions,deletions,changedFiles,files,\
         statusCheckRollup",
    ]);
    let raw: RawDetail = parse_json(&run(command, None)?)?;
    Ok(raw.into())
}

/// The pull request's patch, as GitHub serves it.
pub(crate) fn diff(workdir: &Path, repo: &str, number: u64) -> Result<String, PrError> {
    let mut command = gh(workdir);
    command.args([
        "pr",
        "diff",
        &number.to_string(),
        &repo_flag(repo),
        "--color=never",
    ]);
    let stdout = run(command, None)?;
    Ok(String::from_utf8_lossy(&stdout).into_owned())
}

/// Posts a review. Nothing here runs without an explicit submit in the UI.
pub(crate) fn review(
    workdir: &Path,
    repo: &str,
    number: u64,
    kind: ReviewKind,
    body: &str,
) -> Result<(), PrError> {
    let mut command = gh(workdir);
    command.args([
        "pr",
        "review",
        &number.to_string(),
        &repo_flag(repo),
        kind.flag(),
    ]);
    let body = body.trim();
    if !body.is_empty() {
        command.arg("--body-file=-");
    }
    run(command, (!body.is_empty()).then_some(body)).map(|_| ())
}

/// Opens a pull request and returns its URL. Never pushes: `--head` names a
/// branch that must already be on GitHub.
pub(crate) fn create(workdir: &Path, repo: &str, pr: &NewPullRequest) -> Result<String, PrError> {
    let mut command = gh(workdir);
    command.args([
        "pr",
        "create",
        &repo_flag(repo),
        &format!("--base={}", pr.base),
        &format!("--head={}", pr.head),
        &format!("--title={}", pr.title),
        "--body-file=-",
    ]);
    if pr.draft {
        command.arg("--draft");
    }
    let stdout = run(command, Some(&pr.body))?;
    Ok(String::from_utf8_lossy(&stdout).trim().to_string())
}

/// A full hex object id, so a value from GitHub can never reach git as an
/// option or a revision expression.
fn is_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn git(workdir: &Path) -> Command {
    let mut command = gitcomet_core::process::git_command();
    command.current_dir(workdir).env("GIT_TERMINAL_PROMPT", "0");
    command
}

/// `run` for git: a missing binary is git's problem, not gh's.
fn run_git(command: Command) -> Result<Vec<u8>, PrError> {
    run(command, None).map_err(|err| match err {
        PrError::GhMissing => PrError::Failed("git not found".to_string()),
        err => err,
    })
}

fn has_commit(workdir: &Path, oid: &str) -> bool {
    let mut command = git(workdir);
    command.args(["cat-file", "-e", &format!("{oid}^{{commit}}")]);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Makes the PR's base and head commits available locally and returns their
/// merge base, which is what GitHub diffs a PR against. Fetches by object id
/// only, so no ref, branch, FETCH_HEAD or working-tree file changes.
pub(crate) fn prepare_diff_range(
    workdir: &Path,
    remote: &str,
    base_oid: &str,
    head_oid: &str,
) -> Result<String, PrError> {
    if !is_object_id(base_oid) || !is_object_id(head_oid) {
        return Err(PrError::Failed(
            "GitHub returned an unexpected commit id".to_string(),
        ));
    }
    let missing: Vec<&str> = [base_oid, head_oid]
        .into_iter()
        .filter(|oid| !has_commit(workdir, oid))
        .collect();
    if !missing.is_empty() {
        let mut command = git(workdir);
        command.args([
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            "--",
            remote,
        ]);
        command.args(&missing);
        run_git(command)?;
    }
    let mut command = git(workdir);
    command.args(["merge-base", base_oid, head_oid]);
    // With no common ancestor git exits 1 and says nothing.
    let stdout = run_git(command).unwrap_or_default();
    let merge_base = String::from_utf8_lossy(&stdout).trim().to_string();
    if !is_object_id(&merge_base) {
        return Err(PrError::Failed(
            "the pull request shares no history with its base".to_string(),
        ));
    }
    Ok(merge_base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_json_maps_review_and_checks() {
        let json = r#"[{
            "number": 48, "title": "Keyboard nav", "author": {"login": "gabins123"},
            "headRefName": "feat/keyboard-nav", "baseRefName": "dev", "isDraft": false,
            "reviewDecision": "REVIEW_REQUIRED",
            "statusCheckRollup": [
                {"__typename": "CheckRun", "status": "COMPLETED", "conclusion": "SUCCESS"},
                {"__typename": "CheckRun", "status": "COMPLETED", "conclusion": "FAILURE"},
                {"__typename": "CheckRun", "status": "IN_PROGRESS", "conclusion": ""},
                {"__typename": "StatusContext", "state": "PENDING"},
                {"__typename": "StatusContext", "state": "SUCCESS"}
            ]
        }, {
            "number": 47, "title": "Draft", "author": {"login": "someone"},
            "headRefName": "wip", "baseRefName": "dev", "isDraft": true,
            "reviewDecision": "", "statusCheckRollup": []
        }]"#;
        let raw: Vec<RawSummary> = parse_json(json.as_bytes()).expect("valid list");
        let prs: Vec<PullRequestSummary> = raw.into_iter().map(Into::into).collect();
        assert_eq!(prs[0].number, 48);
        assert_eq!(prs[0].author, "gabins123");
        assert_eq!(prs[0].review, Some(ReviewDecision::ReviewRequired));
        assert_eq!(
            prs[0].checks,
            ChecksSummary {
                passing: 2,
                failing: 1,
                pending: 2
            }
        );
        assert!(prs[1].is_draft);
        assert_eq!(prs[1].review, None);
        assert_eq!(prs[1].checks.total(), 0);
    }

    #[test]
    fn detail_json_maps_mergeable_and_size_limits() {
        let json = r#"{
            "number": 52, "title": "Vendor grammars", "body": "", "url": "https://github.com/o/r/pull/52",
            "author": {"login": "a"}, "headRefName": "vendor", "headRefOid": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "baseRefName": "dev", "baseRefOid": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "isDraft": false,
            "state": "OPEN", "reviewDecision": "APPROVED", "mergeable": "CONFLICTING",
            "additions": 18000, "deletions": 2001, "changedFiles": 214,
            "files": [{"path": "src/a.rs", "additions": 3, "deletions": 1}],
            "statusCheckRollup": []
        }"#;
        let detail: PullRequestDetail = parse_json::<RawDetail>(json.as_bytes())
            .expect("valid detail")
            .into();
        assert_eq!(detail.mergeable, Some(false));
        assert_eq!(detail.review, Some(ReviewDecision::Approved));
        assert_eq!(detail.files[0].path, "src/a.rs");
        // Past both limits: 214 files and 20,001 changed lines.
        assert!(detail.too_large_for_app());
    }

    #[test]
    fn signed_out_is_recognised_from_ghs_own_hint() {
        assert_eq!(
            classify_failure("To get started with GitHub CLI, please run:  gh auth login"),
            PrError::GhSignedOut
        );
        assert_eq!(
            classify_failure("GraphQL: Can not approve your own pull request"),
            PrError::Failed("GraphQL: Can not approve your own pull request".to_string())
        );
    }

    #[test]
    fn only_full_hex_ids_reach_git() {
        assert!(is_object_id(&"a".repeat(40)));
        assert!(is_object_id(&"0".repeat(64)));
        assert!(!is_object_id("--upload-pack=evil"));
        assert!(!is_object_id("HEAD"));
        assert!(!is_object_id(&"g".repeat(40)));
    }
}
