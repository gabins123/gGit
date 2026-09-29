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

/// GitHub's API lists a pull request's first 3,000 files and no more. Past
/// that the list would be silently incomplete, so such a pull request is
/// reviewed on GitHub instead.
pub(crate) const MAX_LISTED_FILES: u64 = 3_000;
/// Files per page of that list; `gh pr view` carries the first page.
pub(crate) const FILES_PER_PAGE: u64 = 100;
/// Codex reads the whole diff in one prompt: past these it's too much.
const MAX_CODEX_FILES: u64 = 100;
const MAX_CODEX_CHANGED_LINES: u64 = 20_000;

/// How far `gh pr list` looks. Open PRs past this are on GitHub.
const LIST_LIMIT: u32 = 100;

/// The newest conversation entries the Details panel shows, and how much of
/// each body; the rest is on GitHub.
const MAX_CONVERSATION: usize = 100;
const MAX_CONVERSATION_BODY_CHARS: usize = 4_000;

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
    Commented,
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
            Self::Commented => "Commented",
        }
    }

    /// `author_login` is the pull request's own author: their replies show up
    /// as a COMMENTED review and must not stand in for an actual reviewer.
    fn from_reviews(
        decision: &str,
        requests: &[RawReviewRequest],
        reviews: &[RawLatestReview],
        author_login: &str,
    ) -> Option<Self> {
        if let Some(decision) = Self::parse(decision) {
            return Some(decision);
        }
        let requested_logins: Vec<String> = requests
            .iter()
            .filter_map(|request| {
                request
                    .requested_reviewer
                    .as_ref()
                    .and_then(RawReviewerIdentity::display_name)
                    .or_else(|| request.identity.display_name())
            })
            .collect();
        let mut approved = false;
        let mut commented = false;
        let mut changes_requested = false;
        for review in reviews {
            // gh sometimes omits a review's author; without one there's no PR
            // author or pending request to match, so the review still counts.
            let login = review.author.as_ref().map_or("", |author| &author.login);
            if !login.is_empty()
                && (login.eq_ignore_ascii_case(author_login)
                    || requested_logins
                        .iter()
                        .any(|requested| requested.eq_ignore_ascii_case(login)))
            {
                continue;
            }
            match review.state.as_str() {
                "CHANGES_REQUESTED" => changes_requested = true,
                "APPROVED" => approved = true,
                "COMMENTED" => commented = true,
                _ => {}
            }
        }
        if changes_requested {
            Some(Self::ChangesRequested)
        } else if !requested_logins.is_empty() {
            Some(Self::ReviewRequired)
        } else if approved {
            Some(Self::Approved)
        } else if commented {
            Some(Self::Commented)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PrReviewerStatus {
    Requested,
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
}

impl PrReviewerStatus {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "APPROVED" => Some(Self::Approved),
            "CHANGES_REQUESTED" => Some(Self::ChangesRequested),
            "COMMENTED" => Some(Self::Commented),
            "DISMISSED" => Some(Self::Dismissed),
            _ => None,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Requested => "Review requested",
            Self::Approved => "Approved",
            Self::ChangesRequested => "Changes requested",
            Self::Commented => "Commented",
            Self::Dismissed => "Dismissed",
        }
    }
}

/// A requested reviewer or the reviewer's latest submitted verdict.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PrReviewer {
    /// The user's login, or a team's name when GitHub requested a team.
    pub(crate) login: String,
    pub(crate) status: PrReviewerStatus,
}

/// One entry of gh's `statusCheckRollup`: a check run (`name`, `status` +
/// `conclusion`) or a commit status (`context`, `state`).
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
struct CheckEntry {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum CheckState {
    Failing,
    Pending,
    Passing,
}

impl CheckEntry {
    fn outcome(&self) -> CheckState {
        match (&self.state, &self.status, &self.conclusion) {
            (Some(state), _, _) => match state.as_str() {
                "SUCCESS" => CheckState::Passing,
                "PENDING" | "EXPECTED" => CheckState::Pending,
                _ => CheckState::Failing,
            },
            (None, Some(status), _) if status != "COMPLETED" => CheckState::Pending,
            (None, _, Some(conclusion)) => {
                if matches!(conclusion.as_str(), "SUCCESS" | "NEUTRAL" | "SKIPPED") {
                    CheckState::Passing
                } else {
                    CheckState::Failing
                }
            }
            (None, _, None) => CheckState::Pending,
        }
    }
}

/// One check as the Details panel lists it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CheckRun {
    pub(crate) name: String,
    pub(crate) state: CheckState,
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
            match entry.outcome() {
                CheckState::Passing => summary.passing += 1,
                CheckState::Failing => summary.failing += 1,
                CheckState::Pending => summary.pending += 1,
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
    #[serde(default)]
    head_repository_owner: Author,
    base_ref_name: String,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    is_cross_repository: bool,
    #[serde(default)]
    review_decision: String,
    #[serde(default)]
    status_check_rollup: Vec<CheckEntry>,
    #[serde(default)]
    review_requests: Vec<RawReviewRequest>,
    #[serde(default)]
    latest_reviews: Vec<RawLatestReview>,
}

/// A row of the open pull request list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PullRequestSummary {
    pub(crate) number: u64,
    pub(crate) title: String,
    pub(crate) author: String,
    pub(crate) head: String,
    /// The head repository's owner; a fork's pull request shares branch
    /// names like `main` with everyone else's.
    pub(crate) head_owner: String,
    pub(crate) base: String,
    pub(crate) is_draft: bool,
    /// From a fork: its head branch name means nothing in this repository,
    /// so it can never be part of a stack here.
    pub(crate) is_cross_repository: bool,
    pub(crate) review: Option<ReviewDecision>,
    pub(crate) checks: ChecksSummary,
    /// Your review is requested on it.
    pub(crate) review_requested: bool,
    /// The signed-in GitHub account authored this pull request.
    pub(crate) is_mine: bool,
}

