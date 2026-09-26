//! The Changes list: every changed file once, staged or not, the way lazygit
//! lists them. Two status letters say what is staged (left) and what is not
//! (right), and `space` flips a file in place instead of moving it to another
//! section.

use super::*;
use crate::kit::click::PointerClickExt as _;
use crate::view::components::{InteractionState, InteractionStyle};
use crate::view::rows::{FileListId, FileListRow, FileOrdinal, RowIx};
use gitcomet_core::domain::LineStats;
use std::path::{Path, PathBuf};

/// What is staged and what is not, for one path.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(in crate::view) struct ChangeLanes {
    pub(in crate::view) staged: Option<FileStatusKind>,
    pub(in crate::view) unstaged: Option<FileStatusKind>,
}

impl ChangeLanes {
    pub(in crate::view) fn of(repo: &RepoState, path: &Path) -> Self {
        Self {
            staged: repo
                .status_entry_for_path(DiffArea::Staged, path)
                .map(|entry| entry.kind),
            unstaged: repo
                .status_entry_for_path(DiffArea::Unstaged, path)
                .map(|entry| entry.kind),
        }
    }

    /// Where the file's diff opens: what is still unstaged comes first.
    pub(in crate::view) fn area(self) -> DiffArea {
        if self.unstaged.is_some() {
            DiffArea::Unstaged
        } else {
            DiffArea::Staged
        }
    }

    pub(in crate::view) fn conflicted(self) -> bool {
        self.unstaged == Some(FileStatusKind::Conflicted)
            || self.staged == Some(FileStatusKind::Conflicted)
    }

    /// What `space` does to it: stage while anything is unstaged, else unstage.
    pub(in crate::view) fn stages(self) -> bool {
        self.unstaged.is_some()
    }

    /// lazygit's two status letters: the staged side, then the unstaged one.
    pub(in crate::view) fn letters(self) -> [char; 2] {
        fn letter(kind: FileStatusKind) -> char {
            match kind {
                FileStatusKind::Modified => 'M',
                FileStatusKind::Added => 'A',
                FileStatusKind::Deleted => 'D',
                FileStatusKind::Renamed => 'R',
                FileStatusKind::Untracked => '?',
                FileStatusKind::Conflicted => 'U',
            }
        }
        // `git rm --cached` leaves a staged deletion beside the untracked
        // file: `D?`, not a bare `??` that hides what `space` would undo.
        if self.unstaged == Some(FileStatusKind::Untracked) && self.staged.is_none() {
            return ['?', '?'];
        }
        if self.conflicted() {
            return ['U', 'U'];
        }
        [
            self.staged.map_or(' ', letter),
            self.unstaged.map_or(' ', letter),
        ]
    }

    fn shown_by(self, filter: ChangesFilter) -> bool {
        match filter {
            ChangesFilter::All => true,
            ChangesFilter::Unstaged => self
                .unstaged
                .is_some_and(|kind| kind != FileStatusKind::Untracked),
            ChangesFilter::Staged => self.staged.is_some(),
            ChangesFilter::Untracked => self.unstaged == Some(FileStatusKind::Untracked),
        }
    }
}

/// lazygit's file filter: everything, or one kind of change.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(in crate::view) enum ChangesFilter {
    #[default]
    All,
    Unstaged,
    Staged,
    Untracked,
}

impl ChangesFilter {
    pub(in crate::view) const ALL: [Self; 4] =
        [Self::All, Self::Unstaged, Self::Staged, Self::Untracked];

    pub(in crate::view) const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Unstaged => "Unstaged",
            Self::Staged => "Staged",
            Self::Untracked => "Untracked",
        }
    }

    pub(in crate::view) fn next(self) -> Self {
        let ix = Self::ALL
            .iter()
            .position(|filter| *filter == self)
            .unwrap_or(0);
        Self::ALL[(ix + 1) % Self::ALL.len()]
    }
}

/// The `/` filter. Space-separated words must each fuzzy-match the path: its
/// letters in order, anything in between, the way fzf and lazygit match.
/// `.rs` or `*.rs` words name the file types to keep instead, any of them.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub(in crate::view) struct ChangesQuery {
    terms: Vec<Box<str>>,
    types: Vec<Box<str>>,
}

impl ChangesQuery {
    pub(in crate::view) fn parse(raw: &str) -> Self {
        let mut query = Self::default();
        for word in raw.split_whitespace() {
            let word = word.to_lowercase();
            match word.strip_prefix("*.").or_else(|| word.strip_prefix('.')) {
                // A bare `.` or `*.` is a type still being typed.
                Some("") => {}
                Some(file_type) => query.types.push(file_type.into()),
                None => query.terms.push(word.into()),
            }
        }
        query
    }

    pub(in crate::view) fn is_empty(&self) -> bool {
        self.terms.is_empty() && self.types.is_empty()
    }

    fn matches(&self, haystack: &str, file_type: &str) -> bool {
        (self.types.is_empty() || self.types.iter().any(|kept| **kept == *file_type))
            && self.terms.iter().all(|term| fuzzy_contains(haystack, term))
    }
}

/// Whether `needle`'s characters appear in `haystack` in order. Both come
/// lowercased; ASCII, which nearly every path is, compares bytes and
/// allocates nothing, so a keystroke over thousands of files stays instant.
fn fuzzy_contains(haystack: &str, needle: &str) -> bool {
    if haystack.is_ascii() && needle.is_ascii() {
        let mut rest = haystack.as_bytes();
        return needle
            .bytes()
            .all(|byte| match rest.iter().position(|b| *b == byte) {
                Some(ix) => {
                    rest = &rest[ix + 1..];
                    true
                }
                None => false,
            });
    }
    let mut rest = haystack.chars();
    needle
        .chars()
        .all(|ch| rest.any(|candidate| candidate == ch))
}

/// A file's type as the filter spells it: its extension, or a dotfile's own
/// name (`.gitignore` is type `gitignore`). Lowercase; empty when it has none.
fn file_type(path: &Path) -> Box<str> {
    match path.extension() {
        Some(ext) => ext.to_string_lossy().to_lowercase().into(),
        None => path
            .file_name()
            .map(|name| name.to_string_lossy())
            .and_then(|name| name.strip_prefix('.').map(str::to_lowercase))
            .unwrap_or_default()
            .into(),
    }
}

/// Every changed path once, in path order.
pub(in crate::view) struct ChangesList {
    /// The worktree entry where there is one, else the staged one: its kind is
    /// what the row's icon shows.
    pub(in crate::view) entries: Vec<FileStatus>,
    pub(in crate::view) lanes: Vec<ChangeLanes>,
    /// Both lanes' `+/-` added up, for the rows and the edit-size sorts.
    stats: FxHashMap<PathBuf, LineStats>,
    /// Per entry: the path lowercased with `/` separators, and its file type.
    /// Worked out once per status so the `/` filter only compares.
    haystacks: Vec<Box<str>>,
    types: Vec<Box<str>>,
    /// The file types present, most files first, for the filter's type chips.
    type_counts: Vec<(Box<str>, usize)>,
}

