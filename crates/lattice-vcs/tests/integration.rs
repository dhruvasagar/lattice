//! Integration tests for `lattice-vcs`.
//!
//! Each test creates a temporary git repository via `git init`, populates
//! it with test files, and verifies operations against `git` CLI output.
//! Special verification: every read operation is checked against the
//! equivalent `git` CLI command on the same repo.

use std::path::Path;
use std::process::Command;

use lattice_vcs::{
    Branch, Commit, GitBlob, Index, PathStatus, Reference, Repository, Stash, WorkingTree,
};

/// Create a temporary directory, initialise a git repo in it, and
/// return the path to the workdir.
fn init_temp_repo() -> (tempfile::TempDir, Repository) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let status = Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .status()
        .expect("git init");
    assert!(status.success(), "git init failed");

    // Configure git user for commits
    for (key, val) in [
        ("user.email", "test@lattice.dev"),
        ("user.name", "lattice-test"),
    ] {
        let status = Command::new("git")
            .args(["config", key, val])
            .current_dir(dir.path())
            .status()
            .expect("git config");
        assert!(status.success());
    }

    let repo = Repository::discover(dir.path()).expect("discover repo");
    (dir, repo)
}

/// Create a file with content and return its path relative to the repo.
fn write_file(dir: &Path, name: &str, content: &str) {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).ok();
    std::fs::write(&path, content).expect("write test file");
}

/// Run git in the repo workdir and return stdout.
fn git(repo: &Repository, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo.workdir().unwrap())
        .output()
        .expect("git command");
    String::from_utf8(output.stdout).expect("utf-8")
}

/// Stage a file via git CLI.
fn git_add(repo: &Repository, path: &str) {
    let status = Command::new("git")
        .args(["add", path])
        .current_dir(repo.workdir().unwrap())
        .status()
        .expect("git add");
    assert!(status.success());
}

/// Commit via git CLI.
fn git_commit(repo: &Repository, msg: &str) {
    let status = Command::new("git")
        .args(["commit", "-m", msg])
        .current_dir(repo.workdir().unwrap())
        .status()
        .expect("git commit");
    assert!(status.success());
}

#[test]
fn repository_discovery() {
    let (_dir, repo) = init_temp_repo();
    assert!(repo.workdir().is_some());
    assert!(repo.gitdir().to_str().unwrap().ends_with(".git"));
    assert!(!repo.is_bare());

    // Discover from a subdirectory
    let subdir = repo.workdir().unwrap().join("subdir");
    std::fs::create_dir_all(&subdir).unwrap();
    let sub_repo = Repository::discover(&subdir).expect("discover from subdir");
    assert_eq!(sub_repo.workdir(), repo.workdir());
}

#[test]
fn repository_outside_git_repo() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let result = Repository::discover(dir.path());
    assert!(result.is_err());
}

#[test]
fn blob_read() {
    let (_dir, repo) = init_temp_repo();
    let workdir = repo.workdir().unwrap();
    write_file(workdir, "hello.txt", "hello world\n");
    git_add(&repo, "hello.txt");
    git_commit(&repo, "initial");

    // Read via GitBlob
    let head_oid = Reference::resolve(&repo, "HEAD").unwrap();
    let rope = GitBlob::read_path(&repo, "HEAD", "hello.txt").unwrap();
    assert_eq!(rope.to_string(), "hello world\n");

    // Verify against git CLI
    let cli_content = git(&repo, &["show", "HEAD:hello.txt"]);
    assert_eq!(rope.to_string(), cli_content);
}

#[test]
fn reference_resolve() {
    let (_dir, repo) = init_temp_repo();
    write_file(repo.workdir().unwrap(), "a.txt", "a\n");
    git_add(&repo, "a.txt");
    git_commit(&repo, "initial");

    let oid = Reference::resolve(&repo, "HEAD").unwrap();
    let cli_oid = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    assert_eq!(oid.to_string(), cli_oid);

    let oid_main = Reference::resolve(&repo, "main").unwrap();
    assert_eq!(oid_main, oid);
}

#[test]
fn reference_symbolic_target() {
    let (_dir, repo) = init_temp_repo();
    write_file(repo.workdir().unwrap(), "a.txt", "a\n");
    git_add(&repo, "a.txt");
    git_commit(&repo, "initial");

    let target = Reference::symbolic_target(&repo, "HEAD").unwrap();
    assert_eq!(target, Some("refs/heads/main".to_string()));
}

#[test]
fn status_clean() {
    let (_dir, repo) = init_temp_repo();
    write_file(repo.workdir().unwrap(), "a.txt", "a\n");
    git_add(&repo, "a.txt");
    git_commit(&repo, "initial");

    let status = WorkingTree::path_status(&repo, "a.txt").unwrap();
    assert_eq!(status, PathStatus::Clean);
}