impl From<RawSummary> for PullRequestSummary {
    fn from(raw: RawSummary) -> Self {
        let review = ReviewDecision::from_reviews(
            &raw.review_decision,
            &raw.review_requests,
            &raw.latest_reviews,
            &raw.author.login,
        );
        Self {
            number: raw.number,
            title: raw.title,
            author: raw.author.login,
            head: raw.head_ref_name,
            head_owner: raw.head_repository_owner.login,
            base: raw.base_ref_name,
            is_draft: raw.is_draft,
            is_cross_repository: raw.is_cross_repository,
            review,
            checks: ChecksSummary::from_entries(&raw.status_check_rollup),
            review_requested: false,
            is_mine: false,
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

/// A stacked pull request unit: every PR from the stack base up. GitHub
/// allows a PR to have two children (a tree); it is flattened here, depth
/// first, ordered by number, so it still "shows as one" — `stack_parent`,
/// `stack_child` and `stack_depth` below recover the real shape from base and
/// head branches (or, for a native stack, straight-line by construction).
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PullRequestStack {
    /// Every pull request in the stack; `members[0]` is always the base.
    pub(crate) members: Vec<u64>,
    /// One of GitHub's own stacks (the GraphQL `stack` field), not just a
    /// base-branch chain gGit inferred.
    pub(crate) native: bool,
}

/// Builds stacks from the base-branch chain: PR B is on PR A when B's base
/// branch is A's head branch, both in `repo_owner`'s repository (a fork's PR
/// can't stack: GitHub's `isCrossRepository` is trusted first, a
/// case-insensitive owner compare backs it up in case gh ever fails to
/// report it). `native_stacks` are GitHub's own stacks (from the live
/// GraphQL `stack` field, see `native_stacks` below); a chain is `native`
/// when its member set matches one of them exactly. Pure and cycle-safe: a
/// chain that loops back on itself stops instead of growing forever.
pub(crate) fn compute_pull_request_stacks(
    prs: &[PullRequestSummary],
    repo_owner: &str,
    native_stacks: &[PullRequestStack],
) -> Vec<PullRequestStack> {
    use std::collections::HashMap;

    let eligible: Vec<&PullRequestSummary> = prs
        .iter()
        .filter(|pr| !pr.is_cross_repository && pr.head_owner.eq_ignore_ascii_case(repo_owner))
        .collect();
    let by_head: HashMap<&str, u64> = eligible
        .iter()
        .map(|pr| (pr.head.as_str(), pr.number))
        .collect();
    let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut has_parent: std::collections::HashSet<u64> = std::collections::HashSet::new();
    for pr in &eligible {
        if let Some(&parent) = by_head.get(pr.base.as_str())
            && parent != pr.number
        {
            children.entry(parent).or_default().push(pr.number);
            has_parent.insert(pr.number);
        }
    }
    for kids in children.values_mut() {
        kids.sort_unstable();
    }

    let mut roots: Vec<u64> = eligible
        .iter()
        .map(|pr| pr.number)
        .filter(|number| !has_parent.contains(number) && children.contains_key(number))
        .collect();
    roots.sort_unstable();

    roots
        .into_iter()
        .filter_map(|root| stack_from_root(root, &children))
        .map(|members| {
            let native = native_stacks.iter().any(|native| {
                native.members.len() == members.len()
                    && native.members.iter().all(|number| members.contains(number))
            });
            PullRequestStack { members, native }
        })
        .collect()
}

/// Every pull request reachable from `root` through `children`, depth-first
/// pre-order (a parent before its children), or `None` when that's fewer
/// than the two pull requests a stack needs. Cycle-safe: a number already
/// visited is never queued again.
fn stack_from_root(
    root: u64,
    children: &std::collections::HashMap<u64, Vec<u64>>,
) -> Option<Vec<u64>> {
    let mut members = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut pending = vec![root];
    while let Some(number) = pending.pop() {
        if !visited.insert(number) {
            continue;
        }
        members.push(number);
        if let Some(kids) = children.get(&number) {
            pending.extend(kids.iter().rev());
        }
    }
    (members.len() >= 2).then_some(members)
}

/// The pull request `number`'s base branch points at, among `members` — its
/// real parent, not just the entry before it in a flattened list (a tree's
/// depth-first order can put a sibling there instead).
pub(crate) fn stack_parent(members: &[u64], number: u64, prs: &[PullRequestSummary]) -> Option<u64> {
    let pr = prs.iter().find(|pr| pr.number == number)?;
    members.iter().copied().find(|&candidate| {
        candidate != number
            && prs
                .iter()
                .any(|other| other.number == candidate && other.head == pr.base)
    })
}

/// The lowest-numbered pull request based directly on `number`, among
/// `members` — deterministic when `number` has more than one child.
pub(crate) fn stack_child(members: &[u64], number: u64, prs: &[PullRequestSummary]) -> Option<u64> {
    let pr = prs.iter().find(|pr| pr.number == number)?;
    members
        .iter()
        .copied()
        .filter(|&candidate| {
            candidate != number
                && prs
                    .iter()
                    .any(|other| other.number == candidate && other.base == pr.head)
        })
        .min()
}

/// `number`'s distance from the stack's base (0 = bottom), following real
/// parent links (`stack_parent`) rather than a flattened member order a tree
/// can put out of a line. `None` when `number` isn't among `members`.
pub(crate) fn stack_depth(members: &[u64], number: u64, prs: &[PullRequestSummary]) -> Option<usize> {
    if !members.contains(&number) {
        return None;
    }
    let mut depth = 0;
    let mut current = number;
    let mut seen = std::collections::HashSet::new();
    while let Some(parent) = stack_parent(members, current, prs) {
        if !seen.insert(current) {
            break;
        }
        depth += 1;
        current = parent;
    }
    Some(depth)
}

/// GitHub's own stacks among `numbers` (queried pull requests), read from the
/// live GraphQL `stack` field: each stack's members, in GitHub's own order
/// (its `entries`, sorted by `position`; `members[0]` is the stack's base).
/// Best-effort: a repository without the public preview, or any GraphQL
/// failure, comes back empty rather than failing the whole list.
pub(crate) fn native_stacks(workdir: &Path, repo: &str, numbers: &[u64]) -> Vec<PullRequestStack> {
    if numbers.is_empty() || !is_repo_slug(repo) {
        return Vec::new();
    }
    let Some((owner, name)) = repo.split_once('/') else {
        return Vec::new();
    };
    let mut fields = String::new();
    for number in numbers {
        if *number > i32::MAX as u64 {
            continue;
        }
        fields.push_str(&format!(
            "pr{number}: pullRequest(number: {number}) {{ stack {{ number entries(first: 50) {{ nodes {{ position pullRequest {{ number }} }} }} }} }}\n"
        ));
    }
    let query =
        format!("query {{ repository(owner: \"{owner}\", name: \"{name}\") {{ {fields} }} }}");
    let mut command = gh(workdir);
    command.args([
        "api",
        "graphql",
        "--hostname=github.com",
        &format!("--raw-field=query={query}"),
    ]);
    match run(command, None) {
        Ok(bytes) => parse_native_stacks(&bytes).unwrap_or_default(),
        Err(err) => {
            eprintln!("Couldn't read GitHub's native pull request stacks: {err}");
            Vec::new()
        }
    }
}

fn parse_native_stacks(bytes: &[u8]) -> Result<Vec<PullRequestStack>, PrError> {
    #[derive(Deserialize)]
    struct EntryPr {
        number: u64,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct EntryNode {
        position: i64,
        #[serde(default)]
        pull_request: Option<EntryPr>,
    }
    #[derive(Deserialize, Default)]
    struct EntryConnection {
        #[serde(default)]
        nodes: Vec<Option<EntryNode>>,
    }
    #[derive(Deserialize)]
    struct Stack {
        number: u64,
        #[serde(default)]
        entries: EntryConnection,
    }
    #[derive(Deserialize)]
    struct Entry {
        #[serde(default)]
        stack: Option<Stack>,
    }
    let value: serde_json::Value = parse_json(bytes)?;
    let repository = value
        .get("data")
        .and_then(|data| data.get("repository"))
        .and_then(|repo| repo.as_object())
        .ok_or_else(|| PrError::Failed("unexpected gh output".to_string()))?;
    let mut by_stack_id: std::collections::HashMap<u64, Vec<(i64, u64)>> =
        std::collections::HashMap::new();
    for entry in repository.values() {
        if entry.is_null() {
            continue;
        }
        let entry: Entry = serde_json::from_value(entry.clone())
            .map_err(|err| PrError::Failed(format!("unexpected gh output: {err}")))?;
        let Some(stack) = entry.stack else { continue };
        let members = by_stack_id.entry(stack.number).or_default();
        for node in stack.entries.nodes.into_iter().flatten() {
            let Some(pr) = node.pull_request else { continue };
            if !members.iter().any(|(_, number)| *number == pr.number) {
                members.push((node.position, pr.number));
            }
        }
    }
    let mut stacks: Vec<PullRequestStack> = by_stack_id
        .into_values()
        .filter_map(|mut members| {
            members.sort_by_key(|(position, _)| *position);
            let members: Vec<u64> = members.into_iter().map(|(_, number)| number).collect();
            (members.len() >= 2).then_some(PullRequestStack {
                members,
                native: true,
            })
        })
        .collect();
    stacks.sort_by_key(|stack| stack.members.first().copied());
    Ok(stacks)
}

/// What merging `number` in `stack` would do, following GitHub's stacked
/// merge rules: the selected pull request and every unmerged one below it
/// land together, bottom-up. Only the pull requests *below* `number` need to
/// be approved, pass their checks, and be ready; `number` itself only needs
/// to satisfy the base branch's protection rules, which GitHub checks (and
/// reports through `merge_stack`'s `failed` outcome) at merge time — as does
/// a non-linear stack, so this never re-derives that shape itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StackMergePlan {
    /// This pull request and everything below it, bottom-up: what merges.
    pub(crate) merges: Vec<u64>,
    /// The rest of the stack, staying open.
    pub(crate) stays_open: Vec<u64>,
    /// Why the merge is refused, naming the pull request, or `None` when it
    /// can go ahead.
    pub(crate) refusal: Option<String>,
}

/// Plans a stack merge of `number`, or `None` when it isn't in `stack`.
pub(crate) fn plan_stack_merge(
    stack: &PullRequestStack,
    number: u64,
    prs: &[PullRequestSummary],
) -> Option<StackMergePlan> {
    let ix = stack.members.iter().position(|member| *member == number)?;
    let by_number: std::collections::HashMap<u64, &PullRequestSummary> =
        prs.iter().map(|pr| (pr.number, pr)).collect();
    let refuse = |reason: String| StackMergePlan {
        merges: Vec::new(),
        stays_open: stack.members.clone(),
        refusal: Some(reason),
    };
    for &below in &stack.members[..ix] {
        let Some(pr) = by_number.get(&below) else {
            return Some(refuse(format!(
                "#{below} below isn't loaded — refresh or open on GitHub"
            )));
        };
        // No review decision at all means this repository doesn't require
        // one: that's fine. Anything short of approved is not.
        if matches!(pr.review, Some(decision) if decision != ReviewDecision::Approved) {
            return Some(refuse(format!("#{below} isn't approved")));
        }
        if pr.checks.failing > 0 {
            return Some(refuse(format!("#{below} has failing checks")));
        }
        if pr.checks.pending > 0 {
            return Some(refuse(format!("#{below} has checks still running")));
        }
        if pr.is_draft {
            return Some(refuse(format!("#{below} is a draft")));
        }
    }
    Some(StackMergePlan {
        merges: stack.members[..=ix].to_vec(),
        stays_open: stack.members[ix + 1..].to_vec(),
        refusal: None,
    })
}

/// How long `merge_stack` polls GitHub's asynchronous merge job before
/// reporting the outcome as unknown (not failed: GitHub may still finish it).
const STACK_MERGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const STACK_MERGE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
/// Transient poll failures (a dropped connection, a gh hiccup) tolerated
/// before giving up on the poll and reporting the outcome as unknown.
const STACK_MERGE_MAX_POLL_FAILURES: u32 = 3;

/// What GitHub did with a stack merge. Both are terminal: `Enqueued` means it
/// went into a required merge queue instead of merging immediately, and
/// GitHub gives nothing further to poll for once it has.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StackMergeOutcome {
    Merged,
    Enqueued,
}

/// The JSON body `merge_stack` sends: the merge method, and the head commit
/// pinned the way the single-PR path's `--match-head-commit` pins it — per
/// GitHub's docs, "if the PR is pushed in between the merge being requested
/// and being executed, the merge will be cancelled."
fn merge_async_payload(method: MergeMethod, head_oid: &str) -> String {
    serde_json::json!({
        "merge_method": method.api_name(),
        "sha": head_oid,
    })
    .to_string()
}

/// Merges `number` through GitHub's asynchronous stack merge API
/// (`PUT .../pulls/{number}/merge-async`), the one GitHub documents as
/// required for a stacked pull request: "the operation includes all open
/// downstack pull requests." Polls `GET .../merge-async/{uuid}` while the job
/// is `pending`.
pub(crate) fn merge_stack(
    workdir: &Path,
    repo: &str,
    number: u64,
    method: MergeMethod,
    head_oid: &str,
) -> Result<StackMergeOutcome, PrError> {
    if !is_repo_slug(repo) {
        return Err(PrError::Failed(format!(
            "{repo} isn't a GitHub repository name"
        )));
    }
    if !is_object_id(head_oid) {
        return Err(PrError::Failed(format!(
            "#{number}'s head commit isn't known yet"
        )));
    }
    let payload = merge_async_payload(method, head_oid);
    let mut start = gh(workdir);
    start.args([
        "api",
        "--hostname=github.com",
        &format!("repos/{repo}/pulls/{number}/merge-async"),
        "--method=PUT",
        "--input=-",
    ]);
    let mut job = parse_merge_async_job(&run_merge_async_start(start, &payload)?)?;
    let deadline = std::time::Instant::now() + STACK_MERGE_TIMEOUT;
    let mut poll_failures = 0u32;
    let unknown = || {
        PrError::Failed(format!(
            "#{number}'s stack merge outcome is unknown; check it on GitHub"
        ))
    };
    loop {
        match job.status {
            MergeAsyncStatus::Merged => return Ok(StackMergeOutcome::Merged),
            MergeAsyncStatus::Enqueued => return Ok(StackMergeOutcome::Enqueued),
            MergeAsyncStatus::Failed => {
                return Err(PrError::Failed(job.message.unwrap_or_else(|| {
                    format!("#{number}'s stack merge failed on GitHub")
                })));
            }
            MergeAsyncStatus::Pending => {}
        }
        if std::time::Instant::now() >= deadline {
            return Err(unknown());
        }
        std::thread::sleep(STACK_MERGE_POLL_INTERVAL);
        let Some(uuid) = job.uuid.clone() else {
            return Err(unknown());
        };
        let mut poll = gh(workdir);
        poll.args([
            "api",
            "--hostname=github.com",
            &format!("repos/{repo}/pulls/{number}/merge-async/{uuid}"),
        ]);
        match run(poll, None).and_then(|bytes| parse_merge_async_job(&bytes)) {
            Ok(next) => {
                job = next;
                poll_failures = 0;
            }
            Err(_) => {
                poll_failures += 1;
                if poll_failures >= STACK_MERGE_MAX_POLL_FAILURES {
                    return Err(unknown());
                }
            }
        }
    }
}

/// GitHub's asynchronous merge job status
/// (`pending`/`merged`/`enqueued`/`failed`, per GitHub's OpenAPI schema for
/// `pull-request-merge-async-result`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MergeAsyncStatus {
    Pending,
    Merged,
    Enqueued,
    Failed,
}

/// GitHub's asynchronous merge job, as both `merge-async` and its polling
/// endpoint return it: `{status, details}`. `details.uuid` polls a `pending`
/// job; `details.message` explains a `failed` one.
struct MergeAsyncJob {
    status: MergeAsyncStatus,
    uuid: Option<String>,
    message: Option<String>,
}

fn parse_merge_async_job(bytes: &[u8]) -> Result<MergeAsyncJob, PrError> {
    #[derive(Deserialize, Default)]
    struct RawDetails {
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        uuid: Option<String>,
    }
    #[derive(Deserialize)]
    struct RawJob {
        status: String,
        #[serde(default)]
        details: Option<RawDetails>,
    }
    let raw: RawJob = parse_json(bytes)?;
    let status = match raw.status.as_str() {
        "pending" => MergeAsyncStatus::Pending,
        "merged" => MergeAsyncStatus::Merged,
        "enqueued" => MergeAsyncStatus::Enqueued,
        "failed" => MergeAsyncStatus::Failed,
        other => {
            return Err(PrError::Failed(format!(
                "unexpected gh output: unknown merge status {other}"
            )));
        }
    };
    let details = raw.details.unwrap_or_default();
    Ok(MergeAsyncJob {
        status,
        uuid: details.uuid,
        message: details.message,
    })
}

/// `run`, but on a non-zero exit it first looks at stdout for GitHub's own
/// explanation: `merge-async` returns its `{status, details}` body even on
/// 400 and 409 (a non-linear stack, a branch protection rule), and gh's exit
/// code otherwise hides it behind a generic HTTP error that only stderr's
/// status line explains. Used only by `merge_stack`'s starting `PUT`; every
/// other caller keeps plain `run`, whose stderr-only failure is what gh
/// normally gives.
fn run_merge_async_start(mut command: Command, body: &str) -> Result<Vec<u8>, PrError> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => PrError::GhMissing,
        _ => PrError::Failed(err.to_string()),
    })?;
    if let Some(mut pipe) = child.stdin.take() {
        // A write error surfaces as the child's own failure below.
        let _ = pipe.write_all(body.as_bytes());
    }
    let output = child
        .wait_with_output()
        .map_err(|err| PrError::Failed(err.to_string()))?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    if let Some(message) = merge_async_failure_message(&output.stdout) {
        return Err(PrError::Failed(message));
    }
    Err(classify_failure(
        String::from_utf8_lossy(&output.stderr).trim(),
    ))
}

