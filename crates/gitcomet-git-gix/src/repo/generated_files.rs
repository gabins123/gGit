//! Which of a pull request's changed files GitHub would treat as generated
//! (`linguist-generated`), read from `.gitattributes` at a commit's own tree —
//! never the working tree, never a `git check-attr` subprocess.

use gitcomet_core::domain::CommitId;
use gitcomet_core::error::{Error, ErrorKind};
use gitcomet_core::services::Result;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::GixRepo;

impl GixRepo {
    /// The subset of `paths` GitHub would treat as generated at `commit_id`:
    /// an explicit `linguist-generated` in `.gitattributes` wins either way
    /// (including un-marking a path the built-in list would otherwise
    /// match); `gitcomet_core::generated_files`'s built-in list decides only
    /// for a path `.gitattributes` says nothing about.
    ///
    /// Reads `.gitattributes` from `commit_id`'s tree via an in-memory index
    /// built from that tree (`gix`'s own recommended way to source attributes
    /// from a tree rather than the worktree), so the answer depends only on
    /// what the commit actually holds — not on what happens to be checked out.
    pub(super) fn generated_file_paths_at_commit_impl(
        &self,
        commit_id: &CommitId,
        paths: &[PathBuf],
    ) -> Result<BTreeSet<PathBuf>> {
        let repo = self.repo();
        let oid = gix::ObjectId::from_hex(commit_id.0.as_bytes())
            .map_err(|e| Error::new(ErrorKind::Backend(format!("invalid commit id: {e}"))))?;
        let commit = repo
            .find_commit(oid)
            .map_err(|e| Error::new(ErrorKind::Backend(format!("gix find_commit: {e}"))))?;
        let tree_id = commit
            .tree_id()
            .map(|id| id.detach())
            .map_err(|e| Error::new(ErrorKind::Backend(format!("gix tree_id: {e}"))))?;
        let index = repo
            .index_from_tree(&tree_id)
            .map_err(|e| Error::new(ErrorKind::Backend(format!("gix index_from_tree: {e}"))))?;
        // `IdMapping`: read `.gitattributes` blobs by id from this in-memory
        // index, never from the worktree — this commit need not be checked out.
        //
        // `attributes_only` also folds in this repository's global/system
        // `.gitattributes`, `core.attributesFile`, and `info/attributes`, and
        // matches case per `core.ignorecase` — none of which GitHub's own
        // `linguist-generated` resolution considers (it only reads the
        // commit's own `.gitattributes` files, case-sensitively). gix has no
        // cheaper way to scope `attributes_only` to just the tree's own files
        // short of assembling an `AttributeMatchGroup` by hand from the
        // tree's `.gitattributes` blobs, which would mean re-implementing
        // gix's own attribute-file assembly — not worth it for a local
        // config most repositories never set, and one that would only ever
        // make MORE paths generated, never fewer, if it ever did differ.
        let mut attributes = repo
            .attributes_only(
                &index,
                gix::worktree::stack::state::attributes::Source::IdMapping,
            )
            .map_err(|e| Error::new(ErrorKind::Backend(format!("gix attributes_only: {e}"))))?;

        let mut generated = BTreeSet::new();
        for path in paths {
            let linguist_attr = linguist_generated_attr(&mut attributes, path);
            if gitcomet_core::generated_files::is_generated_file(path, linguist_attr) {
                generated.insert(path.clone());
            }
        }
        Ok(generated)
    }
}