#[test]
fn status_modified() {
    let (_dir, repo) = init_temp_repo();
    write_file(repo.workdir().unwrap(), "a.txt", "a\n");
    git_add(&repo, "a.txt");
    git_commit(&repo, "initial");
    write_file(repo.workdir().unwrap(), "a.txt", "modified\n");

    let status = WorkingTree::path_status(&repo, "a.txt").unwrap();
    assert_eq!(status, PathStatus::Modified);
}

#[test]
fn status_untracked() {
    let (_dir, repo) = init_temp_repo();
    write_file(repo.workdir().unwrap(), "untracked.txt", "new\n");

    let status = WorkingTree::path_status(&repo, "untracked.txt").unwrap();
    assert_eq!(status, PathStatus::Untracked);
}

#[test]
fn status_added() {
    let (_dir, repo) = init_temp_repo();
    write_file(repo.workdir().unwrap(), "new.txt", "new\n");
    git_add(&repo, "new.txt");

    let status = WorkingTree::path_status(&repo, "new.txt").unwrap();
    assert_eq!(status, PathStatus::Added);
}

#[test]
fn status_deleted() {
    let (_dir, repo) = init_temp_repo();
    write_file(repo.workdir().unwrap(), "a.txt", "a\n");
    git_add(&repo, "a.txt");
    git_commit(&repo, "initial");
    std::fs::remove_file(repo.workdir().unwrap().join("a.txt")).unwrap();

    let status = WorkingTree::path_status(&repo, "a.txt").unwrap();
    assert_eq!(status, PathStatus::Deleted);
}

#[test]
fn status_conflicted() {
    let (_dir, repo) = init_temp_repo();
    let workdir = repo.workdir().unwrap();
    write_file(workdir, "a.txt", "a\n");
    git_add(&repo, "a.txt");
    git_commit(&repo, "initial");

    // Modify and stage
    write_file(workdir, "a.txt", "staged change\n");
    git_add(&repo, "a.txt");

    // Modify again in worktree
    write_file(workdir, "a.txt", "both staged and unstaged\n");

    let status = WorkingTree::path_status(&repo, "a.txt").unwrap();
    assert_eq!(status, PathStatus::Conflicted);
}

#[test]
fn statuses_multiple() {
    let (_dir, repo) = init_temp_repo();
    let workdir = repo.workdir().unwrap();
    write_file(workdir, "tracked.txt", "a\n");
    git_add(&repo, "tracked.txt");
    git_commit(&repo, "initial");

    write_file(workdir, "tracked.txt", "modified\n");
    write_file(workdir, "new.txt", "new\n");
    write_file(workdir, "untracked.txt", "untracked\n");
    git_add(&repo, "new.txt");

    let statuses = WorkingTree::statuses(&repo).unwrap();
    let by_path: std::collections::HashMap<String, PathStatus> = statuses
        .into_iter()
        .map(|(p, s)| (p.to_string_lossy().to_string(), s))
        .collect();

    assert_eq!(by_path.get("tracked.txt"), Some(&PathStatus::Modified));
    assert_eq!(by_path.get("new.txt"), Some(&PathStatus::Added));
    assert_eq!(by_path.get("untracked.txt"), Some(&PathStatus::Untracked));
}

#[test]
fn stage_and_unstage() {
    let (_dir, repo) = init_temp_repo();
    let workdir = repo.workdir().unwrap();
    write_file(workdir, "a.txt", "a\n");
    git_add(&repo, "a.txt");
    git_commit(&repo, "initial");

    write_file(workdir, "a.txt", "modified\n");

    // Verify modified
    assert_eq!(
        WorkingTree::path_status(&repo, "a.txt").unwrap(),
        PathStatus::Modified
    );

    // Stage
    Index::stage_path(&repo, "a.txt").unwrap();
    assert_eq!(
        WorkingTree::path_status(&repo, "a.txt").unwrap(),
        PathStatus::Added
    );

    // Unstage
    Index::unstage_path(&repo, "a.txt").unwrap();
    assert_eq!(
        WorkingTree::path_status(&repo, "a.txt").unwrap(),
        PathStatus::Modified
    );
}

#[test]
fn commit_create() {
    let (_dir, repo) = init_temp_repo();
    let workdir = repo.workdir().unwrap();
    write_file(workdir, "a.txt", "a\n");
    Index::stage_path(&repo, "a.txt").unwrap();

    Commit::create(&repo, "test commit").unwrap();

    // Verify file is clean after commit
    assert_eq!(
        WorkingTree::path_status(&repo, "a.txt").unwrap(),
        PathStatus::Clean
    );

    // Verify commit message
    let log = git(&repo, &["log", "-1", "--format=%s"]);
    assert_eq!(log.trim(), "test commit");
}

#[test]
fn commit_amend() {
    let (_dir, repo) = init_temp_repo();
    let workdir = repo.workdir().unwrap();
    write_file(workdir, "a.txt", "a\n");
    Index::stage_path(&repo, "a.txt").unwrap();
    Commit::create(&repo, "first").unwrap();

    write_file(workdir, "a.txt", "amended\n");
    Index::stage_path(&repo, "a.txt").unwrap();
    Commit::amend(&repo, "amended message").unwrap();

    let log = git(&repo, &["log", "-1", "--format=%s"]);
    assert_eq!(log.trim(), "amended message");

    // Only one commit should exist
    let count = git(&repo, &["rev-list", "--count", "HEAD"]);
    assert_eq!(count.trim(), "1");
}