/// GitHub's own explanation from a `merge-async` response's
/// `details.message`, when gh's stdout on a failed exit is that response
/// body (a 400 or 409, per GitHub's OpenAPI schema for
/// `pull-request-merge-async-result`); `None` when stdout isn't that body,
/// or has no message.
fn merge_async_failure_message(stdout: &[u8]) -> Option<String> {
    parse_merge_async_job(stdout).ok()?.message
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawComment {
    #[serde(default)]
    id: String,
    #[serde(default)]
    author: Author,
    #[serde(default)]
    body: String,
    #[serde(default)]
    created_at: String,
    /// Hidden on GitHub (spam, off-topic, outdated): not shown here either.
    #[serde(default)]
    is_minimized: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReview {
    #[serde(default)]
    id: String,
    #[serde(default)]
    author: Author,
    #[serde(default)]
    body: String,
    #[serde(default)]
    state: String,
    /// `null` on a review that is still pending.
    #[serde(default)]
    submitted_at: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPullRequestCommit {
    #[serde(default)]
    oid: String,
    #[serde(default)]
    message_headline: String,
    #[serde(default)]
    committed_date: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PullRequestCommit {
    pub(crate) oid: String,
    pub(crate) headline: String,
    pub(crate) committed_at: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReviewerIdentity {
    #[serde(default)]
    login: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    slug: Option<String>,
}

impl RawReviewerIdentity {
    fn display_name(&self) -> Option<String> {
        self.login
            .as_ref()
            .or(self.name.as_ref())
            .or(self.slug.as_ref())
            .filter(|name| !name.is_empty())
            .cloned()
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReviewRequest {
    #[serde(flatten)]
    identity: RawReviewerIdentity,
    #[serde(default)]
    requested_reviewer: Option<RawReviewerIdentity>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLatestReview {
    #[serde(default)]
    author: Option<Author>,
    #[serde(default)]
    state: String,
    #[serde(default)]
    submitted_at: Option<String>,
}

fn reviewers(requests: Vec<RawReviewRequest>, reviews: Vec<RawLatestReview>) -> Vec<PrReviewer> {
    let mut result = Vec::new();
    for request in requests {
        let Some(login) = request
            .requested_reviewer
            .as_ref()
            .and_then(RawReviewerIdentity::display_name)
            .or_else(|| request.identity.display_name())
        else {
            continue;
        };
        if !result
            .iter()
            .any(|reviewer: &PrReviewer| reviewer.login.eq_ignore_ascii_case(&login))
        {
            result.push(PrReviewer {
                login,
                status: PrReviewerStatus::Requested,
            });
        }
    }

    let mut latest = Vec::<(String, String, PrReviewerStatus)>::new();
    for review in reviews {
        let (Some(author), Some(status)) = (review.author, PrReviewerStatus::parse(&review.state))
        else {
            continue;
        };
        let submitted_at = review.submitted_at.unwrap_or_default();
        if let Some((login, latest_at, latest_status)) = latest
            .iter_mut()
            .find(|(login, _, _)| login.eq_ignore_ascii_case(&author.login))
        {
            if submitted_at >= *latest_at {
                *login = author.login;
                *latest_at = submitted_at;
                *latest_status = status;
            }
        } else {
            latest.push((author.login, submitted_at, status));
        }
    }
    for (login, _, status) in latest {
        match result
            .iter_mut()
            .find(|reviewer: &&mut PrReviewer| reviewer.login.eq_ignore_ascii_case(&login))
        {
            Some(reviewer) if reviewer.status == PrReviewerStatus::Requested => {}
            Some(reviewer) => reviewer.status = status,
            None => result.push(PrReviewer { login, status }),
        }
    }
    result.sort_by(|a, b| {
        a.login
            .to_ascii_lowercase()
            .cmp(&b.login.to_ascii_lowercase())
            .then_with(|| a.login.cmp(&b.login))
    });
    result
}

/// A comment or review on the pull request's conversation. The body is
/// untrusted GitHub text rendered only through the safe Markdown preview path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConversationEntry {
    pub(crate) id: String,
    pub(crate) author: String,
    /// "commented", "approved", "requested changes", …
    pub(crate) verb: &'static str,
    /// ISO 8601, as GitHub gives it.
    pub(crate) at: String,
    pub(crate) body: String,
}

/// Comments and reviews oldest first, the newest `MAX_CONVERSATION` of them.
/// A review that is only inline code comments has no body and adds nothing
/// here, so it is left out; approvals and change requests always show.
fn conversation(comments: Vec<RawComment>, reviews: Vec<RawReview>) -> Vec<ConversationEntry> {
    let comments = comments
        .into_iter()
        .filter(|comment| !comment.is_minimized)
        .map(|comment| {
            (
                comment.id,
                comment.author,
                "commented",
                comment.created_at,
                comment.body,
            )
        });
    let reviews = reviews.into_iter().filter_map(|review| {
        let verb = match review.state.as_str() {
            "APPROVED" => "approved",
            "CHANGES_REQUESTED" => "requested changes",
            "DISMISSED" => "reviewed (dismissed)",
            "PENDING" => return None,
            _ if review.body.trim().is_empty() => return None,
            _ => "reviewed",
        };
        Some((
            review.id,
            review.author,
            verb,
            review.submitted_at.unwrap_or_default(),
            review.body,
        ))
    });
    let mut entries: Vec<ConversationEntry> = comments
        .chain(reviews)
        .map(|(id, author, verb, at, body)| ConversationEntry {
            id: if id.is_empty() {
                format!("{verb}:{}:{at}", author.login)
            } else {
                id
            },
            author: author.login,
            verb,
            at,
            body: body
                .trim()
                .chars()
                .take(MAX_CONVERSATION_BODY_CHARS)
                .collect(),
        })
        .collect();
    entries.sort_by(|a, b| a.at.cmp(&b.at));
    let skip = entries.len().saturating_sub(MAX_CONVERSATION);
    entries.drain(..skip);
    entries
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
    is_cross_repository: bool,
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
    #[serde(default)]
    comments: Vec<RawComment>,
    #[serde(default)]
    reviews: Vec<RawReview>,
    #[serde(default)]
    review_requests: Vec<RawReviewRequest>,
    #[serde(default)]
    latest_reviews: Vec<RawLatestReview>,
    #[serde(default)]
    commits: Vec<RawPullRequestCommit>,
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
    /// From a fork: its branch name means nothing in this repository.
    pub(crate) is_cross_repository: bool,
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
    /// Failing first, then pending, then passing.
    pub(crate) check_runs: Vec<CheckRun>,
    pub(crate) conversation: Vec<ConversationEntry>,
    pub(crate) reviewers: Vec<PrReviewer>,
    /// GitHub's PR commits, newest first.
    pub(crate) commits: Vec<PullRequestCommit>,
}

impl PullRequestDetail {
    /// Whether GitHub can't list all of its files, so it belongs on GitHub.
    pub(crate) fn too_large_for_app(&self) -> bool {
        self.changed_files > MAX_LISTED_FILES
    }

    /// Whether its diff is too much for one Codex prompt.
    pub(crate) fn too_large_for_codex(&self) -> bool {
        self.changed_files > MAX_CODEX_FILES
            || self.additions + self.deletions > MAX_CODEX_CHANGED_LINES
    }
}

impl From<RawDetail> for PullRequestDetail {
    fn from(raw: RawDetail) -> Self {
        let review = ReviewDecision::from_reviews(
            &raw.review_decision,
            &raw.review_requests,
            &raw.latest_reviews,
            &raw.author.login,
        );
        Self {
            number: raw.number,
            title: raw.title,
            body: raw
                .body
                .trim()
                .chars()
                .take(MAX_CONVERSATION_BODY_CHARS)
                .collect(),
            url: raw.url,
            author: raw.author.login,
            head: raw.head_ref_name,
            head_oid: raw.head_ref_oid,
            base: raw.base_ref_name,
            base_oid: raw.base_ref_oid,
            is_draft: raw.is_draft,
            is_cross_repository: raw.is_cross_repository,
            state: raw.state,
            review,
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
            check_runs: {
                let mut runs: Vec<CheckRun> = raw
                    .status_check_rollup
                    .iter()
                    .map(|entry| CheckRun {
                        name: entry
                            .name
                            .clone()
                            .or_else(|| entry.context.clone())
                            .unwrap_or_else(|| "check".to_string()),
                        state: entry.outcome(),
                    })
                    .collect();
                runs.sort_by(|a, b| a.state.cmp(&b.state).then_with(|| a.name.cmp(&b.name)));
                runs
            },
            conversation: conversation(raw.comments, raw.reviews),
            reviewers: reviewers(raw.review_requests, raw.latest_reviews),
            commits: raw
                .commits
                .into_iter()
                .rev()
                .filter(|commit| is_object_id(&commit.oid))
                .map(|commit| PullRequestCommit {
                    oid: commit.oid,
                    headline: commit.message_headline.chars().take(200).collect(),
                    committed_at: commit.committed_date,
                })
                .collect(),
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

    /// The review's `event`, as the REST API names it.
    fn event(self) -> &'static str {
        match self {
            Self::Comment => "COMMENT",
            Self::Approve => "APPROVE",
            Self::RequestChanges => "REQUEST_CHANGES",
        }
    }
}

/// Which side of the diff a review comment sits on, as GitHub names it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, serde::Serialize, Deserialize)]
pub(crate) enum ReviewSide {
    /// The base: a removed line, by its old number.
    #[serde(rename = "LEFT")]
    Left,
    /// The head: an added or unchanged line, by its new number.
    #[serde(rename = "RIGHT")]
    Right,
}

/// The line, or lines, a review comment is on. `start` is set for a range,
/// which ends at `line`.
#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, Deserialize)]
pub(crate) struct ReviewAnchor {
    pub(crate) path: String,
    pub(crate) side: ReviewSide,
    pub(crate) line: u32,
    #[serde(default)]
    pub(crate) start: Option<(ReviewSide, u32)>,
}

impl ReviewAnchor {
    /// "line 12" or "lines 10–12", for the user.
    pub(crate) fn lines_label(&self) -> String {
        match self.start {
            Some((side, start)) if (side, start) != (self.side, self.line) => {
                format!("lines {start}–{}", self.line)
            }
            _ => format!("line {}", self.line),
        }
    }
}

/// One pending line comment of a review, or a reply to someone's thread.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, Deserialize)]
pub(crate) struct ReviewComment {
    pub(crate) anchor: ReviewAnchor,
    pub(crate) body: String,
    /// Set for a reply: the thread it answers. Replies post after the review,
    /// each on its own thread (GitHub's create-review call can't carry them).
    #[serde(default)]
    pub(crate) reply_to: Option<ReplyTarget>,
}

/// The thread a pending reply answers: its first comment, by id.
#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, Deserialize)]
pub(crate) struct ReplyTarget {
    pub(crate) id: u64,
    pub(crate) author: String,
}

/// One comment of a review thread already on GitHub.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ThreadComment {
    pub(crate) author: String,
    pub(crate) body: String,
    /// ISO 8601, as GitHub gives it.
    pub(crate) at: String,
}

/// A review thread already on GitHub: where it sits and what was said. Its
/// text is from anyone who can comment, shown as plain text only.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReviewThread {
    /// The first comment's id, which replies answer.
    pub(crate) root_id: u64,
    pub(crate) path: String,
    pub(crate) side: ReviewSide,
    /// None once the lines it was on changed (outdated), or for a comment on
    /// the whole file.
    pub(crate) line: Option<u32>,
    /// The line it was written on, in the commit it was written on.
    pub(crate) original_line: Option<u32>,
    /// GitHub's authoritative conversation status.
    pub(crate) is_resolved: bool,
    /// GitHub's authoritative changed-line status.
    pub(crate) is_outdated: bool,
    pub(crate) comments: Vec<ThreadComment>,
}

impl ReviewThread {
    /// Whether GitHub considers this thread outdated.
    pub(crate) fn outdated(&self) -> bool {
        self.is_outdated
    }
}

#[derive(Clone, Debug, Deserialize)]
struct RawReviewComment {
    id: u64,
    #[serde(default)]
    in_reply_to_id: Option<u64>,
    path: String,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default)]
    original_line: Option<u32>,
    #[serde(default)]
    side: Option<ReviewSide>,
    #[serde(default)]
    body: String,
    #[serde(default)]
    user: Option<Author>,
    #[serde(default)]
    created_at: String,
}

/// Review comments as threads: each first comment with the replies to it,
/// oldest first, threads in file and line order.
fn review_threads(raw: Vec<RawReviewComment>) -> Vec<ReviewThread> {
    let mut raw = raw;
    raw.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
    let comment = |raw: &RawReviewComment| ThreadComment {
        author: raw
            .user
            .as_ref()
            .map_or_else(|| "ghost".to_string(), |user| user.login.clone()),
        body: raw
            .body
            .trim()
            .chars()
            .take(MAX_CONVERSATION_BODY_CHARS)
            .collect(),
        at: raw.created_at.clone(),
    };
    let mut threads: Vec<ReviewThread> = raw
        .iter()
        .filter(|raw| raw.in_reply_to_id.is_none())
        .map(|root| ReviewThread {
            root_id: root.id,
            path: root.path.clone(),
            side: root.side.unwrap_or(ReviewSide::Right),
            line: root.line,
            original_line: root.original_line,
            is_resolved: false,
            // Filled authoritatively by the GraphQL thread query; retain the
            // REST heuristic until then for callers that only parse REST.
            is_outdated: root.line.is_none() && root.original_line.is_some(),
            comments: vec![comment(root)],
        })
        .collect();
    for reply in raw.iter().filter(|raw| raw.in_reply_to_id.is_some()) {
        if let Some(thread) = threads
            .iter_mut()
            .find(|thread| Some(thread.root_id) == reply.in_reply_to_id)
        {
            thread.comments.push(comment(reply));
        }
    }
    threads.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
    threads
}

/// A whole review for GitHub's create-review call: verdict, summary and line
/// comments, pinned to the commit that was reviewed.
fn review_payload(
    commit_id: &str,
    kind: ReviewKind,
    body: &str,
    comments: &[ReviewComment],
) -> serde_json::Value {
    let comments: Vec<serde_json::Value> = comments
        .iter()
        .filter(|comment| comment.reply_to.is_none())
        .map(|comment| {
            let mut value = serde_json::json!({
                "path": comment.anchor.path,
                "side": comment.anchor.side,
                "line": comment.anchor.line,
                "body": comment.body,
            });
            // An old and a new line can share a number: a range is about sides too.
            if let Some((side, line)) = comment
                .anchor
                .start
                .filter(|start| *start != (comment.anchor.side, comment.anchor.line))
            {
                value["start_side"] = serde_json::json!(side);
                value["start_line"] = serde_json::json!(line);
            }
            value
        })
        .collect();
    let mut payload = serde_json::json!({
        "commit_id": commit_id,
        "event": kind.event(),
        "comments": comments,
    });
    if !body.trim().is_empty() {
        payload["body"] = serde_json::json!(body);
    }
    payload
}

/// `owner/name` as GitHub allows them: nothing that could turn the API path
/// into something else.
fn is_repo_slug(repo: &str) -> bool {
    let valid = |part: &str| {
        !matches!(part, "" | "." | "..")
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    matches!(repo.split_once('/'), Some((owner, name)) if valid(owner) && valid(name))
}

/// How a pull request lands on its base.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(crate) enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    fn flag(self) -> &'static str {
        match self {
            Self::Merge => "--merge",
            Self::Squash => "--squash",
            Self::Rebase => "--rebase",
        }
    }

    /// The REST API's own spelling, for the async stack merge's JSON body.
    fn api_name(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Squash => "squash",
            Self::Rebase => "rebase",
        }
    }
}

/// What the merge dialog confirmed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MergeRequest {
    pub(crate) method: MergeMethod,
    pub(crate) delete_branch: bool,
    /// The head the user was shown. GitHub refuses the merge if the branch has
    /// moved since, so commits nobody saw never land.
    pub(crate) head_oid: String,
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

/// The open pull requests, and whether gh could say which wait on your review.
/// The open pull requests, whether gh could say which wait on you, and
/// whether the list is every open one (not cut at the limit).
pub(crate) fn list_open(
    workdir: &Path,
    repo: &str,
) -> Result<(Vec<PullRequestSummary>, bool, bool), PrError> {
    let mut command = gh(workdir);
    command.args([
        "pr",
        "list",
        &repo_flag(repo),
        "--state=open",
        &format!("--limit={LIST_LIMIT}"),
        "--json=number,title,author,headRefName,headRepositoryOwner,baseRefName,isDraft,isCrossRepository,reviewDecision,statusCheckRollup,reviewRequests,latestReviews",
    ]);
    let raw: Vec<RawSummary> = parse_json(&run(command, None)?)?;
    let complete = raw.len() < LIST_LIMIT as usize;
    let mut list: Vec<PullRequestSummary> = raw.into_iter().map(Into::into).collect();
    // Which ones wait on you, including any past the list's limit. Only a
    // marker: if gh can't answer, the list still shows (and says so).
    let mut requested = gh(workdir);
    requested.args([
        "pr",
        "list",
        &repo_flag(repo),
        "--state=open",
        "--search=review-requested:@me",
        &format!("--limit={LIST_LIMIT}"),
        "--json=number,title,author,headRefName,headRepositoryOwner,baseRefName,isDraft,isCrossRepository,reviewDecision,statusCheckRollup,reviewRequests,latestReviews",
    ]);
    // Run alongside the login lookup below: both are independent `gh` calls
    // on the critical path of every list load and refresh.
    let (requested, login) = std::thread::scope(|scope| {
        let requested = scope.spawn(|| {
            run(requested, None).and_then(|out| parse_json::<Vec<RawSummary>>(&out))
        });
        let login = viewer_login(workdir);
        (
            requested
                .join()
                .unwrap_or_else(|_| Err(PrError::Failed("gh pr list panicked".to_string()))),
            login,
        )
    });
    let known = requested.is_ok();
    for raw in requested.unwrap_or_default() {
        match list.iter_mut().find(|pr| pr.number == raw.number) {
            Some(pr) => pr.review_requested = true,
            None => list.push(PullRequestSummary {
                review_requested: true,
                ..raw.into()
            }),
        }
    }
    match login {
        Ok(login) => {
            for pr in &mut list {
                pr.is_mine = pr.author.eq_ignore_ascii_case(&login);
            }
        }
        Err(error) => eprintln!("Couldn't identify pull request author: {error}"),
    }
    Ok((list, known, complete))
}

pub(crate) fn view(workdir: &Path, repo: &str, number: u64) -> Result<PullRequestDetail, PrError> {
    let mut command = gh(workdir);
    command.args([
        "pr",
        "view",
        &number.to_string(),
        &repo_flag(repo),
        "--json=number,title,body,url,author,headRefName,headRefOid,baseRefName,baseRefOid,\
         isDraft,isCrossRepository,state,reviewDecision,mergeable,additions,deletions,\
         changedFiles,files,\
          statusCheckRollup,comments,reviews,reviewRequests,latestReviews,commits",
    ]);
    let raw: RawDetail = parse_json(&run(command, None)?)?;
    Ok(raw.into())
}

/// One page of the pull request's files, as GitHub's REST API lists them:
/// `gh pr view` stops at the first 100, and the rest come a page at a time.
pub(crate) fn pull_request_files_page(
    workdir: &Path,
    repo: &str,
    number: u64,
    page: u32,
) -> Result<Vec<PullRequestFile>, PrError> {
    if !is_repo_slug(repo) {
        return Err(PrError::Failed(format!(
            "{repo} isn't a GitHub repository name"
        )));
    }
    #[derive(Deserialize)]
    struct RawFile {
        filename: String,
        #[serde(default)]
        additions: u64,
        #[serde(default)]
        deletions: u64,
    }
    let mut command = gh(workdir);
    command.args([
        "api",
        "--hostname=github.com",
        &format!("repos/{repo}/pulls/{number}/files?per_page={FILES_PER_PAGE}&page={page}"),
    ]);
    let files: Vec<RawFile> = parse_json(&run(command, None)?)?;
    Ok(files
        .into_iter()
        .map(|file| PullRequestFile {
            path: file.filename,
            additions: file.additions,
            deletions: file.deletions,
        })
        .collect())
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

fn checkout_command(workdir: &Path, repo: &str, number: u64, branch: Option<&str>) -> Command {
    let mut command = gh(workdir);
    // The git gh runs must fail rather than wait on a prompt nobody sees.
    command.env("GIT_TERMINAL_PROMPT", "0").args([
        "pr",
        "checkout",
        &number.to_string(),
        &repo_flag(repo),
    ]);
    if let Some(branch) = branch {
        command.arg(format!("--branch={branch}"));
    }
    command
}

/// Checks the pull request's head out into a local branch in `workdir`, as
/// `gh pr checkout` does (fetching a fork's branch too). Local only. `branch`
/// names that local branch; without it gh reuses the head branch's name.
pub(crate) fn checkout(
    workdir: &Path,
    repo: &str,
    number: u64,
    branch: Option<&str>,
) -> Result<(), PrError> {
    run(checkout_command(workdir, repo, number, branch), None).map(|_| ())
}

fn merge_command(workdir: &Path, repo: &str, number: u64, request: &MergeRequest) -> Command {
    let mut command = gh(workdir);
    command.args([
        "pr",
        "merge",
        &number.to_string(),
        &repo_flag(repo),
        request.method.flag(),
        &format!("--match-head-commit={}", request.head_oid),
    ]);
    // With `--repo` gh leaves the local repository alone: only the remote
    // branch goes.
    if request.delete_branch {
        command.arg("--delete-branch");
    }
    command
}

/// Merges on GitHub. Nothing here runs without the merge dialog's explicit
/// confirm.
pub(crate) fn merge(
    workdir: &Path,
    repo: &str,
    number: u64,
    request: &MergeRequest,
) -> Result<(), PrError> {
    if !is_object_id(&request.head_oid) {
        return Err(PrError::Failed(format!(
            "#{number}'s head commit isn't known yet"
        )));
    }
    run(merge_command(workdir, repo, number, request), None).map(|_| ())
}

/// Posts a review and all its line comments in one call, pinned to
/// `commit_id` (the head that was reviewed). Nothing here runs without the
/// submit dialog's explicit confirm.
pub(crate) fn create_review(
    workdir: &Path,
    repo: &str,
    number: u64,
    commit_id: &str,
    kind: ReviewKind,
    body: &str,
    comments: &[ReviewComment],
) -> Result<(), PrError> {
    if !is_object_id(commit_id) {
        return Err(PrError::Failed(format!(
            "#{number}'s reviewed commit isn't known"
        )));
    }
    if !is_repo_slug(repo) {
        return Err(PrError::Failed(format!(
            "{repo} isn't a GitHub repository name"
        )));
    }
    let payload = review_payload(commit_id, kind, body, comments).to_string();
    api_post(
        workdir,
        &format!("repos/{repo}/pulls/{number}/reviews"),
        &payload,
    )
}

/// Posts a reply to a review thread, answering its first comment.
pub(crate) fn reply_to_thread(
    workdir: &Path,
    repo: &str,
    number: u64,
    root_id: u64,
    body: &str,
) -> Result<(), PrError> {
    if !is_repo_slug(repo) {
        return Err(PrError::Failed(format!(
            "{repo} isn't a GitHub repository name"
        )));
    }
    let payload = serde_json::json!({ "body": body }).to_string();
    api_post(
        workdir,
        &format!("repos/{repo}/pulls/{number}/comments/{root_id}/replies"),
        &payload,
    )
}

/// The pull request's review threads already on GitHub, every page of them.
pub(crate) fn list_review_threads(
    workdir: &Path,
    repo: &str,
    number: u64,
) -> Result<Vec<ReviewThread>, PrError> {
    if !is_repo_slug(repo) {
        return Err(PrError::Failed(format!(
            "{repo} isn't a GitHub repository name"
        )));
    }
    let mut command = gh(workdir);
    command.args([
        "api",
        "--hostname=github.com",
        &format!("repos/{repo}/pulls/{number}/comments?per_page=100"),
        "--paginate",
        "--slurp",
    ]);
    let pages: Vec<Vec<RawReviewComment>> = parse_json(&run(command, None)?)?;
    let mut threads = review_threads(pages.into_iter().flatten().collect());
    match review_thread_statuses(workdir, repo, number) {
        Ok(statuses) => {
            let missing = apply_review_thread_statuses(&mut threads, &statuses);
            if !missing.is_empty() {
                eprintln!(
                    "GitHub returned no status for review threads {missing:?} of {repo}#{number}"
                );
            }
        }
        Err(err) => eprintln!("Couldn't load review thread statuses for {repo}#{number}: {err}"),
    }
    Ok(threads)
}

fn apply_review_thread_statuses(
    threads: &mut [ReviewThread],
    statuses: &std::collections::HashMap<u64, ReviewThreadStatus>,
) -> Vec<u64> {
    let mut missing = Vec::new();
    for thread in threads {
        let Some(status) = statuses.get(&thread.root_id) else {
            missing.push(thread.root_id);
            continue;
        };
        thread.is_resolved = status.is_resolved;
        thread.is_outdated = status.is_outdated;
    }
    missing
}

const REVIEW_THREAD_STATUSES_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!, $endCursor: String) { repository(owner: $owner, name: $name) { pullRequest(number: $number) { reviewThreads(first: 100, after: $endCursor) { pageInfo { hasNextPage endCursor } nodes { isResolved isOutdated comments(first: 1) { nodes { databaseId } } } } } } }";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReviewThreadStatus {
    is_resolved: bool,
    is_outdated: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReviewThreadStatusPage {
    data: RawReviewThreadStatusData,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReviewThreadStatusData {
    repository: RawReviewThreadStatusRepository,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReviewThreadStatusRepository {
    pull_request: RawReviewThreadStatusPullRequest,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReviewThreadStatusPullRequest {
    review_threads: RawReviewThreadStatusConnection,
}

#[derive(Deserialize)]
struct RawReviewThreadStatusConnection {
    #[serde(default)]
    nodes: Vec<RawReviewThreadStatusNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReviewThreadStatusNode {
    is_resolved: bool,
    is_outdated: bool,
    comments: RawReviewThreadRootComments,
}

#[derive(Deserialize)]
struct RawReviewThreadRootComments {
    #[serde(default)]
    nodes: Vec<RawReviewThreadRootComment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReviewThreadRootComment {
    #[serde(default)]
    database_id: Option<u64>,
}

fn parse_review_thread_statuses(
    stdout: &[u8],
) -> Result<std::collections::HashMap<u64, ReviewThreadStatus>, PrError> {
    let unexpected =
        |err: serde_json::Error| PrError::Failed(format!("unexpected gh output: {err}"));
    let mut statuses = std::collections::HashMap::new();
    let mut has_page = false;
    for value in serde_json::Deserializer::from_slice(stdout).into_iter::<serde_json::Value>() {
        let value = value.map_err(unexpected)?;
        let pages = match value {
            serde_json::Value::Array(pages) => pages,
            page => vec![page],
        };
        for value in pages {
            let page: RawReviewThreadStatusPage =
                serde_json::from_value(value).map_err(unexpected)?;
            has_page = true;
            for node in page.data.repository.pull_request.review_threads.nodes {
                if let Some(database_id) = node.comments.nodes.first().and_then(|c| c.database_id) {
                    statuses.insert(
                        database_id,
                        ReviewThreadStatus {
                            is_resolved: node.is_resolved,
                            is_outdated: node.is_outdated,
                        },
                    );
                }
            }
        }
    }
    if !has_page {
        return Err(PrError::Failed(
            "unexpected gh output: no review thread status pages".to_string(),
        ));
    }
    Ok(statuses)
}

/// The resolved and outdated state of every review thread, fetched with
/// GitHub's paginated GraphQL connection and keyed by the REST root comment id.
fn review_thread_statuses(
    workdir: &Path,
    repo: &str,
    number: u64,
) -> Result<std::collections::HashMap<u64, ReviewThreadStatus>, PrError> {
    let (owner, name) = graphql_target(repo, number)?;
    let mut command = gh(workdir);
    command.args([
        "api",
        "graphql",
        "--hostname=github.com",
        "--paginate",
        &format!("--raw-field=query={REVIEW_THREAD_STATUSES_QUERY}"),
        &format!("--raw-field=owner={owner}"),
        &format!("--raw-field=name={name}"),
        &format!("--field=number={number}"),
    ]);
    parse_review_thread_statuses(&run(command, None)?)
}

/// A review you submitted on a pull request, as GitHub's REST API lists it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct LastReview {
    /// `APPROVED`, `CHANGES_REQUESTED`, `COMMENTED` or `DISMISSED`.
    pub(crate) state: String,
    /// The summary, untrusted plain text.
    pub(crate) body: String,
    /// ISO 8601, as GitHub gives it.
    pub(crate) submitted_at: String,
    /// The head commit the review was made on; may be empty.
    pub(crate) commit_id: String,
}

impl LastReview {
    pub(crate) fn verdict(&self) -> &'static str {
        match self.state.as_str() {
            "APPROVED" => "Approved",
            "CHANGES_REQUESTED" => "Changes requested",
            "DISMISSED" => "Dismissed",
            _ => "Commented",
        }
    }
}

#[derive(Deserialize)]
struct RawRestReview {
    #[serde(default)]
    user: Option<Author>,
    #[serde(default)]
    state: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    submitted_at: Option<String>,
    #[serde(default)]
    commit_id: Option<String>,
}

/// `login`'s latest submitted review among the pages; a pending one isn't
/// submitted, a dismissed one still counts.
fn latest_review_by(pages: Vec<Vec<RawRestReview>>, login: &str) -> Option<LastReview> {
    pages
        .into_iter()
        .flatten()
        .filter(|review| {
            review.state != "PENDING"
                && review
                    .user
                    .as_ref()
                    .is_some_and(|user| user.login.eq_ignore_ascii_case(login))
        })
        .map(|review| LastReview {
            state: review.state,
            body: review
                .body
                .unwrap_or_default()
                .trim()
                .chars()
                .take(MAX_CONVERSATION_BODY_CHARS)
                .collect(),
            submitted_at: review.submitted_at.unwrap_or_default(),
            commit_id: review.commit_id.unwrap_or_default(),
        })
        // ISO 8601 in one format sorts as text; a tie keeps the later one.
        .max_by(|a, b| a.submitted_at.cmp(&b.submitted_at))
}

/// The signed-in gh account's login. Asked each time rather than cached:
/// `gh auth switch` changes it under a running app, and this is one small
/// call per review opened.
fn viewer_login(workdir: &Path) -> Result<String, PrError> {
    #[derive(Deserialize)]
    struct User {
        login: String,
    }
    let mut command = gh(workdir);
    command.args(["api", "--hostname=github.com", "user"]);
    let user: User = parse_json(&run(command, None)?)?;
    Ok(user.login)
}

/// Your latest submitted review of the pull request, if you ever reviewed it.
pub(crate) fn last_review(
    workdir: &Path,
    repo: &str,
    number: u64,
) -> Result<Option<LastReview>, PrError> {
    if !is_repo_slug(repo) {
        return Err(PrError::Failed(format!(
            "{repo} isn't a GitHub repository name"
        )));
    }
    let login = viewer_login(workdir)?;
    let mut command = gh(workdir);
    command.args([
        "api",
        "--hostname=github.com",
        &format!("repos/{repo}/pulls/{number}/reviews?per_page=100"),
        "--paginate",
        "--slurp",
    ]);
    let pages: Vec<Vec<RawRestReview>> = parse_json(&run(command, None)?)?;
    Ok(latest_review_by(pages, &login))
}

/// Your viewed state of a pull request file, as GitHub keeps it per user.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ViewedState {
    Viewed,
    Unviewed,
    /// Viewed, then changed by a later commit.
    Dismissed,
}

/// The pull request's node id, which the viewed mutations take, and your
/// viewed state of each of its files.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ViewedStates {
    pub(crate) pr_id: String,
    pub(crate) files: std::collections::BTreeMap<String, ViewedState>,
}

const VIEWED_STATES_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!, $endCursor: String) { repository(owner: $owner, name: $name) { pullRequest(number: $number) { id files(first: 100, after: $endCursor) { pageInfo { hasNextPage endCursor } nodes { path viewerViewedState } } } } }";

/// GitHub's node ids are opaque but plain: nothing else reaches a mutation.
fn is_node_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '='))
}

