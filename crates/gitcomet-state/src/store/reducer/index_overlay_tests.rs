use super::*;
use crate::model::RepoState;
use crate::msg::{InternalMsg, RepoActionKind};
use gitcomet_core::domain::{FileConflictKind, FileStatus, FileStatusKind, RepoSpec, RepoStatus};
use std::path::PathBuf;

const ID: RepoId = RepoId(77);

fn fs(path: &str, kind: FileStatusKind) -> FileStatus {
    FileStatus {
        path: PathBuf::from(path),
        kind,
        conflict: None,
    }
}

fn with_status(unstaged: Vec<FileStatus>, staged: Vec<FileStatus>) -> AppState {
    let mut state = AppState::test_default();
    let mut repo = RepoState::new_opening(
        ID,
        RepoSpec {
            workdir: PathBuf::from("repo"),
        },
    );
    repo.set_status(Loadable::Ready(Arc::new(RepoStatus {
        staged: Arc::new(staged),
        unstaged: Arc::new(unstaged),
    })));
    state.repos.push(repo);
    state
}

fn send(state: &mut AppState, msg: Msg) -> Vec<Effect> {
    let mut repos = FxHashMap::default();
    reduce(&mut repos, &AtomicU64::new(1), state, msg)
}

fn stage(state: &mut AppState, paths: &[&str]) {
    let paths = paths.iter().map(PathBuf::from).collect::<Vec<_>>();
    send(
        state,
        Msg::StagePaths {
            repo_id: ID,
            paths: RepoPathList::new(paths),
        },
    );
}

fn unstage(state: &mut AppState, paths: &[&str]) {
    let paths = paths.iter().map(PathBuf::from).collect::<Vec<_>>();
    send(
        state,
        Msg::UnstagePaths {
            repo_id: ID,
            paths: RepoPathList::new(paths),
        },
    );
}

fn lanes(state: &AppState) -> (Vec<FileStatus>, Vec<FileStatus>) {
    let repo = &state.repos[0];
    (
        repo.worktree_status_entries().unwrap().to_vec(),
        repo.staged_status_entries().unwrap().to_vec(),
    )
}

#[test]
fn stage_moves_modified_entry_immediately() {
    let mut state = with_status(
        vec![
            fs("a.txt", FileStatusKind::Modified),
            fs("c.txt", FileStatusKind::Modified),
        ],
        vec![fs("b.txt", FileStatusKind::Added)],
    );
    let rev = state.repos[0].staged_status_rev;
    stage(&mut state, &["c.txt"]);
    let (unstaged, staged) = lanes(&state);
    assert_eq!(unstaged, vec![fs("a.txt", FileStatusKind::Modified)]);
    assert_eq!(
        staged,
        vec![
            fs("b.txt", FileStatusKind::Added),
            fs("c.txt", FileStatusKind::Modified)
        ]
    );
    assert_ne!(state.repos[0].staged_status_rev, rev);
}

#[test]
fn stage_turns_untracked_into_added_under_a_directory() {
    let mut state = with_status(
        vec![
            fs("dir/new.txt", FileStatusKind::Untracked),
            fs("dir/sub/deep.txt", FileStatusKind::Modified),
            fs("dirx.txt", FileStatusKind::Untracked),
        ],
        vec![fs("dir/m.txt", FileStatusKind::Modified)],
    );
    stage(&mut state, &["dir"]);
    let (unstaged, staged) = lanes(&state);
    assert_eq!(unstaged, vec![fs("dirx.txt", FileStatusKind::Untracked)]);
    assert_eq!(
        staged,
        vec![
            fs("dir/m.txt", FileStatusKind::Modified),
            fs("dir/new.txt", FileStatusKind::Added),
            fs("dir/sub/deep.txt", FileStatusKind::Modified),
        ]
    );
}

#[test]
fn stage_of_mm_keeps_staged_kind() {
    let mut state = with_status(
        vec![fs("a.txt", FileStatusKind::Modified)],
        vec![fs("a.txt", FileStatusKind::Added)],
    );
    stage(&mut state, &["a.txt"]);
    let (unstaged, staged) = lanes(&state);
    assert!(unstaged.is_empty());
    assert_eq!(staged, vec![fs("a.txt", FileStatusKind::Added)]);
}