/// The resolved `linguist-generated` attribute for `path`, or `None` when
/// `.gitattributes` says nothing about it — not mentioned, or explicitly
/// reset to unspecified with `!linguist-generated`.
fn linguist_generated_attr(attributes: &mut gix::AttributeStack<'_>, path: &Path) -> Option<bool> {
    // A fresh `Outcome` per path: cheap, and it sidesteps any question of
    // whether matches accumulate across `matching_attributes` calls sharing
    // one `Outcome`.
    let mut outcome = gix::attrs::search::Outcome::default();
    attributes
        .at_path(path, None)
        .ok()?
        .matching_attributes(&mut outcome);
    for matched in outcome.iter() {
        if matched.assignment.name.as_str() != "linguist-generated" {
            continue;
        }
        return match matched.assignment.state {
            gix::attrs::StateRef::Set => Some(true),
            gix::attrs::StateRef::Unset => Some(false),
            // GitHub's own docs only ever show `=true`/`=false`; treat any
            // other explicit value the same way a truthy flag would read.
            gix::attrs::StateRef::Value(value) => {
                Some(!value.as_bstr().eq_ignore_ascii_case(b"false"))
            }
            gix::attrs::StateRef::Unspecified => None,
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    fn git_success(workdir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(args)
            .output()
            .expect("git command to run");
        assert!(
            output.status.success(),
            "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_test_repo(workdir: &Path) {
        git_success(workdir, &["init"]);
        for args in [
            ["config", "user.name", "Test User"].as_slice(),
            ["config", "user.email", "test@example.com"].as_slice(),
        ] {
            git_success(workdir, args);
        }
    }

    fn write_file(workdir: &Path, relative: &str, contents: &str) {
        let path = workdir.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent directories");
        }
        fs::write(path, contents).expect("write file");
    }

    fn commit_all(workdir: &Path, message: &str) {
        git_success(workdir, &["add", "-A"]);
        git_success(workdir, &["commit", "-m", message]);
    }

    fn head_commit_id(workdir: &Path) -> CommitId {
        let output = Command::new("git")
            .arg("-C")
            .arg(workdir)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("rev-parse");
        CommitId(String::from_utf8(output.stdout).expect("utf8").trim().into())
    }

    fn open_repo(workdir: &Path) -> GixRepo {
        let thread_safe_repo = gix::open(workdir).expect("open repo").into_sync();
        GixRepo::new(workdir.to_path_buf(), thread_safe_repo)
    }

    #[test]
    fn builtin_lockfile_is_generated_with_no_gitattributes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workdir = tmp.path();
        init_test_repo(workdir);
        write_file(workdir, "Cargo.lock", "# lockfile\n");
        write_file(workdir, "src/main.rs", "fn main() {}\n");
        commit_all(workdir, "initial");

        let repo = open_repo(workdir);
        let commit_id = head_commit_id(workdir);
        let paths = vec![PathBuf::from("Cargo.lock"), PathBuf::from("src/main.rs")];
        let generated = repo
            .generated_file_paths_at_commit_impl(&commit_id, &paths)
            .expect("generated files");

        assert!(generated.contains(Path::new("Cargo.lock")));
        assert!(!generated.contains(Path::new("src/main.rs")));
    }

    #[test]
    fn gitattributes_marks_and_unmarks_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workdir = tmp.path();
        init_test_repo(workdir);
        write_file(
            workdir,
            ".gitattributes",
            "generated/*.rs linguist-generated\nCargo.lock -linguist-generated\n",
        );
        write_file(workdir, "generated/schema.rs", "// generated\n");
        write_file(workdir, "Cargo.lock", "# lockfile\n");
        write_file(workdir, "src/main.rs", "fn main() {}\n");
        commit_all(workdir, "initial");

        let repo = open_repo(workdir);
        let commit_id = head_commit_id(workdir);
        let paths = vec![
            PathBuf::from("generated/schema.rs"),
            PathBuf::from("Cargo.lock"),
            PathBuf::from("src/main.rs"),
        ];
        let generated = repo
            .generated_file_paths_at_commit_impl(&commit_id, &paths)
            .expect("generated files");

        assert!(
            generated.contains(Path::new("generated/schema.rs")),
            "an explicit linguist-generated should mark an ordinary file"
        );
        assert!(
            !generated.contains(Path::new("Cargo.lock")),
            "-linguist-generated should un-mark a built-in lockfile"
        );
        assert!(!generated.contains(Path::new("src/main.rs")));
    }

    /// The whole point of reading the commit's own tree: a file not checked
    /// out to disk (a leftover from a previous checkout, or simply never
    /// checked out in this worktree) still resolves correctly.
    #[test]
    fn reads_gitattributes_from_the_commit_not_the_working_tree() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workdir = tmp.path();
        init_test_repo(workdir);
        write_file(
            workdir,
            ".gitattributes",
            "vendor/bundle.js linguist-generated\n",
        );
        write_file(workdir, "vendor/bundle.js", "// bundled\n");
        commit_all(workdir, "initial");
        let commit_id = head_commit_id(workdir);

        // Remove the working-tree copies; the answer must still come from
        // the commit's tree, not the (now absent) files on disk.
        fs::remove_file(workdir.join(".gitattributes")).expect("remove worktree attributes file");
        fs::remove_file(workdir.join("vendor/bundle.js")).expect("remove worktree file");

        let repo = open_repo(workdir);
        let paths = vec![PathBuf::from("vendor/bundle.js")];
        let generated = repo
            .generated_file_paths_at_commit_impl(&commit_id, &paths)
            .expect("generated files");

        assert!(generated.contains(Path::new("vendor/bundle.js")));
    }
}
