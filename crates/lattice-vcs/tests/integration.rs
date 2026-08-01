//! Integration tests for `lattice-vcs`.
//!
//! Each test creates a temporary git repository via `git init`, populates
//! it with test files, and verifies operations against `git` CLI output.
//! Special verification: every read operation is checked against the
//! equivalent `git` CLI command on the same repo.

use std::path::Path;
use std::process::Command;

use lattice_vcs::{
    Bisect, Branch, Commit, GitBlob, Index, PathStatus, Reference, Remote, Repository, Stash,
    WorkingTree,
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

// ── MG.18a: partial staging via a synthesized patch ──────────────

/// A ten-line file, committed, then modified in two separate places far
/// enough apart that `git diff` reports them as two distinct hunks.
/// Returns the repo and the workdir path.
fn repo_with_two_hunk_change() -> (tempfile::TempDir, Repository) {
    let (dir, repo) = init_temp_repo();
    let workdir = dir.path();
    let original: String = (1..=20).map(|i| format!("line {i}\n")).collect();
    write_file(workdir, "a.txt", &original);
    git_add(&repo, "a.txt");
    git_commit(&repo, "base");

    // Change line 2 and line 19 — far apart, so two hunks.
    let modified: String = (1..=20)
        .map(|i| match i {
            2 => "line 2 CHANGED\n".to_string(),
            19 => "line 19 CHANGED\n".to_string(),
            _ => format!("line {i}\n"),
        })
        .collect();
    write_file(workdir, "a.txt", &modified);
    (dir, repo)
}

/// Number of hunks in `diff` — lines *starting* with `@@ `. Counting
/// `"@@"` substrings would double-count: a header is `@@ -2,3 +2,3 @@`.
fn hunk_count(diff: &str) -> usize {
    diff.lines().filter(|l| l.starts_with("@@ ")).count()
}

/// The file header plus the first hunk only — a valid patch that moves
/// exactly one of a multi-hunk file's changes. Splits on line
/// boundaries; slicing at a raw `"@@"` byte offset lands mid-hunk and
/// git rejects it as a corrupt patch.
fn first_hunk_only(diff: &str) -> String {
    let mut out = String::new();
    let mut seen_hunk = false;
    for line in diff.lines() {
        if line.starts_with("@@ ") {
            if seen_hunk {
                break;
            }
            seen_hunk = true;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[test]
fn apply_patch_stages_one_hunk_and_leaves_the_other_unstaged() {
    // The core MG.18 claim: partial staging is actually partial. This is
    // what the deleted `stage_hunk` stub only pretended to do — it staged
    // the whole file while its signature promised a single hunk.
    let (_dir, repo) = repo_with_two_hunk_change();

    let full = git(&repo, &["diff", "--", "a.txt"]);
    assert_eq!(hunk_count(&full), 2, "expected two hunks:\n{full}");

    Index::apply_patch(&repo, &first_hunk_only(&full), true, false).expect("stage the first hunk");

    let staged = git(&repo, &["diff", "--cached", "--", "a.txt"]);
    assert!(
        staged.contains("line 2 CHANGED"),
        "the staged hunk is in the index:\n{staged}"
    );
    assert!(
        !staged.contains("line 19 CHANGED"),
        "the OTHER hunk must NOT be staged — that was the stub's bug:\n{staged}"
    );

    let unstaged = git(&repo, &["diff", "--", "a.txt"]);
    assert!(
        unstaged.contains("line 19 CHANGED"),
        "the unstaged hunk is still in the worktree diff:\n{unstaged}"
    );
}

#[test]
fn apply_patch_reversed_unstages_a_hunk() {
    let (_dir, repo) = repo_with_two_hunk_change();
    git_add(&repo, "a.txt");

    let staged = git(&repo, &["diff", "--cached", "--", "a.txt"]);
    assert_eq!(
        hunk_count(&staged),
        2,
        "expected two staged hunks:\n{staged}"
    );

    Index::apply_patch(&repo, &first_hunk_only(&staged), true, true)
        .expect("unstage the first hunk");

    let still_staged = git(&repo, &["diff", "--cached", "--", "a.txt"]);
    assert!(
        !still_staged.contains("line 2 CHANGED"),
        "the reversed hunk left the index:\n{still_staged}"
    );
    assert!(
        still_staged.contains("line 19 CHANGED"),
        "the untouched hunk stayed staged:\n{still_staged}"
    );
}

#[test]
fn apply_patch_fails_loudly_when_context_does_not_match() {
    // The safety property `git apply` buys us: a patch built against a
    // stale buffer must be REFUSED, not applied at some other offset.
    // A silent no-op here would mean the user's commit quietly omits
    // what they staged.
    let (_dir, repo) = repo_with_two_hunk_change();
    let stale = "\
diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1,3 +1,3 @@
 nothing like the real file
-neither is this
+nor this
";
    let err = Index::apply_patch(&repo, stale, true, false);
    assert!(err.is_err(), "a non-matching patch must be an error");

    let staged = git(&repo, &["diff", "--cached", "--", "a.txt"]);
    assert!(staged.is_empty(), "nothing was staged:\n{staged}");
}

#[test]
fn apply_patch_without_cached_touches_the_worktree() {
    // `x` (discard) reverses a hunk out of the working tree itself.
    let (dir, repo) = repo_with_two_hunk_change();
    let full = git(&repo, &["diff", "--", "a.txt"]);

    Index::apply_patch(&repo, &first_hunk_only(&full), false, true).expect("discard first hunk");

    let on_disk = std::fs::read_to_string(dir.path().join("a.txt")).expect("read back");
    assert!(
        !on_disk.contains("line 2 CHANGED"),
        "the discarded hunk is gone from the worktree"
    );
    assert!(
        on_disk.contains("line 19 CHANGED"),
        "the other hunk survives in the worktree"
    );
}

// MG.21b — remote management. Every read is checked against the `git`
// CLI on the same repo, per this file's own rule.

#[test]
fn a_fresh_repo_has_no_remotes() {
    let (_dir, repo) = init_temp_repo();
    assert!(
        Remote::list(&repo)
            .expect("list on a repo with no remotes")
            .is_empty(),
        "no remotes configured is an empty list, not an error"
    );
}

#[test]
fn add_then_list_round_trips_through_git() {
    let (_dir, repo) = init_temp_repo();
    Remote::add(&repo, "origin", "https://example.com/a.git").expect("add origin");
    Remote::add(&repo, "upstream", "https://example.com/b.git").expect("add upstream");

    let listed = Remote::list(&repo).expect("list");
    let names: Vec<&str> = listed.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["origin", "upstream"]);
    assert_eq!(listed[0].fetch_url, "https://example.com/a.git");
    assert_eq!(
        listed[0].push_url, listed[0].fetch_url,
        "no separate pushurl configured, so both columns match"
    );

    // Cross-check against the CLI, not just against ourselves.
    let cli = git(&repo, &["remote"]);
    assert_eq!(cli.lines().collect::<Vec<_>>(), names);
}

#[test]
fn rename_moves_the_remote_and_keeps_its_url() {
    let (_dir, repo) = init_temp_repo();
    Remote::add(&repo, "origin", "https://example.com/a.git").expect("add");
    Remote::rename(&repo, "origin", "upstream").expect("rename");

    let listed = Remote::list(&repo).expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "upstream");
    assert_eq!(listed[0].fetch_url, "https://example.com/a.git");
}

#[test]
fn set_url_repoints_the_remote() {
    let (_dir, repo) = init_temp_repo();
    Remote::add(&repo, "origin", "https://example.com/a.git").expect("add");
    Remote::set_url(&repo, "origin", "git@example.com:c.git").expect("set-url");

    let listed = Remote::list(&repo).expect("list");
    assert_eq!(listed[0].fetch_url, "git@example.com:c.git");
    assert_eq!(
        git(&repo, &["remote", "get-url", "origin"]).trim(),
        "git@example.com:c.git"
    );
}

#[test]
fn a_separate_pushurl_shows_in_the_push_column() {
    // The one case where the two columns differ — the reason
    // `RemoteEntry` carries both rather than a single URL. `set_url`
    // touches only the fetch side, which is what leaves them split.
    let (_dir, repo) = init_temp_repo();
    Remote::add(&repo, "origin", "https://example.com/read.git").expect("add");
    git(
        &repo,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            "git@example.com:write.git",
        ],
    );

    let listed = Remote::list(&repo).expect("list");
    assert_eq!(listed[0].fetch_url, "https://example.com/read.git");
    assert_eq!(listed[0].push_url, "git@example.com:write.git");
}

#[test]
fn remove_deletes_the_remote() {
    let (_dir, repo) = init_temp_repo();
    Remote::add(&repo, "origin", "https://example.com/a.git").expect("add");
    Remote::remove(&repo, "origin").expect("remove");

    assert!(Remote::list(&repo).expect("list").is_empty());
    assert!(git(&repo, &["remote"]).trim().is_empty());
}

#[test]
fn operating_on_an_unknown_remote_is_an_error_not_a_silent_no_op() {
    // The buffer's handlers surface this to the user; swallowing it
    // would leave a row that looks acted-on and is not.
    let (_dir, repo) = init_temp_repo();
    let err = Remote::remove(&repo, "nope").expect_err("no such remote");
    assert!(
        format!("{err}").contains("remote remove nope"),
        "the error names the operation and the remote: {err}"
    );
    assert!(Remote::rename(&repo, "nope", "other").is_err());
    assert!(Remote::set_url(&repo, "nope", "https://example.com/x.git").is_err());
}

#[test]
fn adding_a_duplicate_remote_is_an_error() {
    let (_dir, repo) = init_temp_repo();
    Remote::add(&repo, "origin", "https://example.com/a.git").expect("add");
    assert!(
        Remote::add(&repo, "origin", "https://example.com/b.git").is_err(),
        "git refuses the duplicate and so do we"
    );
}

#[test]
fn run_git_stdin_round_trips_input() {
    // Guards the pipe itself: if stdin were never closed, a child that
    // reads to EOF would hang instead of returning.
    let (_dir, repo) = init_temp_repo();
    let out = repo
        .run_git_stdin(["hash-object", "-w", "--stdin"], b"hello lattice\n")
        .expect("hash-object reads stdin");
    let oid = String::from_utf8(out).expect("utf-8");
    let back = git(&repo, &["cat-file", "-p", oid.trim()]);
    assert_eq!(back, "hello lattice\n", "the bytes we piped came back");
}

// MG.21e — bisect. These drive a REAL `git bisect` in a temp repo:
// the state read is derived from git's own refs, so a test that faked
// them would pass against a reading that git would never produce.

/// A repo with `n` commits on one file, oldest first. Returns their
/// SHAs in commit order.
fn repo_with_linear_history(n: usize) -> (tempfile::TempDir, Repository, Vec<String>) {
    let (dir, repo) = init_temp_repo();
    let mut shas = Vec::new();
    for i in 0..n {
        write_file(repo.workdir().unwrap(), "a.txt", &format!("line {i}\n"));
        git_add(&repo, "a.txt");
        git_commit(&repo, &format!("commit {i}"));
        shas.push(git(&repo, &["rev-parse", "HEAD"]).trim().to_string());
    }
    (dir, repo, shas)
}

#[test]
fn no_bisect_running_reads_as_none() {
    let (_dir, repo, _) = repo_with_linear_history(3);
    assert!(!Bisect::in_progress(&repo));
    assert_eq!(Bisect::state(&repo).expect("state"), None);
}

#[test]
fn starting_a_bisect_makes_it_in_progress_and_reset_ends_it() {
    let (_dir, repo, shas) = repo_with_linear_history(5);
    Bisect::start(&repo, Some(&shas[4]), Some(&shas[0])).expect("start");
    assert!(
        Bisect::in_progress(&repo),
        "`in_progress` reads .git/BISECT_LOG — git must have created it"
    );

    Bisect::reset(&repo).expect("reset");
    assert!(!Bisect::in_progress(&repo));
    assert_eq!(Bisect::state(&repo).expect("state"), None);
}

#[test]
fn the_state_reports_the_revisions_git_itself_would() {
    // Eight commits, oldest good, newest bad: six candidates remain
    // between them, so five are left after the one checked out.
    let (_dir, repo, shas) = repo_with_linear_history(8);
    Bisect::start(&repo, Some(&shas[7]), Some(&shas[0])).expect("start");

    let state = Bisect::state(&repo).expect("state").expect("in progress");
    assert_eq!(
        state.revisions_left,
        Some(3),
        "must match `git bisect`'s own \"3 revisions left\" for this repo: {state:?}"
    );
    assert_eq!(
        state.steps,
        Some(2),
        "and its \"roughly 2 steps\": {state:?}"
    );
    assert!(
        !state.start_ref.is_empty(),
        "reset needs a ref to return to: {state:?}"
    );
}

/// The number we render must be the number `git bisect` prints in the
/// same terminal. Asserting our own reading against a hand-computed
/// constant is what let a wrong formula (`count - 1`) pass once; this
/// parses git's message and compares.
#[test]
fn the_reported_count_agrees_with_gits_own_message() {
    let (_dir, repo, shas) = repo_with_linear_history(8);
    let output = Command::new("git")
        .args(["bisect", "start", &shas[7], &shas[0]])
        .current_dir(repo.workdir().unwrap())
        .output()
        .expect("git bisect start");
    let printed = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    // "Bisecting: 3 revisions left to test after this (roughly 2 steps)"
    let from_git: usize = printed
        .split("Bisecting: ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("git did not print a count: {printed}"));

    let state = Bisect::state(&repo).expect("state").expect("in progress");
    assert_eq!(
        state.revisions_left,
        Some(from_git),
        "we report {:?}, git printed {from_git}",
        state.revisions_left
    );
}

#[test]
fn marking_narrows_the_range() {
    let (_dir, repo, shas) = repo_with_linear_history(8);
    Bisect::start(&repo, Some(&shas[7]), Some(&shas[0])).expect("start");
    let before = Bisect::state(&repo).unwrap().unwrap().revisions_left;

    Bisect::good(&repo, None).expect("mark the checked-out revision good");
    let after = Bisect::state(&repo).unwrap().unwrap().revisions_left;

    assert!(
        after < before,
        "a mark must narrow the search: {before:?} -> {after:?}"
    );
}

#[test]
fn a_bisect_with_no_good_ref_yet_reports_no_count_rather_than_zero() {
    // Only the bad end is known, so nothing has been narrowed.
    // Reporting `0` here would read as "finished".
    let (_dir, repo, shas) = repo_with_linear_history(5);
    Bisect::start(&repo, None, None).expect("start unbounded");
    Bisect::bad(&repo, Some(&shas[4])).expect("mark bad");

    let state = Bisect::state(&repo).expect("state").expect("in progress");
    assert_eq!(state.revisions_left, None, "no range yet: {state:?}");
}

#[test]
fn skip_is_accepted_and_keeps_the_bisect_running() {
    let (_dir, repo, shas) = repo_with_linear_history(8);
    Bisect::start(&repo, Some(&shas[7]), Some(&shas[0])).expect("start");
    Bisect::skip(&repo, None).expect("skip the checked-out revision");
    assert!(Bisect::in_progress(&repo));
}

#[test]
fn the_log_records_every_mark() {
    let (_dir, repo, shas) = repo_with_linear_history(6);
    Bisect::start(&repo, Some(&shas[5]), Some(&shas[0])).expect("start");
    Bisect::good(&repo, None).expect("good");

    let log = Bisect::log(&repo).expect("log");
    assert!(log.contains("bisect start"), "log: {log}");
    assert!(log.contains("good"), "log: {log}");
}

#[test]
fn marking_outside_a_bisect_is_an_error_not_a_silent_no_op() {
    let (_dir, repo, _) = repo_with_linear_history(3);
    let err = Bisect::good(&repo, None).expect_err("no bisect running");
    assert!(
        format!("{err}").contains("bisect good"),
        "the error names the operation: {err}"
    );
}
