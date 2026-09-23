//! Reading the working tree must not take `.git/index.lock`.
//!
//! `git status` opportunistically REWRITES the index to refresh its cached
//! stat data, and takes `.git/index.lock` to do it. That lock is not optional
//! from the perspective of anything else running at the same time: git does not
//! retry, it fails hard with
//!
//!     fatal: Unable to create '.../.git/index.lock': File exists.
//!     Another git process seems to be running in this repository [...]
//!     remove the file manually to continue.
//!
//! Which is a message that blames the user for a race the editor caused, and
//! invites them to go deleting lock files by hand.
//!
//! Magit runs its reads on `spawn_blocking`, so they are concurrent with index
//! mutations by construction — and `spawn_mutation_and_refresh` publishes
//! `BackgroundTaskFinished` after every mutation, which every live magit-status
//! buffer answers by refreshing. So the window is not rare and it is not
//! avoidable by discipline: on a 4418-file repo one refresh measured ~300ms,
//! and staging several entries in a row is the commonest magit workflow there
//! is. Reported 2026-09-23 staging an untracked folder; reproduced by racing
//! `git status` against `git add`, which failed 28 times in 200 attempts, and
//! 0 in 200 once the reader declined optional locks.
//!
//! `index.rs`'s `stage_paths` closed the same bug class narrowly in 2026-08-16
//! by collapsing N `git add`s into one. Its doc comment predicted this one:
//! "a window in which any other git operation in the editor fails".
//!
//! The assertion here is behavioural rather than a check of the argument list:
//! the index file is UNCHANGED across a read. Without
//! `--no-optional-locks` git rewrites it, which is exactly the write that takes
//! the lock.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::Path;
use std::process::Command;

use lattice_vcs::{Repository, WorkingTree};

fn init_temp_repo() -> (tempfile::TempDir, Repository) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?} failed");
    };
    run(&["init"]);
    run(&["config", "user.email", "test@lattice.dev"]);
    run(&["config", "user.name", "lattice-test"]);
    let repo = Repository::discover(dir.path()).expect("discover repo");
    (dir, repo)
}

/// Commit a handful of files, then backdate their mtimes so the index's
/// cached stat data is stale — which is the condition under which `git status`
/// wants to rewrite the index.
fn repo_with_a_stale_stat_cache() -> (tempfile::TempDir, Repository) {
    let (dir, repo) = init_temp_repo();
    for i in 0..200 {
        std::fs::write(dir.path().join(format!("f{i}.txt")), format!("x{i}\n")).unwrap();
    }
    let run = |args: &[&str]| {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .status()
                .expect("git")
                .success(),
            "git {args:?} failed"
        );
    };
    run(&["add", "-A"]);
    run(&["commit", "-qm", "init"]);
    stale_the_stat_cache(dir.path());
    (dir, repo)
}

/// Backdate a few files far enough that git cannot treat them as "racily
/// clean" and must refresh the cached stat data.
fn stale_the_stat_cache(workdir: &Path) {
    for name in ["f5.txt", "f6.txt", "f7.txt"] {
        assert!(
            Command::new("touch")
                .args(["-t", "202001010000", name])
                .current_dir(workdir)
                .status()
                .expect("touch")
                .success()
        );
    }
}

/// A digest of `.git/index`, so a failure prints a number rather than the
/// whole index — which is tens of kilobytes of binary.
fn index_fingerprint(workdir: &Path) -> (usize, u64) {
    use std::hash::{Hash, Hasher};
    let bytes = std::fs::read(workdir.join(".git/index")).expect("read .git/index");
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    (bytes.len(), hasher.finish())
}

#[test]
fn listing_the_working_tree_does_not_rewrite_the_index() {
    let (dir, repo) = repo_with_a_stale_stat_cache();
    let before = index_fingerprint(dir.path());

    WorkingTree::statuses(&repo).expect("statuses");

    assert_eq!(
        before,
        index_fingerprint(dir.path()),
        "a working-tree read rewrote .git/index, which means it took \
         .git/index.lock. Any concurrent stage/unstage/commit then fails with \
         \"Unable to create index.lock: File exists\" — see this file's header."
    );
}

#[test]
fn reading_one_path_does_not_rewrite_the_index() {
    let (dir, repo) = repo_with_a_stale_stat_cache();
    let before = index_fingerprint(dir.path());

    let _ = WorkingTree::path_status(&repo, Path::new("f5.txt"));

    assert_eq!(
        before,
        index_fingerprint(dir.path()),
        "a single-path read rewrote .git/index — same lock, same failure mode"
    );
}

/// The guard is only meaningful if the fixture actually provokes the rewrite.
/// A plain `git status` on this repo MUST rewrite the index; if it stops doing
/// so, the two tests above would pass whether or not the fix is in place.
#[test]
fn the_fixture_provokes_a_rewrite_without_the_flag() {
    let (dir, _repo) = repo_with_a_stale_stat_cache();
    let before = index_fingerprint(dir.path());

    assert!(
        Command::new("git")
            .args(["status", "--porcelain=v1", "-z"])
            .current_dir(dir.path())
            .output()
            .expect("git status")
            .status
            .success()
    );

    assert_ne!(
        before,
        index_fingerprint(dir.path()),
        "plain `git status` did NOT rewrite the index on this fixture, so the \
         tests above prove nothing. Fix the fixture (mtime backdating, file \
         count) before trusting them."
    );
}