/// `owner/name` and a number GraphQL's 32-bit Int can carry.
fn graphql_target(repo: &str, number: u64) -> Result<(&str, &str), PrError> {
    match repo.split_once('/') {
        Some((owner, name)) if is_repo_slug(repo) && number <= i32::MAX as u64 => Ok((owner, name)),
        _ => Err(PrError::Failed(format!(
            "{repo}#{number} isn't a GitHub pull request"
        ))),
    }
}

/// Your viewed state of every file of the pull request, every page of them.
pub(crate) fn viewed_states(
    workdir: &Path,
    repo: &str,
    number: u64,
) -> Result<ViewedStates, PrError> {
    let (owner, name) = graphql_target(repo, number)?;
    let mut command = gh(workdir);
    command.args([
        "api",
        "graphql",
        "--hostname=github.com",
        "--paginate",
        &format!("--raw-field=query={VIEWED_STATES_QUERY}"),
        &format!("--raw-field=owner={owner}"),
        &format!("--raw-field=name={name}"),
        &format!("--field=number={number}"),
    ]);
    parse_viewed_states(&run(command, None)?)
}

/// gh's paginated GraphQL output: one JSON object per page, one after the
/// other (or, with `--slurp`, an array of them).
fn parse_viewed_states(stdout: &[u8]) -> Result<ViewedStates, PrError> {
    #[derive(Deserialize)]
    struct Page {
        data: Data,
    }
    #[derive(Deserialize)]
    struct Data {
        repository: Repository,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Repository {
        pull_request: PullRequest,
    }
    #[derive(Deserialize)]
    struct PullRequest {
        id: String,
        files: Files,
    }
    #[derive(Deserialize)]
    struct Files {
        #[serde(default)]
        nodes: Vec<Option<File>>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct File {
        path: String,
        viewer_viewed_state: String,
    }
    let unexpected =
        |err: serde_json::Error| PrError::Failed(format!("unexpected gh output: {err}"));
    let mut pages: Vec<Page> = Vec::new();
    for value in serde_json::Deserializer::from_slice(stdout).into_iter::<serde_json::Value>() {
        match value.map_err(unexpected)? {
            serde_json::Value::Array(items) => {
                for item in items {
                    pages.push(serde_json::from_value(item).map_err(unexpected)?);
                }
            }
            item => pages.push(serde_json::from_value(item).map_err(unexpected)?),
        }
    }
    let mut states = ViewedStates::default();
    for page in pages {
        let pr = page.data.repository.pull_request;
        states.pr_id = pr.id;
        for file in pr.files.nodes.into_iter().flatten() {
            let state = match file.viewer_viewed_state.as_str() {
                "VIEWED" => ViewedState::Viewed,
                "DISMISSED" => ViewedState::Dismissed,
                _ => ViewedState::Unviewed,
            };
            states.files.insert(file.path, state);
        }
    }
    if !is_node_id(&states.pr_id) {
        return Err(PrError::Failed(
            "GitHub returned no pull request id".to_string(),
        ));
    }
    Ok(states)
}

/// Marks one file of the pull request viewed on GitHub, or not.
pub(crate) fn set_file_viewed(
    workdir: &Path,
    pr_id: &str,
    path: &str,
    viewed: bool,
) -> Result<(), PrError> {
    if !is_node_id(pr_id) || path.is_empty() {
        return Err(PrError::Failed(
            "not a pull request file GitHub knows".to_string(),
        ));
    }
    let mutation = if viewed {
        "markFileAsViewed"
    } else {
        "unmarkFileAsViewed"
    };
    let mut command = gh(workdir);
    command.args([
        "api",
        "graphql",
        "--hostname=github.com",
        &format!(
            "--raw-field=query=mutation($id: ID!, $path: String!) {{ {mutation}(input: {{pullRequestId: $id, path: $path}}) {{ clientMutationId }} }}"
        ),
        &format!("--raw-field=id={pr_id}"),
        // A raw field: gh never reads a path starting with `@` as a file.
        &format!("--raw-field=path={path}"),
    ]);
    run(command, None).map(drop)
}

/// `gh api --method=POST` with a JSON body on stdin. A refusal reports the
/// API's own explanation, which gh prints as JSON on stdout.
fn api_post(workdir: &Path, path: &str, payload: &str) -> Result<(), PrError> {
    let mut command = gh(workdir);
    command.args([
        "api",
        "--hostname=github.com",
        path,
        "--method=POST",
        "--input=-",
    ]);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => PrError::GhMissing,
        _ => PrError::Failed(err.to_string()),
    })?;
    if let Some(mut pipe) = child.stdin.take() {
        // A write error surfaces as the child's own failure below.
        let _ = pipe.write_all(payload.as_bytes());
    }
    let output = child
        .wait_with_output()
        .map_err(|err| PrError::Failed(err.to_string()))?;
    if output.status.success() {
        return Ok(());
    }
    // gh prints the API's explanation (which line GitHub refused, and why)
    // as JSON on stdout; stderr only has the status line.
    let detail = serde_json::from_slice::<serde_json::Value>(&output.stdout)
        .ok()
        .map(|value| {
            let mut parts: Vec<String> = Vec::new();
            if let Some(message) = value["message"].as_str() {
                parts.push(message.to_string());
            }
            if let Some(errors) = value["errors"].as_array() {
                parts.extend(errors.iter().filter_map(|error| {
                    error
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| error["message"].as_str().map(str::to_string))
                }));
            }
            parts.join(": ")
        })
        .filter(|detail| !detail.is_empty());
    match (
        classify_failure(String::from_utf8_lossy(&output.stderr).trim()),
        detail,
    ) {
        (PrError::GhSignedOut, _) => Err(PrError::GhSignedOut),
        (_, Some(detail)) => Err(PrError::Failed(detail)),
        (err, None) => Err(err),
    }
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

