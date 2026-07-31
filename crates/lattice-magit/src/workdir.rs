//! B3: repository discovery, in one place.
//!
//! Every magit mode needs "where is the repository", and every one of
//! them spelled it out — eleven copies of
//! `Repository::discover(…).ok().and_then(|r| r.workdir().map(…))`.
//!
//! **This is not tidying.** `gix::discover` takes a *directory*, and
//! passing it a file path fails silently: `discover` returns `Err`, the
//! `.ok()` swallows it, and the caller gets a default. MG.11 found
//! three sites doing exactly that, one of them in `lattice-host`'s
//! auto-head-diff subsystem — which meant gutter diff signs had never
//! worked, for any file, since they landed. Two functions, one per
//! question, make that mistake unrepresentable: a caller with a file
//! path cannot reach the directory-taking one.

use std::path::{Path, PathBuf};

use lattice_vcs::Repository;

/// The working directory of the repository containing the current
/// directory.
///
/// `None` for "not in a repository" and for a bare one (which has no
/// working tree, so there is nothing for a magit buffer to show). Most
/// callers want `.unwrap_or_default()` — an empty path makes the
/// subsequent git call fail and the buffer say so, which is the same
/// outcome the hand-written copies produced.
pub(crate) fn magit_workdir() -> Option<PathBuf> {
    let repo = Repository::discover(".").ok()?;
    Some(repo.workdir()?.to_path_buf())
}

/// The `(workdir, repo-relative-path)` pair for a file on disk.
///
/// Takes the file's **parent** to `discover`, which is the whole point
/// of this existing separately — see the module note. The relative
/// path falls back to the input when it does not sit under the
/// workdir, which is what a symlinked or otherwise unexpected path
/// does; git will then reject it by name rather than silently acting
/// on something else.
pub(crate) fn workdir_for_file(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let repo = Repository::discover(path.parent()?).ok()?;
    let workdir = repo.workdir()?.to_path_buf();
    let rel = path.strip_prefix(&workdir).unwrap_or(path).to_path_buf();
    Some((workdir, rel))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git_ok(dir: &Path, args: &[&str]) {
        let st = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git");
        assert!(st.success(), "git {args:?} failed");
    }

    /// The bug this module exists to prevent, stated directly: a file
    /// path handed to the directory-taking discovery finds nothing.
    ///
    /// Asserted against `gix` itself rather than against our wrapper,
    /// because the behaviour being relied on is `gix`'s — if a future
    /// version starts accepting file paths, this test says so instead
    /// of the split quietly becoming pointless.
    #[test]
    fn discovery_needs_a_directory_which_is_why_the_file_form_is_separate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        git_ok(p, &["init"]);
        std::fs::write(p.join("a.txt"), "x\n").unwrap();

        assert!(
            Repository::discover(p).is_ok(),
            "a directory discovers the repo"
        );
        assert!(
            Repository::discover(p.join("a.txt")).is_err(),
            "a FILE path does not — and `.ok()` on this is what made \
             three sites fail silently before MG.11 found them"
        );
    }

    #[test]
    fn workdir_for_file_resolves_the_path_relative_to_the_repository() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        git_ok(p, &["init"]);
        std::fs::create_dir(p.join("src")).unwrap();
        let file = p.join("src").join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();

        let (workdir, rel) = workdir_for_file(&file).expect("inside a repo");
        assert_eq!(rel, Path::new("src/main.rs"), "repo-relative, not absolute");
        assert!(
            file.starts_with(&workdir),
            "and the workdir must contain it: {workdir:?}"
        );
    }

    /// A path with no parent cannot be discovered from, and saying so
    /// beats discovering from the process's current directory — which
    /// would silently resolve to whatever repo the editor was launched
    /// in.
    #[test]
    fn a_parentless_path_is_declined_rather_than_guessed() {
        assert!(workdir_for_file(Path::new("")).is_none());
    }
}