impl ChangesList {
    pub(in crate::view) fn build(
        worktree: &[FileStatus],
        staged: &[FileStatus],
        worktree_stats: Option<&FxHashMap<PathBuf, LineStats>>,
        staged_stats: Option<&FxHashMap<PathBuf, LineStats>>,
    ) -> Self {
        let mut by_path: std::collections::BTreeMap<
            &Path,
            (Option<&FileStatus>, Option<&FileStatus>),
        > = std::collections::BTreeMap::new();
        for entry in staged {
            by_path.entry(entry.path.as_path()).or_default().0 = Some(entry);
        }
        for entry in worktree {
            by_path.entry(entry.path.as_path()).or_default().1 = Some(entry);
        }
        let mut entries = Vec::with_capacity(by_path.len());
        let mut lanes = Vec::with_capacity(by_path.len());
        let mut stats = FxHashMap::default();
        let mut haystacks = Vec::with_capacity(by_path.len());
        let mut types = Vec::with_capacity(by_path.len());
        let mut type_counts: FxHashMap<Box<str>, usize> = FxHashMap::default();
        for (path, (staged, unstaged)) in by_path {
            let Some(entry) = unstaged.or(staged) else {
                continue;
            };
            entries.push(entry.clone());
            haystacks.push(
                path.to_string_lossy()
                    .replace('\\', "/")
                    .to_lowercase()
                    .into_boxed_str(),
            );
            let kind = file_type(path);
            if !kind.is_empty() {
                *type_counts.entry(kind.clone()).or_default() += 1;
            }
            types.push(kind);
            lanes.push(ChangeLanes {
                staged: staged.map(|entry| entry.kind),
                unstaged: unstaged.map(|entry| entry.kind),
            });
            let lane_stats = |map: Option<&FxHashMap<PathBuf, LineStats>>| {
                map.and_then(|map| map.get(path)).copied()
            };
            let sum = |a: Option<u32>, b: Option<u32>| match (a, b) {
                (Some(a), Some(b)) => Some(a.saturating_add(b)),
                (a, b) => a.or(b),
            };
            let combined = match (lane_stats(staged_stats), lane_stats(worktree_stats)) {
                (Some(a), Some(b)) => Some(LineStats {
                    additions: sum(a.additions, b.additions),
                    deletions: sum(a.deletions, b.deletions),
                }),
                (a, b) => a.or(b),
            };
            if let Some(combined) = combined {
                stats.insert(path.to_path_buf(), combined);
            }
        }
        let mut type_counts: Vec<(Box<str>, usize)> = type_counts.into_iter().collect();
        type_counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Self {
            entries,
            lanes,
            stats,
            haystacks,
            types,
            type_counts,
        }
    }

    /// Which entries `query` keeps, as a mask over the list; `None` keeps all.
    fn matching(&self, query: &ChangesQuery) -> Option<Vec<bool>> {
        (!query.is_empty()).then(|| {
            (0..self.entries.len())
                .map(|ix| query.matches(&self.haystacks[ix], &self.types[ix]))
                .collect()
        })
    }

    pub(in crate::view) fn position(&self, path: &Path) -> Option<usize> {
        self.entries
            .binary_search_by(|entry| entry.path.as_path().cmp(path))
            .ok()
    }

    /// Per kind filter, the files it shows among `matched` (all when `None`).
    fn counts(&self, matched: Option<&[bool]>) -> [usize; 4] {
        ChangesFilter::ALL.map(|filter| {
            (0..self.lanes.len())
                .filter(|ix| {
                    matched.is_none_or(|matched| matched[*ix]) && self.lanes[*ix].shown_by(filter)
                })
                .count()
        })
    }
}

/// What `a` or a folder toggle does to `shown`: stage what's unstaged, or,
/// when none of it is, unstage it all. `all` is the unfiltered list acting on
/// everything, which the backend spells as no paths at all. `None` when there
/// is nothing to do.
pub(in crate::view) fn toggle_plan<'a>(
    shown: impl Iterator<Item = (&'a Path, ChangeLanes)> + Clone,
    stage: Option<bool>,
    all: bool,
) -> Option<(bool, Vec<PathBuf>)> {
    let stage = stage.unwrap_or_else(|| shown.clone().any(|(_, lanes)| lanes.stages()));
    if all {
        return Some((stage, Vec::new()));
    }
    let paths: Vec<PathBuf> = shown
        .filter(|(_, lanes)| {
            if stage {
                lanes.stages()
            } else {
                lanes.staged.is_some()
            }
        })
        .map(|(path, _)| path.to_path_buf())
        .collect();
    (!paths.is_empty()).then_some((stage, paths))
}

/// The drawn file `direction` steps to from `path`. A file the list no longer
/// shows (filtered out once staged, or committed) steps from where it would
/// sit in path order, so `j` keeps going the way lazygit's cursor does.
fn changes_step(drawn: &[&Path], path: &Path, direction: i8) -> Option<usize> {
    match drawn.iter().position(|candidate| *candidate == path) {
        Some(ix) if direction < 0 => ix.checked_sub(1),
        Some(ix) => (ix + 1 < drawn.len()).then_some(ix + 1),
        None if direction < 0 => drawn.iter().rposition(|candidate| *candidate < path),
        None => drawn.iter().position(|candidate| *candidate > path),
    }
}

/// A range selected in the Changes list: from `anchor` to `head`, the end
/// Shift+J/K and shift-click last moved. It holds only while `head` is the
/// open file in the same repo, so anything else that opens a file (another
/// repo, a closed diff, `j`) leaves it lapsed rather than reviving it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::view) struct ChangesRange {
    repo_id: RepoId,
    anchor: PathBuf,
    head: PathBuf,
}

/// A `/` filter's mask over the list (`None` keeps everything), keyed by
/// what it was worked out from.
type MatchedSlot = Option<(u64, Option<Arc<[bool]>>)>;

/// The list, its filtered and sorted order, and its rows, each built once per
/// change to what feeds it.
#[derive(Default)]
pub(super) struct ChangesCache {
    list: std::cell::RefCell<Option<(u64, Arc<ChangesList>)>>,
    /// What the `/` filter keeps, keyed by the list and the query.
    matched: std::cell::RefCell<MatchedSlot>,
    /// The kind-filtered list sorted, without the `/` filter: a keystroke
    /// only masks this, it never sorts again.
    sorted: std::cell::RefCell<Option<(u64, Arc<[usize]>)>>,
    order: std::cell::RefCell<Option<(u64, Arc<[usize]>)>>,
    plan: std::cell::RefCell<crate::view::rows::FileListPlanCache>,
}

fn changes_list_key(repo: &RepoState) -> u64 {
    let mut hasher = FxHasher::default();
    (
        repo.id,
        repo.worktree_status_cache_rev(),
        repo.staged_status_cache_rev(),
        repo.line_stats_rev(DiffArea::Unstaged),
        repo.line_stats_rev(DiffArea::Staged),
    )
        .hash(&mut hasher);
    hasher.finish()
}

/// Which of the list's entries a tree row shows, or a folder's files.
struct ChangesRows {
    list: Arc<ChangesList>,
    /// What the `/` filter keeps, before the kind filter; `None` is all.
    matched: Option<Arc<[bool]>>,
    /// Projection order: filtered and sorted indexes into `list`.
    order: Arc<[usize]>,
    plan: Arc<crate::view::rows::FileListPlan>,
}

impl ChangesRows {
    fn entry(&self, ordinal: FileOrdinal) -> Option<usize> {
        self.order.get(ordinal.0).copied()
    }

    /// Shown files in the order they're drawn: a tree hoists folders above
    /// files, so its rows aren't the projection's order.
    fn drawn(&self) -> Vec<usize> {
        if self.plan.is_tree() {
            self.plan
                .ordered()
                .iter()
                .filter_map(|ordinal| self.order.get(ordinal).copied())
                .collect()
        } else {
            self.order.to_vec()
        }
    }
}

impl DetailsPaneView {
    pub(in crate::view) fn changes_list(&self, repo: &RepoState) -> Option<Arc<ChangesList>> {
        let worktree = repo.worktree_status_entries()?;
        let staged = repo.staged_status_entries()?;
        let key = changes_list_key(repo);
        let mut cache = self.changes.list.borrow_mut();
        if let Some((cached, list)) = cache.as_ref()
            && *cached == key
        {
            return Some(Arc::clone(list));
        }
        let list = Arc::new(ChangesList::build(
            worktree,
            staged,
            repo.line_stats_for_area(DiffArea::Unstaged),
            repo.line_stats_for_area(DiffArea::Staged),
        ));
        *cache = Some((key, Arc::clone(&list)));
        Some(list)
    }