/// What `gh pr create --fill` would put in the form, worked out from local
/// git: the remote's default branch as the base, a title and body from the
/// branch's own commits.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct NewPullRequestDefaults {
    pub(crate) base: Option<String>,
    pub(crate) title: String,
    pub(crate) body: String,
}

pub(crate) fn new_pull_request_defaults(
    workdir: &Path,
    remote: &str,
    branch: &str,
) -> NewPullRequestDefaults {
    let base = default_branch(workdir, remote);
    let commits = base
        .as_deref()
        .map(|base| branch_commits(workdir, remote, base, branch))
        .unwrap_or_default();
    let (title, body) = fill_from_commits(&commits, branch);
    NewPullRequestDefaults { base, title, body }
}

/// `refs/remotes/<remote>/HEAD`, as clone sets it; else main or master.
fn default_branch(workdir: &Path, remote: &str) -> Option<String> {
    let mut command = git(workdir);
    command.args([
        "symbolic-ref",
        "--quiet",
        "--short",
        &format!("refs/remotes/{remote}/HEAD"),
    ]);
    if let Ok(out) = run_git(command) {
        let target = String::from_utf8_lossy(&out).trim().to_string();
        if let Some(base) = target.strip_prefix(&format!("{remote}/"))
            && !base.is_empty()
        {
            return Some(base.to_string());
        }
    }
    ["main", "master"]
        .into_iter()
        .find(|name| {
            let mut command = git(workdir);
            command.args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/remotes/{remote}/{name}"),
            ]);
            run_git(command).is_ok()
        })
        .map(str::to_string)
}

/// The branch's own commits over the base, oldest first: subject and body.
fn branch_commits(workdir: &Path, remote: &str, base: &str, branch: &str) -> Vec<(String, String)> {
    let mut command = git(workdir);
    command.args([
        "log",
        "--no-merges",
        "--reverse",
        "--max-count=50",
        "--format=%s%x1f%b%x1e",
        &format!("refs/remotes/{remote}/{base}..refs/heads/{branch}"),
    ]);
    let Ok(out) = run_git(command) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out)
        .split('\u{1e}')
        .filter_map(|record| {
            let (subject, body) = record.trim_start_matches('\n').split_once('\u{1f}')?;
            Some((subject.trim().to_string(), body.trim().to_string()))
        })
        .collect()
}

/// gh's `--fill`: one commit gives its own subject and body; several give the
/// branch's name as the title and their subjects as the body.
fn fill_from_commits(commits: &[(String, String)], branch: &str) -> (String, String) {
    if let [(subject, body)] = commits {
        return (subject.clone(), body.clone());
    }
    let name = branch
        .rsplit('/')
        .next()
        .unwrap_or(branch)
        .replace(['-', '_'], " ");
    let mut chars = name.trim().chars();
    let title = chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    let body = commits
        .iter()
        .map(|(subject, _)| format!("- {subject}"))
        .collect::<Vec<_>>()
        .join("\n");
    (title, body)
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

/// Fetches whichever of `oids` aren't local, by object id only, so no ref,
/// branch, FETCH_HEAD or working-tree file changes.
fn fetch_missing(workdir: &Path, remote: &str, oids: &[&str]) -> Result<(), PrError> {
    let missing: Vec<&str> = oids
        .iter()
        .copied()
        .filter(|oid| !has_commit(workdir, oid))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
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
    run_git(command).map(drop)
}

/// What changed from one head of a pull request to a later one.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ChangesSince {
    /// Paths changed between the two commits, whole-repository.
    pub(crate) files: std::collections::BTreeSet<String>,
    /// Commits on the new head that the old one lacks.
    pub(crate) commits: usize,
}

/// Why the changes since a commit couldn't be worked out.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SinceFailure {
    /// GitHub no longer has the old commit: a force push dropped it.
    Gone,
    /// Anything else (network, a locked key, git itself); worth retrying.
    Failed(String),
}

/// The files changed from `old_oid` to `new_oid`, fetching either by object id
/// when it isn't local.
pub(crate) fn changes_since(
    workdir: &Path,
    remote: &str,
    old_oid: &str,
    new_oid: &str,
) -> Result<ChangesSince, SinceFailure> {
    let failed = |err: PrError| SinceFailure::Failed(err.to_string());
    if !is_object_id(old_oid) || !is_object_id(new_oid) {
        return Err(SinceFailure::Failed(
            "GitHub returned an unexpected commit id".to_string(),
        ));
    }
    if let Err(err) = fetch_missing(workdir, remote, &[old_oid, new_oid]) {
        // Only GitHub refusing the object means it's gone; a failed
        // connection says nothing about it.
        let message = err.to_string().to_lowercase();
        let refused = [
            "not our ref",
            "unadvertised object",
            "couldn't find remote ref",
        ]
        .iter()
        .any(|reason| message.contains(reason));
        return Err(if refused && !has_commit(workdir, old_oid) {
            SinceFailure::Gone
        } else {
            failed(err)
        });
    }
    let run_git = |command| run_git(command).map_err(failed);
    let mut command = git(workdir);
    command.args([
        "diff",
        "--name-only",
        "--no-renames",
        "--no-ext-diff",
        "-z",
        old_oid,
        new_oid,
        "--",
    ]);
    let files = parse_name_list(&run_git(command)?);
    let mut command = git(workdir);
    command.args(["rev-list", "--count", &format!("{old_oid}..{new_oid}")]);
    let commits = String::from_utf8_lossy(&run_git(command)?)
        .trim()
        .parse()
        .unwrap_or(0);
    Ok(ChangesSince { files, commits })
}

/// A contiguous selection of PR commits, from the parent of its oldest
/// commit through its newest commit. Net changes are limited to GitHub's PR
/// file list; first-parent non-merge commits also retain reverted paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommitRangeChanges {
    pub(crate) base_oid: String,
    pub(crate) files: std::collections::BTreeSet<String>,
}

pub(crate) fn commit_range_changes(
    workdir: &Path,
    remote: &str,
    oldest_oid: &str,
    newest_oid: &str,
    pr_files: &[String],
) -> Result<CommitRangeChanges, PrError> {
    if !is_object_id(oldest_oid) || !is_object_id(newest_oid) {
        return Err(PrError::Failed(
            "GitHub returned an unexpected commit id".to_string(),
        ));
    }
    fetch_missing(workdir, remote, &[oldest_oid, newest_oid])?;
    let mut parent = git(workdir);
    parent.args(["rev-parse", "--verify", &format!("{oldest_oid}^")]);
    let base_oid = String::from_utf8_lossy(&run_git(parent)?)
        .trim()
        .to_string();
    if !is_object_id(&base_oid) {
        return Err(PrError::Failed(
            "Couldn't find the selected commit's parent".to_string(),
        ));
    }
    let mut diff = git(workdir);
    diff.args([
        "diff",
        "--name-only",
        "--no-renames",
        "--no-ext-diff",
        "-z",
        &base_oid,
        newest_oid,
        "--",
    ]);
    let allowed: std::collections::BTreeSet<&str> = pr_files.iter().map(String::as_str).collect();
    let mut files: std::collections::BTreeSet<String> = parse_name_list(&run_git(diff)?)
        .into_iter()
        .filter(|path| allowed.contains(path.as_str()))
        .collect();
    let mut log = git(workdir);
    log.args([
        "log",
        "--first-parent",
        "--no-merges",
        "--format=",
        "--name-only",
        "--no-renames",
        "-z",
        &format!("{base_oid}..{newest_oid}"),
        "--",
    ]);
    files.extend(parse_name_list(&run_git(log)?));
    if files.len() > MAX_LISTED_FILES as usize {
        return Err(PrError::Failed(format!(
            "This commit range changes more than {MAX_LISTED_FILES} files; review it on GitHub."
        )));
    }
    Ok(CommitRangeChanges { base_oid, files })
}

