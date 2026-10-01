use gitcomet_core::error::{Error, ErrorKind};
use gitcomet_core::path_utils::git_dir_for_workdir;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, Weak};

/// Open the repository backing the worktree at `workdir`.
///
/// This is the single point in the crate that turns a worktree path into an
/// open [`gix::Repository`]. It routes through [`git_dir_for_workdir`] so that a
/// worktree whose directory ends in `.git` (e.g. `/path/myrepo.git`) is opened
/// via its inner `.git` entry rather than being misread by gix as a bare git
/// directory.
///
/// gix 0.85 offers no open-option to force this: when a path ends in `.git` it
/// sets `looks_like_git_dir` and refuses to append `.git`, and `open_path_as_is`
/// only governs the opposite branch. Resolving the path ourselves is therefore
/// the intended fix — so keep every worktree open going through here.
///
/// The raw [`gix::Error`] is returned so callers can map it to their own
/// error type or treat "not a repository" as absence.
pub(crate) fn open_worktree_repo(workdir: &Path) -> gix::Result<gix::Repository> {
    gix::open(git_dir_for_workdir(workdir))
}

/// Swaps `repo`'s freshly created object store for a live one over the same
/// objects directory, so every handle the backend gives out for one repository
/// — its tab, and each of its linked worktrees — reads through one store.
///
/// A store maps each pack it reads from, and gix maps packs copy-on-write,
/// which Windows charges against the commit limit in full per mapping. One
/// store per worktree handle turned 2 GiB of packs and 14 worktrees into 29 GiB
/// of commit charge; shared, the packs are mapped once.
///
/// Stores match on everything a read can observe: the objects directory, the
/// hash kind, multi-pack-index use and the replacement table (fixed when a
/// store is created, so a repository opened after `refs/replace` changed gets
/// the new table rather than a stale one).
pub(crate) fn share_object_store(repo: &mut gix::ThreadSafeRepository) {
    static LIVE: Mutex<Vec<(PathBuf, Weak<gix::odb::Store>)>> = Mutex::new(Vec::new());

    let fresh = &repo.objects;
    let objects_dir = fresh
        .path()
        .canonicalize()
        .unwrap_or_else(|_| fresh.path().to_path_buf());
    let mut live = LIVE.lock().unwrap_or_else(PoisonError::into_inner);
    live.retain(|(_, store)| store.strong_count() > 0);
    let shared = live.iter().find_map(|(dir, store)| {
        let store = store.upgrade()?;
        (*dir == objects_dir
            && store.object_hash() == fresh.object_hash()
            && store.use_multi_pack_index() == fresh.use_multi_pack_index()
            && store.replacements().eq(fresh.replacements()))
        .then_some(store)
    });
    match shared {
        Some(store) => repo.objects = store,
        None => live.push((objects_dir, Arc::downgrade(fresh))),
    }
}

/// Translate a failed [`open_worktree_repo`] into the crate's error type.
///
/// `context` names the operation that was opening the repository and is only
/// used for the catch-all `Backend` message; the two cases callers act on —
/// "not a repository" and I/O — map to their own kinds so they stay
/// distinguishable. Callers that treat a missing repository as absence rather
/// than an error check [`gix::Error::is_not_found`] themselves instead.
pub(crate) fn map_open_error(error: gix::Error, context: &str) -> Error {
    // gix classifies "not a git repository" (and a missing path) as not-found.
    if error.is_not_found() {
        return Error::new(ErrorKind::NotARepository);
    }
    match error.classify().find_map(|class| class.io_kind()) {
        Some(kind) => Error::new(ErrorKind::Io(kind)),
        None => Error::new(ErrorKind::Backend(format!("{context}: {error}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(workdir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .output()
            .expect("git command to run");
        assert!(
            output.status.success(),
            "git {args:?} failed\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn open_shared(workdir: &Path) -> gix::ThreadSafeRepository {
        let mut repo = open_worktree_repo(workdir).expect("open repo").into_sync();
        share_object_store(&mut repo);
        repo
    }

    #[test]
    fn a_repository_and_its_linked_worktrees_share_one_object_store() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        let linked = dir.path().join("linked");
        let other = dir.path().join("other");
        for repo in [&main, &other] {
            std::fs::create_dir(repo).unwrap();
            git(repo, &["init", "-q"]);
            git(repo, &["commit", "-q", "--allow-empty", "-m", "one"]);
        }
        git(&main, &["worktree", "add", "-q", linked.to_str().unwrap()]);

        let main_repo = open_shared(&main);
        let linked_repo = open_shared(&linked);
        assert!(Arc::ptr_eq(&main_repo.objects, &linked_repo.objects));
        assert!(!Arc::ptr_eq(&main_repo.objects, &open_shared(&other).objects));
        assert!(linked_repo.to_thread_local().head_commit().is_ok());

        // An inherited store still finds what was written after it was created.
        git(&main, &["commit", "-q", "--allow-empty", "-m", "two"]);
        git(&main, &["gc", "-q"]);
        let later = open_shared(&main);
        assert!(Arc::ptr_eq(&main_repo.objects, &later.objects));
        let later = later.to_thread_local();
        let head = later.head_commit().expect("new head");
        assert_eq!(head.message_raw_sloppy(), "two\n");
    }
}