    fn changes_projection_key(&self, repo: &RepoState) -> u64 {
        let mut hasher = FxHasher::default();
        (
            changes_list_key(repo),
            self.file_list_sort_for(FileListId::Changes),
            self.changes_filter,
            &self.changes_query,
        )
            .hash(&mut hasher);
        hasher.finish()
    }

    /// The kind-filtered list in sort order, before the `/` filter.
    fn changes_sorted(
        &self,
        repo: &RepoState,
        list: &ChangesList,
        sort: crate::view::rows::CommitFileSort,
    ) -> Arc<[usize]> {
        let mut hasher = FxHasher::default();
        (changes_list_key(repo), sort, self.changes_filter).hash(&mut hasher);
        let key = hasher.finish();
        let mut cache = self.changes.sorted.borrow_mut();
        if let Some((cached, sorted)) = cache.as_ref()
            && *cached == key
        {
            return Arc::clone(sorted);
        }
        let shown: Vec<usize> = (0..list.entries.len())
            .filter(|ix| list.lanes[*ix].shown_by(self.changes_filter))
            .collect();
        let sorted = crate::view::rows::status_section_sorted_indexes(
            &list.entries,
            &shown,
            sort,
            Some(&list.stats),
        );
        *cache = Some((key, Arc::clone(&sorted)));
        sorted
    }

    /// The `/` filter's matches, recomputed only when the list or the query
    /// changes, so scrolling and repaints never re-run it.
    fn changes_matched(&self, repo: &RepoState, list: &ChangesList) -> Option<Arc<[bool]>> {
        let mut hasher = FxHasher::default();
        (changes_list_key(repo), &self.changes_query).hash(&mut hasher);
        let key = hasher.finish();
        let mut cache = self.changes.matched.borrow_mut();
        if let Some((cached, matched)) = cache.as_ref()
            && *cached == key
        {
            return matched.clone();
        }
        let matched: Option<Arc<[bool]>> = list.matching(&self.changes_query).map(Into::into);
        *cache = Some((key, matched.clone()));
        matched
    }

    fn changes_rows(&self, repo: &RepoState) -> Option<ChangesRows> {
        let list = self.changes_list(repo)?;
        let matched = self.changes_matched(repo, &list);
        let sort = self.file_list_sort_for(FileListId::Changes);
        let key = self.changes_projection_key(repo);
        let order = {
            let mut cache = self.changes.order.borrow_mut();
            match cache.as_ref() {
                Some((cached, order)) if *cached == key => Arc::clone(order),
                _ => {
                    let sorted = self.changes_sorted(repo, &list, sort);
                    let order: Arc<[usize]> = match &matched {
                        Some(matched) => sorted.iter().copied().filter(|ix| matched[*ix]).collect(),
                        None => sorted,
                    };
                    *cache = Some((key, Arc::clone(&order)));
                    order
                }
            }
        };
        let layout = self.file_list_layout_for(repo.id, FileListId::Changes);
        let collapsed = self.file_list_collapsed_for(repo.id, FileListId::Changes);
        let plan =
            self.changes
                .plan
                .borrow_mut()
                .plan_for(key, layout, &collapsed, order.len(), || {
                    crate::view::rows::FileTree::build(
                        order.iter().filter_map(|ix| {
                            list.entries.get(*ix).map(|entry| {
                                let stats =
                                    list.stats.get(&entry.path).copied().unwrap_or_default();
                                crate::view::rows::FileTreeItem {
                                    path: entry.path.as_path(),
                                    additions: stats.additions,
                                    deletions: stats.deletions,
                                }
                            })
                        }),
                        sort,
                    )
                });
        Some(ChangesRows {
            list,
            matched,
            order,
            plan,
        })
    }

    /// The shown files in drawn order, for `j`/`k` from Details and the diff.
    pub(in crate::view) fn changes_drawn(
        &self,
        repo_id: RepoId,
    ) -> Option<Vec<(PathBuf, ChangeLanes)>> {
        let repo = self.active_repo().filter(|repo| repo.id == repo_id)?;
        let rows = self.changes_rows(repo)?;
        Some(
            rows.drawn()
                .into_iter()
                .map(|ix| (rows.list.entries[ix].path.clone(), rows.list.lanes[ix]))
                .collect(),
        )
    }

    /// The file `direction` steps to from `path`, for `j`/`k`, F1/F4 and the
    /// diff toolbar's arrows alike.
    pub(in crate::view) fn changes_neighbor(
        &self,
        repo_id: RepoId,
        path: &Path,
        direction: i8,
    ) -> Option<(PathBuf, ChangeLanes)> {
        // By reference: the diff toolbar asks on every render.
        let repo = self.active_repo().filter(|repo| repo.id == repo_id)?;
        let rows = self.changes_rows(repo)?;
        let drawn = rows.drawn();
        let paths: Vec<&Path> = drawn
            .iter()
            .map(|ix| rows.list.entries[*ix].path.as_path())
            .collect();
        let ix = drawn[changes_step(&paths, path, direction)?];
        Some((rows.list.entries[ix].path.clone(), rows.list.lanes[ix]))
    }

    /// Scrolls `path`'s row into view, opening the folders that hide it.
    pub(in crate::view) fn reveal_changes_path(
        &mut self,
        path: &Path,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(repo) = self.active_repo() else {
            return;
        };
        let repo_id = repo.id;
        let Some(rows) = self.changes_rows(repo) else {
            return;
        };
        let Some(ix) = rows.list.position(path) else {
            return;
        };
        let Some(position) = rows.order.iter().position(|entry| *entry == ix) else {
            return;
        };
        let ordinal = FileOrdinal(position);
        let chains = rows.plan.reveal(ordinal);
        let plan = if chains.is_empty() {
            rows.plan
        } else {
            let collapsed = self
                .file_list_collapsed
                .entry((repo_id, FileListId::Changes))
                .or_default();
            for chain in chains {
                collapsed.expand(&chain);
            }
            cx.notify();
            match self.active_repo().and_then(|repo| self.changes_rows(repo)) {
                Some(rows) => rows.plan,
                None => return,
            }
        };
        if let Some(row) = plan.row_ix_for_ordinal(ordinal) {
            self.unstaged_scroll
                .scroll_to_item_strict(row.0, gpui::ScrollStrategy::Center);
        }
    }

    pub(in crate::view) fn cycle_changes_filter(&mut self, cx: &mut gpui::Context<Self>) {
        self.set_changes_filter(self.changes_filter.next(), cx);
    }

    fn set_changes_filter(&mut self, filter: ChangesFilter, cx: &mut gpui::Context<Self>) {
        if self.changes_filter != filter {
            self.changes_filter = filter;
            self.notify_commit_file_projection_dependents(cx);
            cx.notify();
        }
    }

    /// Stages `paths` (everything when empty), confirming first if any of them
    /// still has conflict markers.
    fn stage_changes(
        &mut self,
        repo_id: RepoId,
        paths: Vec<PathBuf>,
        anchor: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if let Some(confirm) = crate::view::conflict_markers::stage_confirm_popover(
            &self.state,
            repo_id,
            paths.clone(),
            false,
        ) {
            self.open_popover_at(confirm, anchor, window, cx);
        } else {
            self.store.dispatch(Msg::StagePaths {
                repo_id,
                paths: paths.clone().into(),
            });
            self.follow_open_change(repo_id, &paths, DiffArea::Staged);
        }
        cx.notify();
    }