/// The head-side line ranges (first, last) of `path`'s hunks in the pull
/// request's own diff, with GitHub's 3 lines of context: the lines GitHub
/// takes a head-side comment on.
pub(crate) fn pr_hunk_ranges(
    workdir: &Path,
    merge_base: &str,
    head: &str,
    path: &str,
) -> Result<Vec<(u32, u32)>, PrError> {
    if !is_object_id(merge_base) || !is_object_id(head) {
        return Err(PrError::Failed(
            "GitHub returned an unexpected commit id".to_string(),
        ));
    }
    let mut command = git(workdir);
    // Literal: a path from GitHub is never pathspec magic.
    command.args([
        "--literal-pathspecs",
        "diff",
        "-U3",
        // Config can't widen the hunks past GitHub's.
        "--inter-hunk-context=0",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--no-renames",
        merge_base,
        head,
        "--",
        path,
    ]);
    Ok(parse_new_hunk_ranges(&String::from_utf8_lossy(&run_git(
        command,
    )?)))
}

/// `@@ -a,b +c,d @@` headers as head-side ranges; an empty side (`d` 0) has
/// no lines to comment on.
fn parse_new_hunk_ranges(diff: &str) -> Vec<(u32, u32)> {
    diff.lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("@@ -")?;
            let new = rest.split_once(" +")?.1.split_once(" @@")?.0;
            let (start, count) = match new.split_once(',') {
                Some((start, count)) => (start.parse::<u32>().ok()?, count.parse::<u32>().ok()?),
                None => (new.parse::<u32>().ok()?, 1),
            };
            (count > 0).then(|| (start, start + count - 1))
        })
        .collect()
}