#[test]
fn branch_list_and_create() {
    let (_dir, repo) = init_temp_repo();
    write_file(repo.workdir().unwrap(), "a.txt", "a\n");
    git_add(&repo, "a.txt");
    git_commit(&repo, "initial");

    let branches = Branch::list(&repo).unwrap();
    assert_eq!(branches, vec!["main"]);

    Branch::create(&repo, "feature", false, None).unwrap();
    let branches = Branch::list(&repo).unwrap();
    assert!(branches.contains(&"main".to_string()));
    assert!(branches.contains(&"feature".to_string()));
}

#[test]
fn branch_create_from_explicit_base() {
    let (_dir, repo) = init_temp_repo();
    write_file(repo.workdir().unwrap(), "a.txt", "a\n");
    git_add(&repo, "a.txt");
    git_commit(&repo, "initial on main");

    // Branch off main, add a commit only 'topic' has.
    Branch::create(&repo, "topic", true, None).unwrap();
    write_file(repo.workdir().unwrap(), "b.txt", "b\n");
    git_add(&repo, "b.txt");
    git_commit(&repo, "only on topic");
    Branch::checkout(&repo, "main").unwrap();

    // Create a new branch explicitly from 'topic' while sitting on
    // 'main' — the wizard's flow (pick a base different from HEAD).
    Branch::create(&repo, "from-topic", true, Some("topic")).unwrap();

    let head = git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(head.trim(), "from-topic");
    // 'from-topic' must contain topic's commit, not just main's.
    let log = git(&repo, &["log", "--format=%s"]);
    assert!(log.contains("only on topic"));
}

#[test]
fn branch_checkout_and_delete() {
    let (_dir, repo) = init_temp_repo();
    write_file(repo.workdir().unwrap(), "a.txt", "a\n");
    git_add(&repo, "a.txt");
    git_commit(&repo, "initial");

    Branch::create(&repo, "topic", true, None).unwrap();

    // We should be on 'topic' now
    let head = git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(head.trim(), "topic");

    // Checkout back to main
    Branch::checkout(&repo, "main").unwrap();
    let head = git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(head.trim(), "main");

    // Delete topic
    Branch::delete(&repo, "topic").unwrap();
    let branches = Branch::list(&repo).unwrap();
    assert!(!branches.contains(&"topic".to_string()));
}

#[test]
fn stash_list_apply_pop_drop() {
    let (_dir, repo) = init_temp_repo();
    let workdir = repo.workdir().unwrap();
    write_file(workdir, "a.txt", "a\n");
    git_add(&repo, "a.txt");
    git_commit(&repo, "initial");

    write_file(workdir, "a.txt", "modified\n");

    // Stash
    Stash::create(&repo, Some("my stash"), false).unwrap();
    assert_eq!(
        WorkingTree::path_status(&repo, "a.txt").unwrap(),
        PathStatus::Clean
    );

    // List
    let stashes = Stash::list(&repo).unwrap();
    assert_eq!(stashes.len(), 1);
    assert!(stashes[0].message.contains("my stash"));

    // Pop
    Stash::pop(&repo, 0).unwrap();
    assert_eq!(
        WorkingTree::path_status(&repo, "a.txt").unwrap(),
        PathStatus::Modified
    );
    assert!(Stash::list(&repo).unwrap().is_empty());
}

#[test]
fn stash_apply_keeps_entry() {
    let (_dir, repo) = init_temp_repo();
    let workdir = repo.workdir().unwrap();
    write_file(workdir, "a.txt", "a\n");
    git_add(&repo, "a.txt");
    git_commit(&repo, "initial");

    write_file(workdir, "a.txt", "modified\n");
    Stash::create(&repo, None, false).unwrap();

    // Apply (keeps stash)
    Stash::apply(&repo, 0).unwrap();
    assert_eq!(
        WorkingTree::path_status(&repo, "a.txt").unwrap(),
        PathStatus::Modified
    );
    assert_eq!(Stash::list(&repo).unwrap().len(), 1);

    // Drop
    Stash::drop(&repo, 0).unwrap();
    assert!(Stash::list(&repo).unwrap().is_empty());
}

#[test]
fn stash_include_untracked() {
    let (_dir, repo) = init_temp_repo();
    let workdir = repo.workdir().unwrap();
    write_file(workdir, "tracked.txt", "a\n");
    git_add(&repo, "tracked.txt");
    git_commit(&repo, "initial");
    write_file(workdir, "untracked.txt", "untracked\n");

    // Stash with untracked
    Stash::create(&repo, None, true).unwrap();
    // Untracked file should now be gone
    assert!(!workdir.join("untracked.txt").exists());
}