    fn unstage_changes(
        &mut self,
        repo_id: RepoId,
        paths: Vec<PathBuf>,
        cx: &mut gpui::Context<Self>,
    ) {
        self.store.dispatch(Msg::UnstagePaths {
            repo_id,
            paths: paths.clone().into(),
        });
        self.follow_open_change(repo_id, &paths, DiffArea::Unstaged);
        cx.notify();
    }

    /// Keeps the open diff on its file as the file changes lanes: staging it
    /// moves the diff to what's staged, unstaging back.
    fn follow_open_change(&self, repo_id: RepoId, paths: &[PathBuf], area: DiffArea) {
        let Some(DiffTarget::WorkingTree { path, area: open }) = self
            .active_repo()
            .filter(|repo| repo.id == repo_id)
            .and_then(|repo| repo.diff_state.diff_target.as_ref())
        else {
            return;
        };
        if *open != area && (paths.is_empty() || paths.iter().any(|p| path.starts_with(p))) {
            self.store.dispatch(Msg::SelectDiff {
                repo_id,
                target: DiffTarget::WorkingTree {
                    path: path.clone(),
                    area,
                },
            });
        }
    }

    /// `a` and the header buttons: stage what the filter shows, or, when none
    /// of it is unstaged, unstage it all. An unfiltered list acts on
    /// everything, the way `git add -A` does.
    pub(in crate::view) fn toggle_all_changes(
        &mut self,
        stage: Option<bool>,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(repo) = self.active_repo() else {
            return;
        };
        let repo_id = repo.id;
        let Some(rows) = self.changes_rows(repo) else {
            return;
        };
        let shown = rows
            .order
            .iter()
            .map(|ix| (rows.list.entries[*ix].path.as_path(), rows.list.lanes[*ix]));
        // Filtered either way, it names the files: an empty list is all of them.
        let all = self.changes_filter == ChangesFilter::All && self.changes_query.is_empty();
        let Some((stage, paths)) = toggle_plan(shown, stage, all) else {
            return;
        };
        if stage {
            let anchor = crate::view::conflict_markers::centered_dialog_anchor(window);
            self.stage_changes(repo_id, paths, anchor, window, cx);
        } else {
            self.unstage_changes(repo_id, paths, cx);
        }
    }

    /// A folder's shown files, stage or unstage: stage while any is unstaged.
    fn toggle_changes_folder(
        &mut self,
        key: &Path,
        anchor: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(repo) = self.active_repo() else {
            return;
        };
        let repo_id = repo.id;
        let Some(rows) = self.changes_rows(repo) else {
            return;
        };
        let shown = rows
            .order
            .iter()
            .map(|ix| (rows.list.entries[*ix].path.as_path(), rows.list.lanes[*ix]))
            .filter(|(path, _)| path.starts_with(key));
        match toggle_plan(shown, None, false) {
            Some((true, paths)) => self.stage_changes(repo_id, paths, anchor, window, cx),
            Some((false, paths)) => self.unstage_changes(repo_id, paths, cx),
            None => {}
        }
    }

    /// Moves the range's moving end from `open` to `to`. A range still held
    /// at `open` keeps its anchor; otherwise a new one starts at `open`.
    pub(in crate::view) fn extend_changes_range(
        &mut self,
        repo_id: RepoId,
        open: PathBuf,
        to: PathBuf,
        cx: &mut gpui::Context<Self>,
    ) {
        let anchor = match &self.changes_range {
            Some(range) if range.repo_id == repo_id && range.head == open => range.anchor.clone(),
            _ => open,
        };
        self.changes_range = Some(ChangesRange {
            repo_id,
            anchor,
            head: to,
        });
        cx.notify();
    }

    pub(in crate::view) fn clear_changes_range(&mut self, cx: &mut gpui::Context<Self>) {
        if self.changes_range.take().is_some() {
            cx.notify();
        }
    }

    /// The selected range's ends as drawn positions, low first: from the
    /// anchor to the open file. `None` without a range of two or more, or
    /// when either end is filtered out of sight.
    fn changes_span(&self, repo: &RepoState, rows: &ChangesRows) -> Option<(usize, usize)> {
        let range = self
            .changes_range
            .as_ref()
            .filter(|range| range.repo_id == repo.id)?;
        let DiffTarget::WorkingTree { path: open, .. } = repo.diff_state.diff_target.as_ref()?
        else {
            return None;
        };
        if *open != range.head {
            return None;
        }
        let anchor = range.anchor.as_path();
        let drawn = rows.drawn();
        let position = |path: &Path| {
            drawn
                .iter()
                .position(|ix| rows.list.entries[*ix].path == path)
        };
        let (from, to) = (position(anchor)?, position(open)?);
        (from != to).then(|| (from.min(to), from.max(to)))
    }

    /// The selected range's files in drawn order, for `space`, Ctrl+S/U and
    /// `d`; `None` without a range.
    pub(in crate::view) fn changes_selection(
        &self,
        repo_id: RepoId,
    ) -> Option<Vec<(PathBuf, ChangeLanes)>> {
        let repo = self.active_repo().filter(|repo| repo.id == repo_id)?;
        let rows = self.changes_rows(repo)?;
        let (from, to) = self.changes_span(repo, &rows)?;
        // A file inside a collapsed folder is between the ends but out of
        // sight: acting on it would be a surprise, so it isn't selected.
        let mut projection = vec![usize::MAX; rows.list.entries.len()];
        for (position, ix) in rows.order.iter().enumerate() {
            projection[*ix] = position;
        }
        Some(
            rows.drawn()[from..=to]
                .iter()
                .filter(|ix| {
                    rows.plan
                        .row_ix_for_ordinal(FileOrdinal(projection[**ix]))
                        .is_some()
                })
                .map(|ix| (rows.list.entries[*ix].path.clone(), rows.list.lanes[*ix]))
                .collect(),
        )
    }

    /// Whether the `/` filter narrows the list.
    pub(in crate::view) fn changes_query_active(&self) -> bool {
        !self.changes_query.is_empty()
    }

    /// `/`: shows the filter box and focuses it on the next frame, so the `/`
    /// that opened it isn't typed into it.
    pub(in crate::view) fn open_changes_query(&mut self, cx: &mut gpui::Context<Self>) {
        self.changes_query_open = true;
        self.changes_query_focus_pending = true;
        cx.notify();
    }

    /// Esc: drops the filter and hands the keyboard back to the list.
    pub(in crate::view) fn clear_changes_query(
        &mut self,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        self.changes_query_open = false;
        if !self.changes_query_input.read(cx).text().is_empty() {
            self.changes_query_input
                .update(cx, |input, cx| input.set_text("", cx));
        }
        if !self.changes_query.is_empty() {
            self.changes_query = ChangesQuery::default();
            self.notify_commit_file_projection_dependents(cx);
        }
        window.focus(&self.panel_focus_handle, cx);
        cx.notify();
    }

    /// Typing filters as it goes; Enter keeps the filter and goes back to the
    /// list, Esc drops it.
    pub(super) fn changes_query_input_changed(
        &mut self,
        input: Entity<components::TextInput>,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let (enter, escape) = input.update(cx, |input, _| {
            (input.take_enter_pressed(), input.take_escape_pressed())
        });
        if escape {
            self.clear_changes_query(window, cx);
            return;
        }
        let query = ChangesQuery::parse(input.read(cx).text());
        if query != self.changes_query {
            self.changes_query = query;
            self.notify_commit_file_projection_dependents(cx);
            cx.notify();
        }
        if enter {
            self.changes_query_open = false;
            window.focus(&self.panel_focus_handle, cx);
            cx.notify();
        }
    }