/// `git diff --name-only -z` output as paths.
fn parse_name_list(stdout: &[u8]) -> std::collections::BTreeSet<String> {
    stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect()
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
    fetch_missing(workdir, remote, &[base_oid, head_oid])?;
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
    #[test]
    fn review_decision_ranks_changes_requested_over_a_pending_request() {
        let requests: Vec<super::RawReviewRequest> =
            serde_json::from_str(r#"[{"requestedReviewer": {"login": "bob"}}]"#)
                .expect("requests");
        let reviews: Vec<super::RawLatestReview> = serde_json::from_str(
            r#"[{"author": {"login": "alice"}, "state": "CHANGES_REQUESTED"}]"#,
        )
        .expect("reviews");
        assert_eq!(
            super::ReviewDecision::from_reviews("", &requests, &reviews, "pr-author"),
            Some(super::ReviewDecision::ChangesRequested)
        );
    }

    #[test]
    fn review_decision_is_review_required_with_only_a_pending_request() {
        let requests: Vec<super::RawReviewRequest> =
            serde_json::from_str(r#"[{"requestedReviewer": {"login": "bob"}}]"#)
                .expect("requests");
        assert_eq!(
            super::ReviewDecision::from_reviews("", &requests, &[], "pr-author"),
            Some(super::ReviewDecision::ReviewRequired)
        );
    }

    #[test]
    fn review_decision_ignores_the_authors_own_comment() {
        let reviews: Vec<super::RawLatestReview> = serde_json::from_str(
            r#"[{"author": {"login": "pr-author"}, "state": "COMMENTED"}]"#,
        )
        .expect("reviews");
        assert_eq!(
            super::ReviewDecision::from_reviews("", &[], &reviews, "pr-author"),
            None
        );
    }

    #[test]
    fn your_last_review_is_your_latest_submitted_one() {
        let pages: Vec<Vec<super::RawRestReview>> = serde_json::from_str(
            r#"[[
              {"user": {"login": "Me"}, "state": "APPROVED", "body": " ok ", "submitted_at": "2026-01-02T00:00:00Z", "commit_id": "a"},
              {"user": {"login": "other"}, "state": "COMMENTED", "body": "", "submitted_at": "2026-03-01T00:00:00Z", "commit_id": "b"},
              {"user": null, "state": "COMMENTED", "body": null, "submitted_at": null, "commit_id": null}
            ], [
              {"user": {"login": "me"}, "state": "DISMISSED", "body": "old", "submitted_at": "2026-02-01T00:00:00Z", "commit_id": "c"},
              {"user": {"login": "me"}, "state": "PENDING", "body": "draft", "commit_id": "d"}
            ]]"#,
        )
        .expect("pages");
        let last = super::latest_review_by(pages, "me").expect("a review");
        assert_eq!(
            (
                last.state.as_str(),
                last.body.as_str(),
                last.commit_id.as_str()
            ),
            ("DISMISSED", "old", "c")
        );
        assert_eq!(last.verdict(), "Dismissed");
        assert_eq!(super::latest_review_by(Vec::new(), "me"), None);
        let only_pending: Vec<Vec<super::RawRestReview>> = serde_json::from_str(
            r#"[[{"user": {"login": "me"}, "state": "PENDING", "body": ""}]]"#,
        )
        .expect("pages");
        assert_eq!(super::latest_review_by(only_pending, "me"), None);
    }

    #[test]
    fn viewed_states_read_every_page() {
        use super::ViewedState::*;
        let page = |path: &str, state: &str| {
            format!(
                r#"{{"data":{{"repository":{{"pullRequest":{{"id":"PR_kw1","files":{{"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[{{"path":"{path}","viewerViewedState":"{state}"}}]}}}}}}}}}}"#
            )
        };
        // One object per page, as `--paginate` prints them.
        let stream = format!("{}\n{}", page("a.rs", "VIEWED"), page("b.rs", "DISMISSED"));
        let states = super::parse_viewed_states(stream.as_bytes()).expect("states");
        assert_eq!(states.pr_id, "PR_kw1");
        assert_eq!(
            states.files.into_iter().collect::<Vec<_>>(),
            [
                ("a.rs".to_string(), Viewed),
                ("b.rs".to_string(), Dismissed)
            ]
        );
        // `--slurp`'s array of pages reads the same.
        let slurped = format!("[{}]", page("c.rs", "UNVIEWED"));
        let states = super::parse_viewed_states(slurped.as_bytes()).expect("states");
        assert_eq!(states.files.get("c.rs"), Some(&Unviewed));
        assert!(super::parse_viewed_states(b"").is_err());
        assert!(
            super::parse_viewed_states(
                br#"{"data":{"repository":{"pullRequest":{"id":"a b","files":{"nodes":[]}}}}}"#
            )
            .is_err()
        );
        assert!(super::graphql_target("o/r", 1 << 40).is_err());
        assert!(super::graphql_target("o/../r", 1).is_err());
        assert_eq!(super::graphql_target("o/r", 7).ok(), Some(("o", "r")));
    }

    #[test]
    fn hunk_headers_give_the_head_side_lines() {
        let diff = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,3 +1,4 @@ fn x\n a\n+b\n@@ -20 +21 @@\n-c\n+d\n@@ -30,2 +32,0 @@\n-e\n-f\n";
        assert_eq!(super::parse_new_hunk_ranges(diff), [(1, 4), (21, 21)]);
        assert!(super::parse_new_hunk_ranges("").is_empty());
    }

    #[test]
    fn outdated_threads_keep_the_line_they_were_written_on() {
        let raw: Vec<super::RawReviewComment> = serde_json::from_str(
            r#"[{"id": 1, "path": "a.rs", "line": null, "original_line": 12, "body": "old"},
                {"id": 2, "path": "a.rs", "line": 3, "original_line": 3, "body": "now"},
                {"id": 3, "path": "a.rs", "line": null, "original_line": null, "body": "whole file"}]"#,
        )
        .expect("comments");
        let threads = super::review_threads(raw);
        let outdated: Vec<_> = threads
            .iter()
            .filter(|thread| thread.outdated())
            .map(|thread| (thread.root_id, thread.original_line))
            .collect();
        assert_eq!(outdated, [(1, Some(12))]);
    }

    #[test]
    fn graphql_thread_statuses_match_rest_roots_and_override_the_rest_heuristic() {
        let raw: Vec<super::RawReviewComment> = serde_json::from_str(
            r#"[
                {"id": 1, "path": "a.rs", "line": null, "original_line": 12, "body": "old"},
                {"id": 2, "path": "b.rs", "line": 4, "original_line": 4, "body": "current"}
            ]"#,
        )
        .expect("REST comments");
        let mut threads = super::review_threads(raw);
        assert!(threads[0].outdated());
        let page = |id, is_resolved, is_outdated| {
            serde_json::json!({
                "data": {"repository": {"pullRequest": {"reviewThreads": {
                    "pageInfo": {"hasNextPage": false, "endCursor": null},
                    "nodes": [{
                        "isResolved": is_resolved,
                        "isOutdated": is_outdated,
                        "comments": {"nodes": [{"databaseId": id}]}
                    }]
                }}}}
            })
            .to_string()
        };
        let pages = format!("{}\n{}", page(1, true, false), page(2, false, true));
        let statuses = super::parse_review_thread_statuses(pages.as_bytes()).expect("statuses");
        assert!(super::apply_review_thread_statuses(&mut threads, &statuses).is_empty());

        let first = threads.iter().find(|thread| thread.root_id == 1).unwrap();
        assert!(first.is_resolved);
        assert!(!first.is_outdated);
        assert!(!first.outdated());
        let second = threads.iter().find(|thread| thread.root_id == 2).unwrap();
        assert!(!second.is_resolved);
        assert!(second.is_outdated);
        assert!(second.outdated());
        let raw: Vec<super::RawReviewComment> = serde_json::from_str(
            r#"[{"id": 1, "path": "a.rs", "line": null, "original_line": 12, "body": "old"},
                {"id": 2, "path": "b.rs", "line": 4, "original_line": 4, "body": "current"}]"#,
        )
        .expect("REST comments");
        let mut partial = super::review_threads(raw);
        let statuses = std::collections::HashMap::from([(
            2,
            super::ReviewThreadStatus {
                is_resolved: true,
                is_outdated: true,
            },
        )]);
        assert_eq!(
            super::apply_review_thread_statuses(&mut partial, &statuses),
            vec![1]
        );
        assert!(!partial[0].is_resolved);
        assert!(partial[0].outdated());
        assert!(partial[1].is_resolved);
        assert!(partial[1].is_outdated);
    }

    #[test]
    fn changed_paths_read_nul_separated() {
        let files = super::parse_name_list(b"a.rs\0dir/b c.rs\0\0");
        assert_eq!(
            files.into_iter().collect::<Vec<_>>(),
            ["a.rs".to_string(), "dir/b c.rs".to_string()]
        );
        assert!(super::parse_name_list(b"").is_empty());
    }

    #[test]
    fn fill_takes_one_commit_whole_and_names_several_by_branch() {
        let one = vec![("Fix the thing".to_string(), "Because.".to_string())];
        assert_eq!(
            fill_from_commits(&one, "feat/x"),
            ("Fix the thing".to_string(), "Because.".to_string())
        );
        let two = vec![
            ("First".to_string(), String::new()),
            ("Second".to_string(), "body".to_string()),
        ];
        assert_eq!(
            fill_from_commits(&two, "feat/pr-actions_v2"),
            ("Pr actions v2".to_string(), "- First\n- Second".to_string())
        );
        assert_eq!(
            fill_from_commits(&[], "dev"),
            ("Dev".to_string(), String::new())
        );
    }

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
    fn review_summary_distinguishes_comment_only_and_requested_again() {
        let raw: Vec<RawSummary> = parse_json(
            br#"[{
            "number":1,"title":"one","headRefName":"one","baseRefName":"main",
            "latestReviews":[{"state":"COMMENTED"}]
        },{
            "number":2,"title":"two","headRefName":"two","baseRefName":"main",
            "reviewRequests":[{"login":"reviewer"}],
            "latestReviews":[{"state":"APPROVED"}]
        }]"#,
        )
        .expect("reviews");
        let prs: Vec<PullRequestSummary> = raw.into_iter().map(Into::into).collect();
        assert_eq!(prs[0].review, Some(ReviewDecision::Commented));
        assert_eq!(prs[1].review, Some(ReviewDecision::ReviewRequired));
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
        // 214 files and 20,001 changed lines: too much for one Codex prompt,
        // but reviewable here, a page of files at a time.
        assert!(detail.too_large_for_codex());
        assert!(!detail.too_large_for_app());
    }

    #[test]
    fn detail_json_maps_and_deduplicates_reviewers() {
        let json = r#"{
            "number": 9, "title": "t", "url": "u", "headRefName": "h", "headRefOid": "a",
            "baseRefName": "b", "baseRefOid": "c",
            "reviewRequests": [
                {"__typename": "User", "login": "Octo"},
                {"requestedReviewer": {"__typename": "Team", "name": "Platform"}}
            ],
            "latestReviews": [
                {"author": {"login": "octo"}, "state": "APPROVED", "submittedAt": "2026-09-01T00:00:00Z"},
                {"author": {"login": "zed"}, "state": "COMMENTED", "submittedAt": "2026-09-02T00:00:00Z"},
                {"author": null, "state": "CHANGES_REQUESTED", "submittedAt": "2026-09-03T00:00:00Z"},
                {"author": {"login": "pending"}, "state": "PENDING", "submittedAt": null}
            ]
        }"#;
        let detail: PullRequestDetail = parse_json::<RawDetail>(json.as_bytes())
            .expect("valid detail")
            .into();
        assert_eq!(
            detail
                .reviewers
                .iter()
                .map(|reviewer| (reviewer.login.as_str(), reviewer.status))
                .collect::<Vec<_>>(),
            [
                ("Octo", PrReviewerStatus::Requested),
                ("Platform", PrReviewerStatus::Requested),
                ("zed", PrReviewerStatus::Commented),
            ]
        );
        assert_eq!(
            PrReviewerStatus::ChangesRequested.label(),
            "Changes requested"
        );
    }

    #[test]
    fn detail_json_lists_commits_newest_first() {
        let old = "a".repeat(40);
        let new = "b".repeat(40);
        let json = serde_json::json!({
            "number": 9, "title": "t", "url": "u", "headRefName": "h", "headRefOid": new,
            "baseRefName": "b", "baseRefOid": "c",
            "commits": [
                {"oid": old, "messageHeadline": "first", "committedDate": "2026-09-01T00:00:00Z"},
                {"oid": new, "messageHeadline": "second", "committedDate": "2026-09-02T00:00:00Z"},
                {"oid": "bad", "messageHeadline": "invalid", "committedDate": ""}
            ]
        });
        let detail: PullRequestDetail = parse_json::<RawDetail>(json.to_string().as_bytes())
            .expect("valid detail")
            .into();
        assert_eq!(detail.commits.len(), 2);
        assert_eq!(detail.commits[0].headline, "second");
        assert_eq!(detail.commits[1].headline, "first");
    }

    #[test]
    fn commit_range_starts_at_oldest_parent_and_lists_touched_paths() {
        let dir = tempfile::tempdir().expect("temporary repository");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .expect("git runs");
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        let commit = |message: &str| {
            git(&["add", "--all"]);
            git(&[
                "-c",
                "user.name=Tester",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                message,
            ]);
            git(&["rev-parse", "HEAD"])
        };
        std::fs::write(dir.path().join("a.rs"), "base\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "base\n").unwrap();
        std::fs::write(dir.path().join("reverted.rs"), "base\n").unwrap();
        let base = commit("base");
        std::fs::write(dir.path().join("a.rs"), "oldest\n").unwrap();
        let oldest = commit("oldest");
        std::fs::write(dir.path().join("b.rs"), "newest\n").unwrap();
        commit("middle");
        std::fs::write(dir.path().join("reverted.rs"), "changed\n").unwrap();
        commit("change reverted file");
        std::fs::write(dir.path().join("reverted.rs"), "base\n").unwrap();
        commit("revert file");
        git(&["checkout", "-q", "-b", "feature"]);
        git(&["checkout", "-q", "-b", "upstream", &base]);
        std::fs::write(dir.path().join("main-only.rs"), "imported\n").unwrap();
        commit("upstream change");
        git(&["checkout", "-q", "feature"]);
        git(&[
            "-c",
            "user.name=Tester",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "merge",
            "--no-ff",
            "-q",
            "-m",
            "merge upstream",
            "upstream",
        ]);
        let newest = git(&["rev-parse", "HEAD"]);

        let files = vec!["a.rs".to_string(), "b.rs".to_string()];
        let range = super::commit_range_changes(dir.path(), "unused", &oldest, &newest, &files)
            .expect("local range");
        assert_eq!(range.base_oid, base);
        assert_eq!(
            range.files.into_iter().collect::<Vec<_>>(),
            ["a.rs", "b.rs", "reverted.rs"]
        );
        assert!(
            super::commit_range_changes(dir.path(), "unused", "invalid", &newest, &files).is_err()
        );

        let remote = tempfile::tempdir().expect("remote");
        let remote_path = remote.path().to_str().expect("UTF-8 path");
        git(&["init", "--bare", "-q", remote_path]);
        git(&["push", "-q", remote_path, "HEAD:refs/heads/main"]);
        let client = tempfile::tempdir().expect("client");
        let output = std::process::Command::new("git")
            .arg("init")
            .arg("-q")
            .current_dir(client.path())
            .output()
            .expect("git init");
        assert!(output.status.success());
        let fetched =
            super::commit_range_changes(client.path(), remote_path, &oldest, &newest, &files)
                .expect("fetch missing commits from the remote");
        assert_eq!(fetched.base_oid, base);
        assert_eq!(
            fetched.files.into_iter().collect::<Vec<_>>(),
            ["a.rs", "b.rs", "reverted.rs"]
        );
    }

    #[test]
    fn detail_json_lists_checks_and_the_conversation() {
        let json = r#"{
            "number": 9, "title": "t", "url": "u", "headRefName": "h", "headRefOid": "a",
            "baseRefName": "b", "baseRefOid": "c",
            "statusCheckRollup": [
                {"__typename": "CheckRun", "name": "lint", "status": "COMPLETED", "conclusion": "SUCCESS"},
                {"__typename": "CheckRun", "name": "build", "status": "COMPLETED", "conclusion": "FAILURE"},
                {"__typename": "StatusContext", "context": "ci/deploy", "state": "PENDING"}
            ],
            "comments": [
                {"author": {"login": "bob"}, "body": "Second", "createdAt": "2026-09-02T00:00:00Z"},
                {"author": {"login": "spam"}, "body": "buy", "createdAt": "2026-09-03T00:00:00Z", "isMinimized": true}
            ],
            "reviews": [
                {"author": {"login": "me"}, "body": "draft", "state": "PENDING", "submittedAt": null},
                {"author": {"login": "amy"}, "body": "", "state": "APPROVED", "submittedAt": "2026-09-04T00:00:00Z"},
                {"author": {"login": "cid"}, "body": "", "state": "COMMENTED", "submittedAt": "2026-09-01T00:00:00Z"},
                {"author": {"login": "dee"}, "body": "First", "state": "COMMENTED", "submittedAt": "2026-09-01T00:00:00Z"}
            ]
        }"#;
        let detail: PullRequestDetail = parse_json::<RawDetail>(json.as_bytes())
            .expect("valid detail")
            .into();
        let checks: Vec<_> = detail
            .check_runs
            .iter()
            .map(|run| (run.name.as_str(), run.state))
            .collect();
        assert_eq!(
            checks,
            [
                ("build", CheckState::Failing),
                ("ci/deploy", CheckState::Pending),
                ("lint", CheckState::Passing),
            ]
        );
        // Oldest first; the hidden comment and the body-less inline review drop out.
        let conversation: Vec<_> = detail
            .conversation
            .iter()
            .map(|entry| (entry.author.as_str(), entry.verb, entry.body.as_str()))
            .collect();
        assert_eq!(
            conversation,
            [
                ("dee", "reviewed", "First"),
                ("bob", "commented", "Second"),
                ("amy", "approved", ""),
            ]
        );
    }

    #[test]
    fn checkout_and_merge_name_the_repository_and_never_prompt() {
        let args = |command: Command| -> Vec<String> {
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(
            args(checkout_command(Path::new("."), "o/r", 7, None)),
            ["pr", "checkout", "7", "--repo=github.com/o/r"]
        );
        assert_eq!(
            args(checkout_command(Path::new("."), "o/r", 7, Some("pr/7"))),
            [
                "pr",
                "checkout",
                "7",
                "--repo=github.com/o/r",
                "--branch=pr/7"
            ]
        );
        let head = "a".repeat(40);
        let request = |method, delete_branch| MergeRequest {
            method,
            delete_branch,
            head_oid: head.clone(),
        };
        assert_eq!(
            args(merge_command(
                Path::new("."),
                "o/r",
                7,
                &request(MergeMethod::Squash, true)
            )),
            [
                "pr".to_string(),
                "merge".into(),
                "7".into(),
                "--repo=github.com/o/r".into(),
                "--squash".into(),
                format!("--match-head-commit={head}"),
                "--delete-branch".into(),
            ]
        );
        // An unknown head never reaches gh.
        assert!(
            merge(
                Path::new("."),
                "o/r",
                7,
                &MergeRequest {
                    head_oid: "--admin".into(),
                    ..request(MergeMethod::Merge, false)
                }
            )
            .is_err()
        );
    }

    #[test]
    fn review_payload_carries_ranges_and_skips_an_empty_summary() {
        let comments = [
            ReviewComment {
                anchor: ReviewAnchor {
                    path: "src/a.rs".into(),
                    side: ReviewSide::Right,
                    line: 12,
                    start: Some((ReviewSide::Right, 10)),
                },
                body: "range".into(),
                reply_to: None,
            },
            ReviewComment {
                anchor: ReviewAnchor {
                    path: "src/b.rs".into(),
                    side: ReviewSide::Left,
                    line: 3,
                    start: Some((ReviewSide::Left, 3)),
                },
                body: "one removed line".into(),
                reply_to: None,
            },
        ];
        let payload = review_payload(&"a".repeat(40), ReviewKind::RequestChanges, "  ", &comments);
        assert_eq!(payload["event"], "REQUEST_CHANGES");
        assert!(payload.get("body").is_none());
        assert_eq!(
            payload["comments"][0],
            serde_json::json!({
                "path": "src/a.rs", "side": "RIGHT", "line": 12, "body": "range",
                "start_side": "RIGHT", "start_line": 10,
            })
        );
        // A one-line "range" is a plain line comment.
        assert!(payload["comments"][1].get("start_line").is_none());
        assert_eq!(payload["comments"][1]["side"], "LEFT");
        let modified_line = [ReviewComment {
            anchor: ReviewAnchor {
                path: "src/c.rs".into(),
                side: ReviewSide::Right,
                line: 6,
                start: Some((ReviewSide::Left, 6)),
            },
            body: "old and new line 6".into(),
            reply_to: None,
        }];
        let payload = review_payload(&"a".repeat(40), ReviewKind::Comment, "", &modified_line);
        assert_eq!(payload["comments"][0]["start_side"], "LEFT");
        assert_eq!(payload["comments"][0]["start_line"], 6);
    }

    #[test]
    fn threads_gather_replies_under_their_first_comment() {
        let json = r#"[
            {"id": 2, "in_reply_to_id": 1, "path": "src/a.rs", "line": 4, "side": "RIGHT",
             "body": "Agreed.", "user": {"login": "me"}, "created_at": "2026-09-02T00:00:00Z"},
            {"id": 1, "path": "src/a.rs", "line": 4, "side": "RIGHT",
             "body": "Wrap here?", "user": {"login": "octo"}, "created_at": "2026-09-01T00:00:00Z"},
            {"id": 3, "path": "src/a.rs", "line": null, "side": "LEFT",
             "body": "Outdated one", "user": null, "created_at": "2026-08-01T00:00:00Z"}
        ]"#;
        let raw: Vec<RawReviewComment> = parse_json(json.as_bytes()).expect("valid comments");
        let threads = review_threads(raw);
        assert_eq!(threads.len(), 2);
        // Outdated (no line) sorts first; its deleted author reads as ghost.
        assert_eq!((threads[0].line, threads[0].side), (None, ReviewSide::Left));
        assert_eq!(threads[0].comments[0].author, "ghost");
        assert_eq!(threads[1].root_id, 1);
        assert_eq!(threads[1].line, Some(4));
        let said: Vec<_> = threads[1]
            .comments
            .iter()
            .map(|comment| (comment.author.as_str(), comment.body.as_str()))
            .collect();
        assert_eq!(said, [("octo", "Wrap here?"), ("me", "Agreed.")]);
    }

    #[test]
    fn replies_stay_out_of_the_review_payload() {
        let anchor = ReviewAnchor {
            path: "src/a.rs".into(),
            side: ReviewSide::Right,
            line: 4,
            start: None,
        };
        let comments = [
            ReviewComment {
                anchor: anchor.clone(),
                body: "line comment".into(),
                reply_to: None,
            },
            ReviewComment {
                anchor,
                body: "a reply".into(),
                reply_to: Some(ReplyTarget {
                    id: 1,
                    author: "octo".into(),
                }),
            },
        ];
        let payload = review_payload(&"a".repeat(40), ReviewKind::Comment, "", &comments);
        assert_eq!(payload["comments"].as_array().map(Vec::len), Some(1));
        assert_eq!(payload["comments"][0]["body"], "line comment");
    }

    #[test]
    fn only_plain_owner_and_name_reach_the_api_path() {
        assert!(is_repo_slug("gabins123/gGit"));
        assert!(is_repo_slug("o/r.js"));
        assert!(!is_repo_slug("o/r/../../user"));
        assert!(!is_repo_slug("o/--method=DELETE"));
        assert!(!is_repo_slug("../r"));
        assert!(is_repo_slug("org/.github"));
        assert!(!is_repo_slug("o"));
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

    fn stack_pr(number: u64, head: &str, base: &str, owner: &str) -> super::PullRequestSummary {
        super::PullRequestSummary {
            number,
            title: format!("pr {number}"),
            author: "someone".to_string(),
            head: head.to_string(),
            head_owner: owner.to_string(),
            base: base.to_string(),
            is_draft: false,
            is_cross_repository: false,
            review: None,
            checks: super::ChecksSummary::default(),
            review_requested: false,
            is_mine: false,
        }
    }

    #[test]
    fn a_linear_chain_becomes_one_stack_bottom_first() {
        let prs = vec![
            stack_pr(1, "feat-a", "dev", "me"),
            stack_pr(2, "feat-b", "feat-a", "me"),
            stack_pr(3, "feat-c", "feat-b", "me"),
        ];
        let stacks = super::compute_pull_request_stacks(&prs, "me", &[]);
        assert_eq!(stacks.len(), 1);
        assert_eq!(stacks[0].members, vec![1, 2, 3]);
        assert!(!stacks[0].native);
    }

    #[test]
    fn a_pr_with_two_children_is_a_tree_shown_as_one_stack() {
        let prs = vec![
            stack_pr(1, "feat-a", "dev", "me"),
            stack_pr(2, "feat-b", "feat-a", "me"),
            stack_pr(3, "feat-c", "feat-a", "me"),
        ];
        let stacks = super::compute_pull_request_stacks(&prs, "me", &[]);
        assert_eq!(stacks.len(), 1);
        assert_eq!(stacks[0].members.len(), 3);
        assert_eq!(stacks[0].members[0], 1);
        assert_eq!(
            stacks[0].members[1..].iter().collect::<std::collections::HashSet<_>>(),
            [&2, &3].into_iter().collect()
        );
    }

    #[test]
    fn a_cross_repository_pull_request_never_stacks() {
        let mut fork = stack_pr(2, "feat-a", "feat-a", "me");
        fork.is_cross_repository = true;
        let prs = vec![stack_pr(1, "feat-a", "dev", "me"), fork];
        let stacks = super::compute_pull_request_stacks(&prs, "me", &[]);
        assert!(stacks.is_empty());
    }

    #[test]
    fn a_differently_owned_head_never_stacks_even_when_not_flagged_cross_repository() {
        // The case-insensitive owner compare is a fallback for when
        // `isCrossRepository` itself can't be trusted.
        let prs = vec![
            stack_pr(1, "feat-a", "dev", "me"),
            stack_pr(2, "feat-b", "feat-a", "someone-else"),
        ];
        let stacks = super::compute_pull_request_stacks(&prs, "me", &[]);
        assert!(stacks.is_empty());
    }

    #[test]
    fn the_owner_compare_is_case_insensitive() {
        let prs = vec![
            stack_pr(1, "feat-a", "dev", "Me"),
            stack_pr(2, "feat-b", "feat-a", "me"),
        ];
        let stacks = super::compute_pull_request_stacks(&prs, "ME", &[]);
        assert_eq!(stacks.len(), 1);
    }

    #[test]
    fn a_base_branch_cycle_does_not_grow_the_stack_forever() {
        // Two pull requests whose base branches point at each other: not a
        // real GitHub state, but detection must terminate and skip it.
        let prs = vec![
            stack_pr(1, "feat-a", "feat-b", "me"),
            stack_pr(2, "feat-b", "feat-a", "me"),
        ];
        let stacks = super::compute_pull_request_stacks(&prs, "me", &[]);
        assert!(stacks.is_empty());
    }

    #[test]
    fn a_pr_whose_base_branch_has_no_pull_request_is_not_a_stack() {
        let prs = vec![stack_pr(1, "feat-a", "dev", "me")];
        let stacks = super::compute_pull_request_stacks(&prs, "me", &[]);
        assert!(stacks.is_empty());
    }

    #[test]
    fn a_chain_matching_a_native_stacks_members_is_native() {
        let prs = vec![
            stack_pr(1, "feat-a", "dev", "me"),
            stack_pr(2, "feat-b", "feat-a", "me"),
        ];
        let native = [super::PullRequestStack {
            members: vec![1, 2],
            native: true,
        }];
        let stacks = super::compute_pull_request_stacks(&prs, "me", &native);
        assert_eq!(stacks.len(), 1);
        assert!(stacks[0].native);
    }

    #[test]
    fn a_chain_with_no_matching_native_stack_stays_a_plain_base_branch_chain() {
        let prs = vec![
            stack_pr(1, "feat-a", "dev", "me"),
            stack_pr(2, "feat-b", "feat-a", "me"),
        ];
        let stacks = super::compute_pull_request_stacks(&prs, "me", &[]);
        assert_eq!(stacks.len(), 1);
        assert!(!stacks[0].native);
    }

    #[test]
    fn stack_parent_and_child_follow_real_base_and_head_not_flattened_order() {
        // 1 has two children, 2 and 3: a tree, flattened as [1, 2, 3] or
        // [1, 3, 2] depending on traversal, but 2 and 3 are siblings, not
        // parent and child.
        let prs = vec![
            stack_pr(1, "feat-a", "dev", "me"),
            stack_pr(2, "feat-b", "feat-a", "me"),
            stack_pr(3, "feat-c", "feat-a", "me"),
        ];
        let members = [1, 2, 3];
        assert_eq!(super::stack_parent(&members, 2, &prs), Some(1));
        assert_eq!(super::stack_parent(&members, 3, &prs), Some(1));
        assert_eq!(super::stack_parent(&members, 1, &prs), None);
        // Two children: the lowest number wins, deterministically.
        assert_eq!(super::stack_child(&members, 1, &prs), Some(2));
        assert_eq!(super::stack_depth(&members, 1, &prs), Some(0));
        assert_eq!(super::stack_depth(&members, 2, &prs), Some(1));
        assert_eq!(super::stack_depth(&members, 3, &prs), Some(1));
        assert_eq!(super::stack_depth(&members, 99, &prs), None);
    }

    fn approved_stack_pr(number: u64, head: &str, base: &str) -> super::PullRequestSummary {
        let mut pr = stack_pr(number, head, base, "me");
        pr.review = Some(super::ReviewDecision::Approved);
        pr
    }

    #[test]
    fn merging_the_top_of_a_clean_stack_takes_everything_below_it() {
        let prs = vec![
            approved_stack_pr(1, "feat-a", "dev"),
            approved_stack_pr(2, "feat-b", "feat-a"),
            approved_stack_pr(3, "feat-c", "feat-b"),
        ];
        let stack = super::PullRequestStack {
            members: vec![1, 2, 3],
            native: true,
        };
        let plan = super::plan_stack_merge(&stack, 2, &prs).expect("in the stack");
        assert_eq!(plan.merges, vec![1, 2]);
        assert_eq!(plan.stays_open, vec![3]);
        assert_eq!(plan.refusal, None);
    }

    #[test]
    fn the_selected_pull_request_itself_needs_no_review_or_checks() {
        // Only #1 (below #2) is checked; #2 itself has no review and no
        // checks recorded, and that's fine: branch protection is GitHub's
        // job at merge time.
        let prs = vec![approved_stack_pr(1, "feat-a", "dev"), stack_pr(2, "feat-b", "feat-a", "me")];
        let stack = super::PullRequestStack {
            members: vec![1, 2],
            native: true,
        };
        let plan = super::plan_stack_merge(&stack, 2, &prs).expect("in the stack");
        assert_eq!(plan.refusal, None);
    }

    #[test]
    fn no_review_decision_below_is_not_a_refusal() {
        // A repository with no required reviews reports no decision at all;
        // that must not be confused with an explicit non-approval.
        let prs = vec![
            stack_pr(1, "feat-a", "dev", "me"),
            approved_stack_pr(2, "feat-b", "feat-a"),
        ];
        let stack = super::PullRequestStack {
            members: vec![1, 2],
            native: true,
        };
        let plan = super::plan_stack_merge(&stack, 2, &prs).expect("in the stack");
        assert_eq!(plan.refusal, None);
    }

    #[test]
    fn an_unapproved_pull_request_below_refuses_the_merge_by_name() {
        let mut prs = vec![
            approved_stack_pr(1, "feat-a", "dev"),
            approved_stack_pr(2, "feat-b", "feat-a"),
        ];
        prs[0].review = Some(super::ReviewDecision::ChangesRequested);
        let stack = super::PullRequestStack {
            members: vec![1, 2],
            native: true,
        };
        let plan = super::plan_stack_merge(&stack, 2, &prs).expect("in the stack");
        assert!(plan.merges.is_empty());
        assert_eq!(plan.refusal, Some("#1 isn't approved".to_string()));
    }

    #[test]
    fn failing_checks_below_refuse_the_merge_by_name() {
        let mut prs = vec![
            approved_stack_pr(1, "feat-a", "dev"),
            approved_stack_pr(2, "feat-b", "feat-a"),
        ];
        prs[0].checks.failing = 1;
        let stack = super::PullRequestStack {
            members: vec![1, 2],
            native: true,
        };
        let plan = super::plan_stack_merge(&stack, 2, &prs).expect("in the stack");
        assert_eq!(plan.refusal, Some("#1 has failing checks".to_string()));
    }

    #[test]
    fn pending_checks_below_refuse_the_merge_by_name() {
        let mut prs = vec![
            approved_stack_pr(1, "feat-a", "dev"),
            approved_stack_pr(2, "feat-b", "feat-a"),
        ];
        prs[0].checks.pending = 1;
        let stack = super::PullRequestStack {
            members: vec![1, 2],
            native: true,
        };
        let plan = super::plan_stack_merge(&stack, 2, &prs).expect("in the stack");
        assert_eq!(plan.refusal, Some("#1 has checks still running".to_string()));
    }

    #[test]
    fn a_draft_below_refuses_the_merge_by_name() {
        let mut prs = vec![
            approved_stack_pr(1, "feat-a", "dev"),
            approved_stack_pr(2, "feat-b", "feat-a"),
        ];
        prs[0].is_draft = true;
        let stack = super::PullRequestStack {
            members: vec![1, 2],
            native: true,
        };
        let plan = super::plan_stack_merge(&stack, 2, &prs).expect("in the stack");
        assert_eq!(plan.refusal, Some("#1 is a draft".to_string()));
    }

    #[test]
    fn a_below_pull_request_missing_from_the_loaded_list_refuses_by_name() {
        // #1 exists in the real stack (GitHub said so) but never loaded
        // locally: LIST_LIMIT, a filter, or a merged pull request gh's list
        // left out.
        let prs = vec![approved_stack_pr(2, "feat-b", "feat-a")];
        let stack = super::PullRequestStack {
            members: vec![1, 2],
            native: true,
        };
        let plan = super::plan_stack_merge(&stack, 2, &prs).expect("in the stack");
        assert!(plan.merges.is_empty());
        assert_eq!(
            plan.refusal,
            Some("#1 below isn't loaded — refresh or open on GitHub".to_string())
        );
    }

    #[test]
    fn merge_async_payload_pins_the_head_and_names_the_method() {
        let payload = super::merge_async_payload(super::MergeMethod::Squash, "deadbeef");
        let value: serde_json::Value = serde_json::from_str(&payload).expect("json");
        assert_eq!(value["merge_method"], "squash");
        assert_eq!(value["sha"], "deadbeef");
    }

    #[test]
    fn merge_async_job_reads_a_pending_result() {
        let job = super::parse_merge_async_job(
            br#"{"status": "pending", "details": {"message": "Merge in progress", "uuid": "11111111-1111-1111-1111-111111111111", "merge_method": "merge", "merge_action": "merge", "expected_head_sha": "deadbeef"}}"#,
        )
        .expect("parses");
        assert_eq!(job.status, super::MergeAsyncStatus::Pending);
        assert_eq!(
            job.uuid.as_deref(),
            Some("11111111-1111-1111-1111-111111111111")
        );
    }

    #[test]
    fn merge_async_job_reads_a_merged_result() {
        let job = super::parse_merge_async_job(br#"{"status": "merged"}"#).expect("parses");
        assert_eq!(job.status, super::MergeAsyncStatus::Merged);
    }

    #[test]
    fn merge_async_job_reads_an_enqueued_result() {
        let job = super::parse_merge_async_job(
            br#"{"status": "enqueued", "details": {"message": "Added to the merge queue"}}"#,
        )
        .expect("parses");
        assert_eq!(job.status, super::MergeAsyncStatus::Enqueued);
    }

    #[test]
    fn merge_async_job_reads_a_failed_result_with_its_message() {
        let job = super::parse_merge_async_job(
            br#"{"status": "failed", "details": {"message": "Required status check has not succeeded"}}"#,
        )
        .expect("parses");
        assert_eq!(job.status, super::MergeAsyncStatus::Failed);
        assert_eq!(
            job.message.as_deref(),
            Some("Required status check has not succeeded")
        );
    }

    #[test]
    fn a_409_failed_body_on_stdout_gives_githubs_own_reason() {
        // A 400 or 409 from `merge-async` still returns the
        // `pull-request-merge-async-result` body; gh's own exit is non-zero,
        // but the reason is on stdout, not stderr.
        let stdout = br#"{"status": "failed", "details": {"message": "The stack needs to be rebased before it can be merged"}}"#;
        assert_eq!(
            super::merge_async_failure_message(stdout),
            Some("The stack needs to be rebased before it can be merged".to_string())
        );
    }

    #[test]
    fn stdout_that_isnt_a_merge_async_body_has_no_message() {
        assert_eq!(super::merge_async_failure_message(b"not json"), None);
        assert_eq!(
            super::merge_async_failure_message(br#"{"message": "some other API error"}"#),
            None
        );
    }

    #[test]
    fn native_stacks_parse_dynamic_graphql_aliases_ordered_by_position() {
        let body = br#"{"data":{"repository":{
            "pr1": {"stack": {"number": 7, "entries": {"nodes": [
                {"position": 1, "pullRequest": {"number": 2}},
                {"position": 0, "pullRequest": {"number": 1}}
            ]}}},
            "pr2": {"stack": {"number": 7, "entries": {"nodes": [
                {"position": 1, "pullRequest": {"number": 2}},
                {"position": 0, "pullRequest": {"number": 1}}
            ]}}},
            "pr3": {"stack": null},
            "pr4": null
        }}}"#;
        let stacks = super::parse_native_stacks(body).expect("parses");
        assert_eq!(stacks.len(), 1);
        assert_eq!(stacks[0].members, vec![1, 2]);
        assert!(stacks[0].native);
    }

    #[test]
    fn a_native_stack_with_only_one_entry_is_dropped() {
        let body = br#"{"data":{"repository":{
            "pr1": {"stack": {"number": 7, "entries": {"nodes": [
                {"position": 0, "pullRequest": {"number": 1}}
            ]}}}
        }}}"#;
        let stacks = super::parse_native_stacks(body).expect("parses");
        assert!(stacks.is_empty());
    }
}