#[test]
fn unstage_of_added_goes_back_to_untracked() {
    let mut state = with_status(vec![], vec![fs("a.txt", FileStatusKind::Added)]);
    unstage(&mut state, &["a.txt"]);
    let (unstaged, staged) = lanes(&state);
    assert_eq!(unstaged, vec![fs("a.txt", FileStatusKind::Untracked)]);
    assert!(staged.is_empty());
}

#[test]
fn conflicted_and_renamed_entries_are_untouched() {
    let conflicted = FileStatus {
        path: PathBuf::from("c.txt"),
        kind: FileStatusKind::Modified,
        conflict: Some(FileConflictKind::AddedByUs),
    };
    let mut state = with_status(
        vec![conflicted.clone(), fs("k.txt", FileStatusKind::Conflicted)],
        vec![fs("r.txt", FileStatusKind::Renamed)],
    );
    stage(&mut state, &[]);
    unstage(&mut state, &[]);
    let (unstaged, staged) = lanes(&state);
    assert_eq!(
        unstaged,
        vec![conflicted, fs("k.txt", FileStatusKind::Conflicted)]
    );
    assert_eq!(staged, vec![fs("r.txt", FileStatusKind::Renamed)]);
    assert!(state.repos[0].has_unstaged_conflicts);
}

#[test]
fn empty_paths_stage_everything() {
    let mut state = with_status(
        vec![
            fs("a.txt", FileStatusKind::Modified),
            fs("b.txt", FileStatusKind::Untracked),
        ],
        vec![],
    );
    stage(&mut state, &[]);
    let (unstaged, staged) = lanes(&state);
    assert!(unstaged.is_empty());
    assert_eq!(
        staged,
        vec![
            fs("a.txt", FileStatusKind::Modified),
            fs("b.txt", FileStatusKind::Added)
        ]
    );
}

#[test]
fn stale_loads_while_pending_still_show_file_staged() {
    let stale_unstaged = vec![fs("a.txt", FileStatusKind::Modified)];
    let mut state = with_status(stale_unstaged.clone(), vec![]);
    stage(&mut state, &["a.txt"]);

    send(
        &mut state,
        Msg::Internal(InternalMsg::StatusLoaded {
            repo_id: ID,
            result: Ok(RepoStatus {
                staged: Arc::new(vec![]),
                unstaged: Arc::new(stale_unstaged.clone()),
            }),
        }),
    );
    let expected = (vec![], vec![fs("a.txt", FileStatusKind::Modified)]);
    assert_eq!(lanes(&state), expected);

    // Single-lane loads: the staged lane must re-insert the moved entry even
    // though the shown worktree lane no longer holds it.
    send(
        &mut state,
        Msg::Internal(InternalMsg::StagedStatusLoaded {
            repo_id: ID,
            result: Ok(vec![]),
        }),
    );
    send(
        &mut state,
        Msg::Internal(InternalMsg::WorktreeStatusLoaded {
            repo_id: ID,
            result: Ok(stale_unstaged),
        }),
    );
    assert_eq!(lanes(&state), expected);
}

#[test]
fn op_is_dropped_once_stage_paths_finishes() {
    let mut state = with_status(vec![fs("a.txt", FileStatusKind::Modified)], vec![]);
    stage(&mut state, &["a.txt"]);
    stage(&mut state, &["b.txt"]);
    assert_eq!(state.repos[0].pending_index_ops.len(), 2);

    let finished = || {
        Msg::Internal(InternalMsg::RepoActionFinished {
            repo_id: ID,
            action: RepoActionKind::StagePaths,
            result: Ok(()),
        })
    };
    send(&mut state, finished());
    // Finishes arrive in request order, so the oldest op retires.
    assert_eq!(state.repos[0].pending_index_ops.len(), 1);
    // The optimistic rows stay until the reload lands.
    assert_eq!(lanes(&state).1, vec![fs("a.txt", FileStatusKind::Modified)]);

    // The retired op no longer hides a later external reset of its path.
    send(
        &mut state,
        Msg::Internal(InternalMsg::StatusLoaded {
            repo_id: ID,
            result: Ok(RepoStatus {
                staged: Arc::new(vec![]),
                unstaged: Arc::new(vec![fs("a.txt", FileStatusKind::Modified)]),
            }),
        }),
    );
    assert_eq!(
        lanes(&state),
        (vec![fs("a.txt", FileStatusKind::Modified)], vec![])
    );

    send(&mut state, finished());
    assert!(state.repos[0].pending_index_ops.is_empty());
}