    /// A type chip: adds `.ext` to the filter, or takes it back out.
    fn toggle_changes_type(&mut self, file_type: &str, cx: &mut gpui::Context<Self>) {
        let text = self.changes_query_input.read(cx).text().to_string();
        let token = format!(".{file_type}");
        let starred = format!("*{token}");
        let mut words: Vec<&str> = text.split_whitespace().collect();
        let before = words.len();
        words.retain(|word| {
            !word.eq_ignore_ascii_case(&token) && !word.eq_ignore_ascii_case(&starred)
        });
        if words.len() == before {
            words.push(&token);
        }
        let next = words.join(" ");
        self.changes_query_input
            .update(cx, |input, cx| input.set_text(next, cx));
    }

    /// `Shift+Space`: the open file's folder, stage or unstage; the folder
    /// button's key in a tree, and a way to act on a directory in a flat list.
    pub(in crate::view) fn toggle_open_change_folder(
        &mut self,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(DiffTarget::WorkingTree { path, .. }) = self
            .active_repo()
            .and_then(|repo| repo.diff_state.diff_target.as_ref())
        else {
            return;
        };
        let Some(folder) = path.parent().map(Path::to_path_buf) else {
            return;
        };
        let anchor = crate::view::conflict_markers::centered_dialog_anchor(window);
        self.toggle_changes_folder(&folder, anchor, window, cx);
    }

    /// The Changes view: header, the one list, and the commit box.
    pub(in crate::view) fn changes_view(&mut self, cx: &mut gpui::Context<Self>) -> AnyElement {
        let theme = self.theme;
        let ui_scale = self.ui_scale();
        let Some(repo) = self.active_repo() else {
            return components::empty_state(theme, "Changes", "No repository selected.")
                .into_any_element();
        };
        let repo_id = repo.id;
        let loading = repo.worktree_status_is_loading() || repo.staged_status_is_loading();
        let status_error = [&repo.worktree_status, &repo.staged_status]
            .into_iter()
            .find_map(|lane| match lane {
                Loadable::Error(error) => Some(error.clone()),
                _ => None,
            })
            .or_else(|| match &repo.status {
                Loadable::Error(error) => Some(error.clone()),
                _ => None,
            });
        let busy = repo.local_actions_in_flight > 0;
        let rows = self.changes_rows(repo);
        let counts = rows
            .as_ref()
            .map_or([0; 4], |rows| rows.list.counts(rows.matched.as_deref()));
        let total = rows.as_ref().map_or(0, |rows| rows.list.entries.len());
        let querying = !self.changes_query.is_empty();
        let shown = rows.as_ref().map_or(0, |rows| rows.order.len());
        let row_count = rows.as_ref().map_or(0, |rows| rows.plan.row_len());
        let any_unstaged = rows
            .as_ref()
            .is_some_and(|rows| rows.order.iter().any(|ix| rows.list.lanes[*ix].stages()));
        let any_staged = rows.as_ref().is_some_and(|rows| {
            rows.order
                .iter()
                .any(|ix| rows.list.lanes[*ix].staged.is_some())
        });
        let filter = self.changes_filter;

        let menu_invoker: SharedString = "changes_view_menu".into();
        let menu_open = self.active_context_menu_invoker.as_ref() == Some(&menu_invoker);
        let icon_muted = with_alpha(
            theme.colors.accent.foreground,
            if theme.is_dark { 0.72 } else { 0.82 },
        );
        let title = div()
            .id("changes_view_title")
            .debug_selector(|| "changes_view_title".to_string())
            .flex()
            .items_center()
            .gap_1()
            .px_1()
            .rounded(px(theme.radii.row))
            .tab_index(0)
            .control_interaction(
                InteractionStyle::header(theme),
                InteractionState::default().open(menu_open),
            )
            .child(
                div()
                    .text_size(theme.ui_text(14.0))
                    .font_weight(FontWeight::BOLD)
                    .whitespace_nowrap()
                    .child(if querying {
                        format!("Changes · {} of {total}", counts[0])
                    } else {
                        format!("Changes · {}", counts[0])
                    }),
            )
            .child(svg_icon("icons/chevron_down.svg", icon_muted, px(12.0)))
            .on_activate(
                false,
                controls::ControlActivation::Action,
                cx.listener(move |this, e: &ClickEvent, window, cx| {
                    this.open_popover_at(
                        PopoverKind::ChangeTrackingSettings.invoked_by(menu_invoker.clone()),
                        e.position(),
                        window,
                        cx,
                    );
                    cx.notify();
                }),
            );

        let stage_all = components::Button::new("changes_stage_all", "Stage all")
            .style(components::ButtonStyle::Subtle)
            .disabled(!any_unstaged)
            .on_click(theme, cx, |this, _e, window, cx| {
                this.toggle_all_changes(Some(true), window, cx);
            })
            .debug_selector(|| "changes_stage_all".to_string())
            .gitcomet_tooltip(theme, "Stage what the list shows (a)".into());
        let unstage_all = components::Button::new("changes_unstage_all", "Unstage all")
            .style(components::ButtonStyle::Subtle)
            .disabled(!any_staged)
            .on_click(theme, cx, |this, _e, window, cx| {
                this.toggle_all_changes(Some(false), window, cx);
            })
            .debug_selector(|| "changes_unstage_all".to_string())
            .gitcomet_tooltip(theme, "Unstage what the list shows (a)".into());

        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .pt_2()
            .child(title)
            .child(div().flex_1())
            .when(busy, |header| {
                header.child(svg_spinner(
                    ("changes_busy", repo_id.0),
                    icon_muted,
                    px(14.0),
                ))
            })
            .child(self.file_list_controls(FileListId::Changes, repo_id, "changes", false, cx))
            .child(stage_all)
            .child(unstage_all);

        let mut chips = div()
            .id("changes_filter")
            .debug_selector(|| "changes_filter".to_string())
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .overflow_hidden();
        for (ix, option) in ChangesFilter::ALL.into_iter().enumerate() {
            let selected = option == filter;
            let count = counts[ix];
            chips = chips.child(
                div()
                    .id(("changes_filter_chip", ix))
                    .debug_selector(move || format!("changes_filter_chip_{ix}"))
                    .flex_none()
                    .px(ui_scale.px(8.0))
                    .h(components::control_height(ui_scale))
                    .flex()
                    .items_center()
                    .rounded(px(theme.radii.control))
                    .text_size(theme.ui_text(12.0))
                    .whitespace_nowrap()
                    .text_color(if selected {
                        theme.colors.interaction.selected_foreground
                    } else {
                        theme.colors.foreground.secondary
                    })
                    .tab_index(0)
                    .control_interaction(
                        InteractionStyle::new(theme).selection_outline(false),
                        InteractionState::default()
                            .selected(selected, theme.colors.interaction.selected_background),
                    )
                    .child(format!("{} {count}", option.label()))
                    .on_activate(
                        false,
                        controls::ControlActivation::Action,
                        cx.listener(move |this, e: &ClickEvent, _window, cx| {
                            if e.standard_click() {
                                this.set_changes_filter(option, cx);
                            }
                        }),
                    )
                    .gitcomet_tooltip(theme, "F cycles the filter".into()),
            );
        }

        let query_bar = (self.changes_query_open || querying).then(|| {
            let kept_types = &self.changes_query.types;
            let type_chips = rows
                .as_ref()
                .map(|rows| {
                    rows.list
                        .type_counts
                        .iter()
                        .take(8)
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
                .into_iter()
                .enumerate()
                .map(|(ix, (file_type, count))| {
                    let selected = kept_types.contains(&file_type);
                    div()
                        .id(("changes_type_chip", ix))
                        .debug_selector(move || format!("changes_type_chip_{ix}"))
                        .flex_none()
                        .px(ui_scale.px(6.0))
                        .h(components::control_height(ui_scale))
                        .flex()
                        .items_center()
                        .rounded(px(theme.radii.control))
                        .text_size(theme.ui_text(12.0))
                        .whitespace_nowrap()
                        .text_color(if selected {
                            theme.colors.interaction.selected_foreground
                        } else {
                            theme.colors.foreground.secondary
                        })
                        .control_interaction(
                            InteractionStyle::new(theme).selection_outline(false),
                            InteractionState::default()
                                .selected(selected, theme.colors.interaction.selected_background),
                        )
                        .child(format!(".{file_type} {count}"))
                        .on_activate(
                            false,
                            controls::ControlActivation::Action,
                            cx.listener(move |this, e: &ClickEvent, _window, cx| {
                                if e.standard_click() {
                                    this.toggle_changes_type(&file_type, cx);
                                }
                            }),
                        )
                })
                .collect::<Vec<_>>();
            div()
                .px_2()
                .pb_1()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .rounded(px(theme.radii.control))
                        .border_1()
                        .border_color(theme.colors.stroke.default)
                        .bg(theme.colors.surface.raised)
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .py(ui_scale.px(4.0))
                                .child(self.changes_query_input.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(theme.ui_text(11.5))
                                .text_color(theme.colors.foreground.secondary)
                                .child("Enter keeps · Esc clears"),
                        ),
                )
                .when(!type_chips.is_empty(), |bar| {
                    bar.child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_1()
                            .children(type_chips),
                    )
                })
        });

        let body = if let Some(error) = status_error.filter(|_| rows.is_none()) {
            components::empty_state_message(theme, format!("Couldn't read the changes: {error}"))
                .into_any_element()
        } else if rows.is_none() || (loading && counts[0] == 0) {
            components::empty_state_message(theme, "Loading…").into_any_element()
        } else if counts[0] == 0 {
            components::empty_state_message(theme, "Working tree clean.").into_any_element()
        } else if shown == 0 && querying {
            components::empty_state_message(theme, "Nothing matches the filter. Esc clears it.")
                .into_any_element()
        } else if shown == 0 {
            components::empty_state_message(
                theme,
                format!(
                    "Nothing {}. F shows the rest.",
                    filter.label().to_lowercase()
                ),
            )
            .into_any_element()
        } else {
            let list = uniform_list(
                "changes",
                row_count,
                cx.processor(Self::render_changes_rows),
            )
            .h_full()
            .min_h(px(0.0))
            .track_scroll(&self.unstaged_scroll);
            let list = restrict_scroll_to_vertical_axis(list);
            div()
                .id("changes_scroll_container")
                .relative()
                .flex()
                .flex_col()
                .flex_1()
                .h_full()
                .min_h(px(0.0))
                .overflow_hidden()
                .child(
                    div()
                        .flex_1()
                        .h_full()
                        .min_h(px(0.0))
                        .pr(components::Scrollbar::visible_gutter(
                            self.unstaged_scroll.clone(),
                            components::ScrollbarAxis::Vertical,
                        ))
                        .child(list),
                )
                .child(
                    components::Scrollbar::new("changes_scrollbar", self.unstaged_scroll.clone())
                        .render(theme),
                )
                .into_any_element()
        };

        let bounds_for_prepaint = std::rc::Rc::clone(&self.status_sections_bounds_ref);
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .h_full()
            .child(header)
            .child(chips)
            .children(query_bar)
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_hidden()
                    .on_children_prepainted(move |children_bounds, window, _app| {
                        let next = children_bounds.first().copied();
                        let mut measured = bounds_for_prepaint.borrow_mut();
                        if *measured != next {
                            *measured = next;
                            window.refresh();
                        }
                    })
                    .child(div().absolute().top_0().left_0().size_full())
                    .child(body),
            )
            .child(div().px_2().py_2().child(self.commit_box(cx)))
            .into_any_element()
    }

    fn render_changes_rows(
        this: &mut Self,
        range: std::ops::Range<usize>,
        _window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(repo) = this.active_repo() else {
            return Vec::new();
        };
        let Some(rows) = this.changes_rows(repo) else {
            return Vec::new();
        };
        let repo_id = repo.id;
        let open = match repo.diff_state.diff_target.as_ref() {
            Some(DiffTarget::WorkingTree { path, .. }) => Some(path.clone()),
            _ => None,
        };
        let span = this.changes_span(repo, &rows);
        let is_tree = rows.plan.is_tree();
        let theme = this.theme;
        let ui_scale = this.ui_scale();
        let row_height = crate::ui_scale::UiScale::current(cx)
            .row_height(crate::view::rows::STATUS_ROW_HEIGHT_PX, 32.0);
        let detail_width = this
            .current_status_sections_bounds()
            .map(|bounds| bounds.size.width)
            .unwrap_or(Pixels::MAX);
        let alignment_group = (!is_tree).then(|| {
            this.unstaged_path_alignment_group
                .visible_rows(path_alignment_visible_signature(&(
                    repo_id,
                    9u8,
                    this.changes_projection_key(repo),
                    rows.plan.row_len(),
                    range.start,
                    range.end,
                )))
        });
        let mut out = Vec::with_capacity(range.len());
        for ix in range {
            let Some(row) = rows.plan.row_at(RowIx(ix)) else {
                continue;
            };
            match row {
                FileListRow::Directory {
                    key,
                    label,
                    depth,
                    collapsed,
                    chain,
                    subtree: _,
                    additions,
                    deletions,
                } => {
                    let group: SharedString = format!("changes_dir_{}_{ix}", repo_id.0).into();
                    let detail = crate::view::rows::directory_row_detail_for_width(
                        detail_width,
                        depth,
                        additions.is_some() || deletions.is_some(),
                        this.ui_scale_percent,
                    );
                    let action_key = Arc::clone(&key);
                    let action = components::Button::new(
                        format!("changes_dir_action_{ix}"),
                        "Stage / unstage",
                    )
                    .style(components::ButtonStyle::Solid)
                    .on_click(theme, cx, move |this, e, window, cx| {
                        cx.stop_propagation();
                        this.toggle_changes_folder(&action_key, e.position(), window, cx);
                    })
                    .gitcomet_tooltip(
                        theme,
                        "Stage this folder, or unstage it once it's all staged".into(),
                    );
                    out.push(
                        crate::view::rows::directory_row(crate::view::rows::DirectoryRowProps {
                            theme,
                            ui_scale_percent: this.ui_scale_percent,
                            id: ("changes_dir", ix).into(),
                            label: &label,
                            depth,
                            collapsed,
                            additions,
                            deletions,
                            row_height,
                            row_group: Some(group.clone()),
                            detail,
                        })
                        .debug_selector(move || format!("changes_dir_{}_{ix}", repo_id.0))
                        .child(
                            div()
                                .absolute()
                                .right_0()
                                .top_0()
                                .bottom_0()
                                .flex()
                                .items_center()
                                .invisible()
                                .group_hover(group, |d| d.visible())
                                .child(action),
                        )
                        .on_activate(
                            false,
                            controls::ControlActivation::Composite,
                            cx.listener(move |this, e: &ClickEvent, _window, cx| {
                                if !e.standard_click() {
                                    return;
                                }
                                this.toggle_file_list_dir(
                                    repo_id,
                                    FileListId::Changes,
                                    Arc::clone(&key),
                                    Arc::clone(&chain),
                                    collapsed,
                                    cx,
                                );
                            }),
                        )
                        .into_any_element(),
                    );
                }
                FileListRow::File { ordinal, depth } => {
                    let Some(entry_ix) = rows.entry(ordinal) else {
                        continue;
                    };
                    let entry = &rows.list.entries[entry_ix];
                    let lanes = rows.list.lanes[entry_ix];
                    let label = if is_tree {
                        SharedString::from(
                            entry
                                .path
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_else(|| entry.path.display().to_string()),
                        )
                    } else {
                        this.cached_path_display(&entry.path)
                    };
                    let position = rows.plan.display_position(ordinal).unwrap_or(ordinal.0);
                    let selected = open.as_deref() == Some(entry.path.as_path())
                        || span.is_some_and(|(from, to)| (from..=to).contains(&position));
                    out.push(changes_row(
                        ChangesRowCtx {
                            theme,
                            ui_scale,
                            ix,
                            depth,
                            is_tree,
                            repo_id,
                            selected,
                            lanes,
                            stats: rows.list.stats.get(&entry.path).copied(),
                            alignment_group: alignment_group.clone(),
                            active_menu: this.active_context_menu_invoker.clone(),
                            tooltip_host: this.tooltip_host.clone(),
                            row_height,
                        },
                        entry,
                        label,
                        cx,
                    ));
                }
            }
        }
        out
    }
}

struct ChangesRowCtx {
    theme: AppTheme,
    ui_scale: crate::ui_scale::UiScale,
    ix: usize,
    depth: usize,
    is_tree: bool,
    repo_id: RepoId,
    selected: bool,
    lanes: ChangeLanes,
    stats: Option<LineStats>,
    alignment_group: Option<components::PathTruncationAlignmentGroup>,
    active_menu: Option<SharedString>,
    tooltip_host: WeakEntity<TooltipHost>,
    row_height: Pixels,
}

fn changes_row(
    ctx: ChangesRowCtx,
    entry: &FileStatus,
    label: SharedString,
    cx: &mut gpui::Context<DetailsPaneView>,
) -> AnyElement {
    let ChangesRowCtx {
        theme,
        ui_scale,
        ix,
        depth,
        is_tree,
        repo_id,
        selected,
        lanes,
        stats,
        alignment_group,
        active_menu,
        tooltip_host,
        row_height,
    } = ctx;
    let scaled_px = crate::ui_scale::scaler(ui_scale);
    let area = lanes.area();
    let conflicted = lanes.conflicted();
    let (icon, color) = crate::view::rows::file_row_icon(&entry.path, entry.kind, &theme);
    let tint = crate::view::rows::file_kind_row_tint(entry.kind, &theme);
    let badge = crate::view::rows::file_row_kind_badge(entry.kind, &theme);
    let menu_invoker: SharedString =
        format!("changes_file_menu_{}_{}", repo_id.0, entry.path.display()).into();
    let menu_active = active_menu.as_ref() == Some(&menu_invoker);
    let row_group: SharedString = format!("changes_row_{}_{ix}", repo_id.0).into();
    let interaction =
        crate::view::rows::FileRowInteraction::new(theme, tint, selected, menu_active);
    let badge_disc = interaction.badge_disc(row_group.clone());

    let [staged_letter, unstaged_letter] = lanes.letters();
    let letter_color = |staged: bool| {
        if conflicted {
            theme.colors.status.warning.foreground
        } else if staged {
            theme.colors.status.success.foreground
        } else {
            theme.colors.status.danger.foreground
        }
    };
    let letters = div()
        .flex_none()
        .flex()
        .font_family(crate::view::UI_MONOSPACE_FONT_FAMILY)
        .text_size(theme.ui_text(13.0))
        .font_weight(FontWeight::BOLD)
        .child(
            div()
                .text_color(letter_color(true))
                .child(staged_letter.to_string()),
        )
        .child(
            div()
                .text_color(letter_color(false))
                .child(unstaged_letter.to_string()),
        );

    let path = Arc::new(entry.path.clone());
    let path_for_button = Arc::clone(&path);
    let path_for_menu = Arc::clone(&path);
    let path_for_row = Arc::clone(&path);
    let button_label = if conflicted {
        "Resolve…"
    } else if lanes.stages() {
        "Stage"
    } else {
        "Unstage"
    };
    let menu_for_button = menu_invoker.clone();
    let button = components::Button::new(format!("changes_toggle_{ix}"), button_label)
        .style(components::ButtonStyle::Solid)
        .on_click(theme, cx, move |this, e, window, cx| {
            cx.stop_propagation();
            let path = (*path_for_button).clone();
            if conflicted {
                this.open_popover_at(
                    PopoverKind::StatusFileMenu {
                        repo_id,
                        area,
                        path,
                    }
                    .invoked_by(menu_for_button.clone()),
                    e.position(),
                    window,
                    cx,
                );
            } else if lanes.stages() {
                this.stage_changes(repo_id, vec![path], e.position(), window, cx);
            } else {
                this.unstage_changes(repo_id, vec![path], cx);
            }
        })
        .debug_selector(move || format!("changes_toggle_{}_{ix}", repo_id.0))
        .h(components::in_row_control_height(
            crate::ui_scale::UiScale::current(cx).with_appearance(theme.metrics),
        ));

    div()
        .id(("changes_row", ix))
        .debug_selector(move || format!("changes_row_{}_{ix}", repo_id.0))
        .relative()
        .group(row_group.clone())
        .flex()
        .items_center()
        .gap(scaled_px(8.0))
        .pl(if is_tree {
            crate::view::rows::file_row_indent_px(depth, ui_scale.percent())
        } else {
            scaled_px(8.0)
        })
        .pr(scaled_px(8.0))
        .h(row_height)
        .w_full()
        .map(|row| interaction.apply(row))
        .on_pointer_click(
            MouseButton::Right,
            cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                this.open_popover_at(
                    PopoverKind::StatusFileMenu {
                        repo_id,
                        area,
                        path: (*path_for_menu).clone(),
                    }
                    .invoked_by(menu_invoker.clone()),
                    e.position,
                    window,
                    cx,
                );
                cx.notify();
            }),
        )
        .child(letters)
        .child(crate::view::rows::file_row_icon_slot(
            icon,
            color,
            badge,
            badge_disc,
            14.0,
            16.0,
            ui_scale.percent(),
        ))
        .child(
            div()
                .text_size(theme.ui_text(14.0))
                .line_height(theme.ui_text(crate::view::rows::STATUS_ROW_LINE_HEIGHT_PX))
                .flex_1()
                .min_w(px(0.0))
                .child(
                    match alignment_group {
                        Some(group) => components::TruncatedText::aligned_path(
                            label,
                            theme.ui_text(14.0),
                            group,
                        ),
                        None => components::TruncatedText::new(label, theme.ui_text(14.0)),
                    }
                    .id(("changes_row_path", ix))
                    .full_text_tooltip(tooltip_host)
                    .render(cx),
                ),
        )
        .child(div().flex_none().child(components::diff_stat_optional(
            theme,
            ui_scale,
            stats.and_then(|stats| stats.additions),
            stats.and_then(|stats| stats.deletions),
        )))
        .child(
            div()
                .absolute()
                .right_0()
                .top_0()
                .bottom_0()
                .flex()
                .items_center()
                .invisible()
                .group_hover(row_group, |d| d.visible())
                .child(button),
        )
        .on_activate(
            false,
            controls::ControlActivation::Composite,
            cx.listener(move |this, e: &ClickEvent, window, cx| {
                window.focus(&this.panel_focus_handle, cx);
                let path = (*path_for_row).clone();
                let open_path = this
                    .active_repo()
                    .filter(|repo| repo.id == repo_id)
                    .and_then(|repo| match repo.diff_state.diff_target.as_ref() {
                        Some(DiffTarget::WorkingTree { path, .. }) => Some(path.clone()),
                        _ => None,
                    });
                let open = open_path.as_ref() == Some(&path);
                // Shift-click selects from the open file (or the range's
                // anchor) to this one; a plain click drops the range.
                let extend = e.modifiers().shift && open_path.is_some() && !open;
                match open_path.filter(|_| extend) {
                    Some(from) => this.extend_changes_range(repo_id, from, path.clone(), cx),
                    None => this.clear_changes_range(cx),
                }
                if e.standard_click() && open && !extend {
                    this.store.dispatch(Msg::ClearDiffSelection { repo_id });
                } else if conflicted && area == DiffArea::Unstaged {
                    this.store
                        .dispatch(Msg::SelectConflictDiff { repo_id, path });
                } else {
                    this.store.dispatch(Msg::SelectDiff {
                        repo_id,
                        target: DiffTarget::WorkingTree { path, area },
                    });
                }
                cx.notify();
            }),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(path: &str, kind: FileStatusKind) -> FileStatus {
        FileStatus {
            path: PathBuf::from(path),
            kind,
            conflict: None,
        }
    }

    #[test]
    fn every_path_once_with_both_lanes() {
        let worktree = [
            status("b.rs", FileStatusKind::Modified),
            status("new.rs", FileStatusKind::Untracked),
        ];
        let staged = [
            status("a.rs", FileStatusKind::Added),
            status("b.rs", FileStatusKind::Modified),
        ];
        let list = ChangesList::build(&worktree, &staged, None, None);
        let paths: Vec<_> = list.entries.iter().map(|e| e.path.clone()).collect();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("a.rs"),
                PathBuf::from("b.rs"),
                PathBuf::from("new.rs")
            ]
        );
        assert_eq!(list.lanes[0].letters(), ['A', ' ']);
        assert_eq!(list.lanes[1].letters(), ['M', 'M']);
        assert_eq!(list.lanes[2].letters(), ['?', '?']);
        assert_eq!(list.lanes[0].area(), DiffArea::Staged);
        assert_eq!(list.lanes[1].area(), DiffArea::Unstaged);
        assert!(list.lanes[1].stages() && !list.lanes[0].stages());
        assert_eq!(list.position(Path::new("b.rs")), Some(1));
        assert_eq!(list.counts(None), [3, 1, 2, 1]);
    }

    #[test]
    fn conflicts_read_uu_and_stats_add_up() {
        let worktree = [
            status("c.rs", FileStatusKind::Conflicted),
            status("d.rs", FileStatusKind::Modified),
        ];
        let staged = [status("d.rs", FileStatusKind::Modified)];
        let stats = |adds: u32| {
            let mut map = FxHashMap::default();
            map.insert(
                PathBuf::from("d.rs"),
                LineStats {
                    additions: Some(adds),
                    deletions: None,
                },
            );
            map
        };
        let list = ChangesList::build(&worktree, &staged, Some(&stats(2)), Some(&stats(3)));
        assert_eq!(list.lanes[0].letters(), ['U', 'U']);
        assert!(list.lanes[0].conflicted());
        assert_eq!(
            list.stats.get(Path::new("d.rs")),
            Some(&LineStats {
                additions: Some(5),
                deletions: None
            })
        );
    }

    #[test]
    fn a_staged_deletion_beside_the_untracked_file_reads_d_question() {
        let lanes = ChangeLanes {
            staged: Some(FileStatusKind::Deleted),
            unstaged: Some(FileStatusKind::Untracked),
        };
        assert_eq!(lanes.letters(), ['D', '?']);
    }

    #[test]
    fn toggling_stages_what_is_unstaged_else_unstages_what_is_staged() {
        let lanes = |staged, unstaged| ChangeLanes { staged, unstaged };
        let m = Some(FileStatusKind::Modified);
        let rows = [
            (Path::new("a.rs"), lanes(m, m)),
            (Path::new("b.rs"), lanes(m, None)),
            (Path::new("c.rs"), lanes(None, m)),
        ];
        // Filtered: explicit paths, never the empty list that means all.
        assert_eq!(
            toggle_plan(rows.iter().copied(), None, false),
            Some((true, vec![PathBuf::from("a.rs"), PathBuf::from("c.rs")]))
        );
        assert_eq!(
            toggle_plan(rows[1..2].iter().copied(), None, false),
            Some((false, vec![PathBuf::from("b.rs")]))
        );
        assert_eq!(
            toggle_plan(rows[2..].iter().copied(), Some(false), false),
            None
        );
        // Unfiltered: everything.
        assert_eq!(
            toggle_plan(rows.iter().copied(), None, true),
            Some((true, Vec::new()))
        );
    }

    #[test]
    fn stepping_goes_on_from_a_file_the_list_no_longer_shows() {
        let drawn = [Path::new("a.rs"), Path::new("c.rs"), Path::new("e.rs")];
        assert_eq!(changes_step(&drawn, Path::new("c.rs"), 1), Some(2));
        assert_eq!(changes_step(&drawn, Path::new("e.rs"), 1), None);
        assert_eq!(changes_step(&drawn, Path::new("a.rs"), -1), None);
        // b.rs was staged out of an Unstaged filter.
        assert_eq!(changes_step(&drawn, Path::new("b.rs"), 1), Some(1));
        assert_eq!(changes_step(&drawn, Path::new("b.rs"), -1), Some(0));
        assert_eq!(changes_step(&drawn, Path::new("z.rs"), 1), None);
    }

    #[test]
    fn the_filter_matches_fuzzily_and_keeps_file_types() {
        let query = ChangesQuery::parse("pnl FOC .RS");
        assert!(query.matches("src/view/panel_focus.rs", "rs"));
        assert!(!query.matches("src/view/panel_focus.md", "md"));
        // Every word has to match, in any order of words.
        assert!(!query.matches("src/view/panes/details.rs", "rs"));
        // `*.md` too, and several types keep any of them.
        let types = ChangesQuery::parse("*.md .toml");
        assert!(types.matches("docs/shortcuts.md", "md"));
        assert!(types.matches("cargo.toml", "toml"));
        assert!(!types.matches("src/lib.rs", "rs"));
        // A lone dot is a type still being typed, not a filter.
        assert!(ChangesQuery::parse(". *.").is_empty());
        assert!(fuzzy_contains("crates/ünïcode/ß.rs", "üß"));
        assert!(!fuzzy_contains("abc", "cab"));
        assert_eq!(&*file_type(Path::new(".gitignore")), "gitignore");
        assert_eq!(&*file_type(Path::new("src/Main.RS")), "rs");
        assert_eq!(&*file_type(Path::new("Makefile")), "");
    }

    #[test]
    fn the_list_counts_types_most_first_and_filters_by_query() {
        let worktree = [
            status("src/a.rs", FileStatusKind::Modified),
            status("src/b.rs", FileStatusKind::Modified),
            status("docs/c.md", FileStatusKind::Modified),
        ];
        let list = ChangesList::build(&worktree, &[], None, None);
        assert_eq!(list.type_counts, vec![("rs".into(), 2), ("md".into(), 1)]);
        // Path order: docs/c.md, src/a.rs, src/b.rs.
        assert_eq!(
            list.matching(&ChangesQuery::parse(".md")),
            Some(vec![true, false, false])
        );
        assert_eq!(
            list.matching(&ChangesQuery::parse("src b")),
            Some(vec![false, false, true])
        );
        assert_eq!(list.matching(&ChangesQuery::default()), None);
        assert_eq!(list.counts(Some(&[false, true, true])), [2, 2, 0, 0]);
    }

    #[test]
    fn filter_cycles_through_every_kind() {
        let mut filter = ChangesFilter::All;
        let mut seen = Vec::new();
        for _ in 0..4 {
            seen.push(filter);
            filter = filter.next();
        }
        assert_eq!(seen, ChangesFilter::ALL.to_vec());
        assert_eq!(filter, ChangesFilter::All);
    }
}
