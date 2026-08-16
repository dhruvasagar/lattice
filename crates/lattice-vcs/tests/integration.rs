//! Integration tests for `lattice-vcs`.
//!
//! Each test creates a temporary git repository via `git init`, populates
//! it with test files, and verifies operations against `git` CLI output.
//! Special verification: every read operation is checked against the
//! equivalent `git` CLI command on the same repo.

use std::path::Path;
use std::process::Command;

use lattice_vcs::{
    Bisect, Branch, Commit, GitBlob, Index, PathChange, PathStatus, Reference, Remote, Repository,
    Stash, Submodule, SubmoduleState, WorkingTree,
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
fn stage_paths_stages_every_path_in_one_command() {
    // Reported 2026-08-16: staging a visual-mode selection reported
    // "Unable to create '.git/index.lock': File exists". Each `git add`
    // takes that lock for the duration of its process, so N files staged
    // as N commands is N lock cycles — a window per file in which any
    // other git operation in the editor fails.
    let (dir, repo) = init_temp_repo();
    for name in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(dir.path().join(name), "x\n").unwrap();
    }
    Index::stage_paths(&repo, ["a.txt", "b.txt", "c.txt"]).unwrap();
    let staged = git(&repo, &["diff", "--cached", "--name-only"]);
    let mut names: Vec<&str> = staged.lines().collect();
    names.sort_unstable();
    assert_eq!(names, vec!["a.txt", "b.txt", "c.txt"]);
}

#[test]
fn stage_paths_is_atomic_across_the_selection() {
    // One command means all-or-nothing. The loop it replaced could fail
    // partway and leave half a selection staged, which is why the magit
    // layer had to describe "3 of 5 staged" as an outcome at all.
    let (dir, repo) = init_temp_repo();
    std::fs::write(dir.path().join("real.txt"), "x\n").unwrap();
    // A path that does not exist makes `git add` reject the WHOLE
    // invocation, so the real file must not be staged either.
    let err = Index::stage_paths(&repo, ["real.txt", "missing.txt"]);
    assert!(err.is_err(), "git rejects the batch");
    let staged = git(&repo, &["diff", "--cached", "--name-only"]);
    assert!(
        staged.trim().is_empty(),
        "a rejected batch stages nothing, got {staged:?}"
    );
}

#[test]
fn stage_path_is_the_single_path_case_of_stage_paths() {
    let (dir, repo) = init_temp_repo();
    std::fs::write(dir.path().join("solo.txt"), "x\n").unwrap();
    Index::stage_path(&repo, "solo.txt").unwrap();
    assert_eq!(
        git(&repo, &["diff", "--cached", "--name-only"]).trim(),
        "solo.txt"
    );
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
    let _head_oid = Reference::resolve(&repo, "HEAD").unwrap();
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
    assert_eq!(status, PathChange::CLEAN);
}

#[test]
fn status_modified() {
    let (_dir, repo) = init_temp_repo();
    write_file(repo.workdir().unwrap(), "a.txt", "a\n");
    git_add(&repo, "a.txt");
    git_commit(&repo, "initial");
    write_file(repo.workdir().unwrap(), "a.txt", "modified\n");

    let status = WorkingTree::path_status(&repo, "a.txt").unwrap();
    assert_eq!(
        status,
        PathChange {
            staged: None,
            unstaged: Some(PathStatus::Modified),
            ..PathChange::CLEAN
        }
    );
}

#[test]
fn status_untracked() {
    let (_dir, repo) = init_temp_repo();
    write_file(repo.workdir().unwrap(), "untracked.txt", "new\n");

    let status = WorkingTree::path_status(&repo, "untracked.txt").unwrap();
    assert_eq!(
        status,
        PathChange {
            staged: None,
            unstaged: Some(PathStatus::Untracked),
            ..PathChange::CLEAN
        }
    );
}

#[test]
fn status_added() {
    let (_dir, repo) = init_temp_repo();
    write_file(repo.workdir().unwrap(), "new.txt", "new\n");
    git_add(&repo, "new.txt");

    let status = WorkingTree::path_status(&repo, "new.txt").unwrap();
    assert_eq!(
        status,
        PathChange {
            staged: Some(PathStatus::Added),
            unstaged: None,
            ..PathChange::CLEAN
        }
    );
}

#[test]
fn status_deleted() {
    let (_dir, repo) = init_temp_repo();
    write_file(repo.workdir().unwrap(), "a.txt", "a\n");
    git_add(&repo, "a.txt");
    git_commit(&repo, "initial");
    std::fs::remove_file(repo.workdir().unwrap().join("a.txt")).unwrap();

    let status = WorkingTree::path_status(&repo, "a.txt").unwrap();
    assert_eq!(
        status,
        PathChange {
            staged: None,
            unstaged: Some(PathStatus::Deleted),
            ..PathChange::CLEAN
        }
    );
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

    // `MM`: staged changes AND further unstaged ones. This is NOT a
    // merge conflict — it used to be reported as `PathStatus::Conflicted`
    // purely so magit's refresh would put the row in both sections.
    let status = WorkingTree::path_status(&repo, "a.txt").unwrap();
    assert_eq!(
        status,
        PathChange {
            staged: Some(PathStatus::Modified),
            unstaged: Some(PathStatus::Modified),
            ..PathChange::CLEAN
        },
        "both axes carry a modification; neither is a conflict"
    );
}

/// Reported-adjacent (2026-08-10): the parser used the default
/// porcelain form, which QUOTES and octal-escapes any path git thinks
/// needs it — and `core.quotepath` is on by default, so every
/// non-ASCII filename came back as `"\303\251t\303\251.txt"`.
/// The resulting `PathBuf` named a file that does not exist, so magit
/// showed a mangled row and acting on it would have missed.
///
/// `-z` returns paths verbatim.
#[test]
fn non_ascii_paths_are_not_quoted_or_escaped() {
    let (_dir, repo) = init_temp_repo();
    let workdir = repo.workdir().unwrap();
    write_file(workdir, "été.txt", "a\n");
    git_add(&repo, "été.txt");
    git_commit(&repo, "initial");
    write_file(workdir, "été.txt", "b\n");

    let statuses = WorkingTree::statuses(&repo).unwrap();
    let paths: Vec<String> = statuses
        .iter()
        .map(|(p, _)| p.to_string_lossy().to_string())
        .collect();
    assert!(
        paths.contains(&"été.txt".to_string()),
        "path must round-trip verbatim, got {paths:?}"
    );
}

/// A staged rename is `Renamed` (it used to be `Added`, so it rendered
/// as "new file"), and the path it came FROM survives.
///
/// With `-z` a rename is two NUL-separated records — the new path,
/// then the original — which is also why the ` -> ` form is not parsed:
/// ` -> ` is legal inside a filename.
#[test]
fn a_staged_rename_keeps_its_origin() {
    let (_dir, repo) = init_temp_repo();
    let workdir = repo.workdir().unwrap();
    write_file(workdir, "old name.txt", "a\n");
    git_add(&repo, "old name.txt");
    git_commit(&repo, "initial");
    repo.run_git_str(["mv", "old name.txt", "new name.txt"])
        .unwrap();

    let statuses = WorkingTree::statuses(&repo).unwrap();
    let (path, change) = statuses
        .iter()
        .find(|(p, _)| p.to_string_lossy() == "new name.txt")
        .expect("renamed path present");
    assert_eq!(change.staged, Some(PathStatus::Renamed));
    assert_eq!(change.unstaged, None);
    assert_eq!(
        change.original_path.as_deref(),
        Some(std::path::Path::new("old name.txt")),
        "the origin travels with the change"
    );
    assert_eq!(path.to_string_lossy(), "new name.txt");
}

/// A type change (regular file ⇄ symlink) used to decode to `None` on
/// both axes, so the path vanished from the status view — a change the
/// user could neither see nor stage.
#[cfg(unix)]
#[test]
fn a_type_change_is_reported_not_dropped() {
    let (_dir, repo) = init_temp_repo();
    let workdir = repo.workdir().unwrap();
    write_file(workdir, "target.txt", "a\n");
    write_file(workdir, "tc.txt", "a\n");
    git_add(&repo, "target.txt");
    git_add(&repo, "tc.txt");
    git_commit(&repo, "initial");
    std::fs::remove_file(workdir.join("tc.txt")).unwrap();
    std::os::unix::fs::symlink("target.txt", workdir.join("tc.txt")).unwrap();

    let statuses = WorkingTree::statuses(&repo).unwrap();
    let (_, change) = statuses
        .iter()
        .find(|(p, _)| p.to_string_lossy() == "tc.txt")
        .expect("a type change must appear in the status at all");
    assert_eq!(change.unstaged, Some(PathStatus::TypeChanged));
}

/// Unstaging a rename has to reset BOTH index entries.
///
/// A staged rename is the new path added AND the old one deleted.
/// `git reset HEAD -- <new>` alone leaves `D  old` still staged — a
/// deletion the user never asked for, which the next commit would
/// record. Verified against real git rather than reasoned about,
/// because the wrong version looks like it worked: the row does
/// disappear from Staged changes.
#[test]
fn unstaging_a_rename_leaves_nothing_staged() {
    let (_dir, repo) = init_temp_repo();
    let workdir = repo.workdir().unwrap();
    write_file(workdir, "old.txt", "hello\n");
    git_add(&repo, "old.txt");
    git_commit(&repo, "initial");
    repo.run_git_str(["mv", "old.txt", "new.txt"]).unwrap();

    let staged_rename = WorkingTree::statuses(&repo).unwrap();
    let (_, change) = staged_rename
        .iter()
        .find(|(p, _)| p.to_string_lossy() == "new.txt")
        .expect("rename staged");
    let origin = change.original_path.clone().expect("origin recorded");

    Index::unstage_paths(&repo, [std::path::Path::new("new.txt"), &origin]).unwrap();

    let after = WorkingTree::statuses(&repo).unwrap();
    let still_staged: Vec<String> = after
        .iter()
        .filter(|(_, c)| c.staged.is_some())
        .map(|(p, c)| format!("{}: {:?}", p.display(), c.staged))
        .collect();
    assert!(
        still_staged.is_empty(),
        "unstaging a rename must leave the index at HEAD, still staged: {still_staged:?}"
    );
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
    let by_path: std::collections::HashMap<String, PathChange> = statuses
        .into_iter()
        .map(|(p, s)| (p.to_string_lossy().to_string(), s))
        .collect();

    assert_eq!(
        by_path.get("tracked.txt").and_then(|c| c.unstaged),
        Some(PathStatus::Modified)
    );
    assert_eq!(by_path.get("tracked.txt").and_then(|c| c.staged), None);
    assert_eq!(
        by_path.get("new.txt").and_then(|c| c.staged),
        Some(PathStatus::Added)
    );
    assert_eq!(
        by_path.get("untracked.txt").and_then(|c| c.unstaged),
        Some(PathStatus::Untracked)
    );
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
        PathChange {
            staged: None,
            unstaged: Some(PathStatus::Modified),
            ..PathChange::CLEAN
        }
    );

    // Stage. This used to assert `Added` — the bug, pinned as intent:
    // staging a MODIFICATION does not make the file new, and magit
    // rendered the row as "new file" because of it.
    Index::stage_path(&repo, "a.txt").unwrap();
    assert_eq!(
        WorkingTree::path_status(&repo, "a.txt").unwrap(),
        PathChange {
            staged: Some(PathStatus::Modified),
            unstaged: None,
            ..PathChange::CLEAN
        },
        "a staged modification is staged + modified, not added"
    );

    // Unstage
    Index::unstage_path(&repo, "a.txt").unwrap();
    assert_eq!(
        WorkingTree::path_status(&repo, "a.txt").unwrap(),
        PathChange {
            staged: None,
            unstaged: Some(PathStatus::Modified),
            ..PathChange::CLEAN
        }
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
        PathChange::CLEAN
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

/// MG.42-E1: augment records a `squash!` marker that CARRIES a note.
///
/// The whole reason augment is not just "fixup with extra steps" is
/// that `--squash` and `-m` compose: git writes `squash! <subject>` as
/// the first line and appends the user's message below it. If they did
/// not compose, the note the user typed would be silently discarded —
/// which is why this asserts on the body, not just the subject.
#[test]
fn commit_augment_records_a_squash_marker_carrying_the_note() {
    let (_dir, repo) = init_temp_repo();
    let workdir = repo.workdir().unwrap();
    write_file(workdir, "a.txt", "a\n");
    Index::stage_path(&repo, "a.txt").unwrap();
    Commit::create(&repo, "first").unwrap();
    let target = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();

    write_file(workdir, "b.txt", "b\n");
    Index::stage_path(&repo, "b.txt").unwrap();
    Commit::augment(&repo, &target, "why this change").unwrap();

    // The subject is git's generated marker, naming the target's
    // subject — that is what makes `--autosquash` fold it in later.
    let subject = git(&repo, &["log", "-1", "--format=%s"]);
    assert_eq!(subject.trim(), "squash! first");

    // And the note survived. A `--squash` that dropped `-m` would pass
    // this test's subject assertion and lose the user's text.
    let body = git(&repo, &["log", "-1", "--format=%b"]);
    assert!(
        body.contains("why this change"),
        "the note the user wrote must reach the commit, got: {body:?}"
    );

    // Augment adds a commit; it does not rewrite the target.
    let count = git(&repo, &["rev-list", "--count", "HEAD"]);
    assert_eq!(count.trim(), "2");
}

/// MG.42-E1: merge-edit completes the merge with an authored message.
///
/// Distinct from the `n` don't-commit row, which deliberately leaves a
/// staged merge behind. If this silently behaved like `--no-commit`,
/// the user would write a message and end up with nothing committed.
#[test]
fn merge_with_message_completes_the_merge_in_one_step() {
    let (_dir, repo) = init_temp_repo();
    let workdir = repo.workdir().unwrap();
    write_file(workdir, "a.txt", "a\n");
    git_add(&repo, "a.txt");
    git_commit(&repo, "initial");

    git(&repo, &["checkout", "-b", "feature"]);
    write_file(workdir, "b.txt", "b\n");
    git_add(&repo, "b.txt");
    git_commit(&repo, "feature work");
    git(&repo, &["checkout", "main"]);

    // A second commit on main forces a real merge commit rather than a
    // fast-forward, which would take no message at all.
    write_file(workdir, "c.txt", "c\n");
    git_add(&repo, "c.txt");
    git_commit(&repo, "main work");

    Commit::merge_with_message(&repo, "feature", "merge feature into main").unwrap();

    let subject = git(&repo, &["log", "-1", "--format=%s"]);
    assert_eq!(subject.trim(), "merge feature into main");

    // Two parents: the merge actually happened and was committed.
    let parents = git(&repo, &["log", "-1", "--format=%P"]);
    assert_eq!(
        parents.split_whitespace().count(),
        2,
        "merge-edit must produce a merge commit, not a staged merge"
    );
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

/// MG.32: `m` in the branch submenu. Renaming a branch you are NOT on
/// is the ordinary case.
#[test]
fn branch_rename_a_branch_you_are_not_on() {
    let (_dir, repo) = init_temp_repo();
    write_file(repo.workdir().unwrap(), "a.txt", "a\n");
    git_add(&repo, "a.txt");
    git_commit(&repo, "initial");

    Branch::create(&repo, "old-name", false, None).unwrap();
    Branch::rename(&repo, "old-name", "new-name").unwrap();

    let branches = Branch::list(&repo).unwrap();
    assert!(!branches.contains(&"old-name".to_string()));
    assert!(branches.contains(&"new-name".to_string()));
    // Renaming another branch must not move HEAD.
    let head = git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(head.trim(), "main");
}

/// Renaming the branch you are ON is the case that must not detach
/// HEAD — git carries the checkout across, and HEAD has to follow.
#[test]
fn branch_rename_the_checked_out_branch_carries_head() {
    let (_dir, repo) = init_temp_repo();
    write_file(repo.workdir().unwrap(), "a.txt", "a\n");
    git_add(&repo, "a.txt");
    git_commit(&repo, "initial");

    Branch::create(&repo, "topic", true, None).unwrap();
    Branch::rename(&repo, "topic", "topic-renamed").unwrap();

    let head = git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(
        head.trim(),
        "topic-renamed",
        "HEAD must follow the rename, not detach"
    );
}

/// Renaming onto a name that already exists must FAIL rather than
/// clobber it. `git branch -m` (no `-M`) refuses, and the error has to
/// reach the caller — a silent overwrite here destroys a branch.
#[test]
fn branch_rename_refuses_to_clobber_an_existing_branch() {
    let (_dir, repo) = init_temp_repo();
    write_file(repo.workdir().unwrap(), "a.txt", "a\n");
    git_add(&repo, "a.txt");
    git_commit(&repo, "initial");

    Branch::create(&repo, "keep", false, None).unwrap();
    Branch::create(&repo, "other", false, None).unwrap();

    assert!(
        Branch::rename(&repo, "other", "keep").is_err(),
        "renaming onto an existing name must not silently clobber it"
    );
    let branches = Branch::list(&repo).unwrap();
    assert!(branches.contains(&"keep".to_string()));
    assert!(branches.contains(&"other".to_string()));
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
        PathChange::CLEAN
    );

    // List
    let stashes = Stash::list(&repo).unwrap();
    assert_eq!(stashes.len(), 1);
    assert!(stashes[0].message.contains("my stash"));

    // Pop
    Stash::pop(&repo, 0).unwrap();
    assert_eq!(
        WorkingTree::path_status(&repo, "a.txt").unwrap(),
        PathChange {
            staged: None,
            unstaged: Some(PathStatus::Modified),
            ..PathChange::CLEAN
        }
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
        PathChange {
            staged: None,
            unstaged: Some(PathStatus::Modified),
            ..PathChange::CLEAN
        }
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

// MG.21h — submodules. Driven against real `git submodule`, because
// the status format is the whole contract and a hand-written fixture
// would only prove the parser agrees with itself.

/// A superproject with one submodule cloned from a local repo.
fn repo_with_submodule() -> (tempfile::TempDir, tempfile::TempDir, Repository, String) {
    let (child_dir, child) = init_temp_repo();
    write_file(child.workdir().unwrap(), "lib.txt", "child\n");
    git_add(&child, "lib.txt");
    git_commit(&child, "child initial");

    let (super_dir, superproject) = init_temp_repo();
    write_file(superproject.workdir().unwrap(), "top.txt", "top\n");
    git_add(&superproject, "top.txt");
    git_commit(&superproject, "super initial");

    let url = child_dir.path().to_string_lossy().to_string();
    // Local-path submodules need this since git 2.38 (CVE-2022-39253).
    // It has to ride on the command — a repo-local `git config` is not
    // consulted for the submodule's own clone.
    let out = Command::new("git")
        .args([
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            &url,
            "vendor/child",
        ])
        .current_dir(superproject.workdir().unwrap())
        .output()
        .expect("git submodule add");
    assert!(
        out.status.success(),
        "submodule add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    git_commit(&superproject, "add submodule");

    (
        child_dir,
        super_dir,
        superproject,
        "vendor/child".to_string(),
    )
}

#[test]
fn a_repo_without_submodules_lists_none() {
    let (_dir, repo) = init_temp_repo();
    assert!(Submodule::list(&repo).expect("list").is_empty());
}

#[test]
fn a_freshly_added_submodule_lists_as_in_sync() {
    let (_child, _sup, repo, path) = repo_with_submodule();
    let listed = Submodule::list(&repo).expect("list");
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].path, path);
    assert_eq!(listed[0].state, SubmoduleState::InSync, "{listed:?}");
    assert!(
        !listed[0].sha.is_empty(),
        "the recorded commit is what `update` checks out: {listed:?}"
    );
}

/// Cross-check the marker against git's own line rather than trusting
/// our reading of it — the same discipline the bisect count needed.
#[test]
fn the_parsed_submodule_state_is_the_marker_git_printed() {
    let (_child, _sup, repo, _path) = repo_with_submodule();
    let raw = git(&repo, &["submodule", "status"]);
    let marker = raw.chars().next().expect("a status line");
    let listed = Submodule::list(&repo).expect("list");
    assert_eq!(
        listed[0].state.marker(),
        marker,
        "we read {:?} where git printed {marker:?}: {raw:?}",
        listed[0].state.marker()
    );
}

#[test]
fn sync_and_update_succeed_on_a_populated_submodule() {
    let (_child, _sup, repo, path) = repo_with_submodule();
    Submodule::sync(&repo, Some(&path)).expect("sync");
    // Already populated by `add`, so `update` needs no clone and no
    // file-protocol exemption.
    Submodule::update(&repo, Some(&path)).expect("update");
    assert_eq!(Submodule::list(&repo).expect("list").len(), 1);
}

#[test]
fn removing_a_submodule_deinits_and_drops_it() {
    let (_child, _sup, repo, path) = repo_with_submodule();
    Submodule::remove(&repo, &path).expect("remove");
    assert!(
        Submodule::list(&repo).expect("list").is_empty(),
        "the submodule is gone from git's own status"
    );
    assert!(
        !repo.workdir().unwrap().join(&path).exists(),
        "and its working tree with it"
    );
}

#[test]
fn removing_an_unknown_submodule_is_an_error_not_a_silent_no_op() {
    let (_dir, repo) = init_temp_repo();
    let err = Submodule::remove(&repo, "vendor/nope").expect_err("no such submodule");
    assert!(
        format!("{err}").contains("vendor/nope"),
        "the error names the path: {err}"
    );
}

// ── MG.35: `Reference::list` against a real repository ───────────────

/// The parser is unit-tested against synthetic output; this pins the
/// *format string* — the half a unit test can never reach. A typo in
/// `--format` yields fields in the wrong order or missing entirely, and
/// the parser would happily produce plausible nonsense from it.
#[test]
fn listing_refs_reports_branches_remotes_and_tags_with_their_targets() {
    let (dir, repo) = init_temp_repo();
    write_file(dir.path(), "a.txt", "one\n");
    Index::stage_path(&repo, "a.txt").expect("stage");
    Commit::create(&repo, "first").expect("commit");
    git(&repo, &["tag", "v1.0.0"]);
    git(&repo, &["branch", "feature/x"]);

    let refs = lattice_vcs::Reference::list(&repo).expect("list refs");

    let head = refs
        .iter()
        .find(|r| r.head)
        .expect("exactly one ref is the checked-out branch");
    assert_eq!(head.kind, lattice_vcs::RefKind::Branch);
    assert!(!head.short_id.is_empty(), "the object id came through");

    let feature = refs
        .iter()
        .find(|r| r.name == "feature/x")
        .expect("the second branch is listed");
    assert_eq!(feature.kind, lattice_vcs::RefKind::Branch);
    assert!(!feature.head, "only the checked-out branch is marked");

    let tag = refs
        .iter()
        .find(|r| r.name == "v1.0.0")
        .expect("the tag is listed");
    assert_eq!(tag.kind, lattice_vcs::RefKind::Tag);
    assert_eq!(
        tag.short_id, head.short_id,
        "the tag points at the same commit HEAD does"
    );
    assert_eq!(
        tag.subject, "first",
        "the subject field is in the right slot"
    );
}

/// A branch ahead of its upstream reports so. This is the field the
/// refs buffer exists to show, and it is also the one most likely to
/// land in the wrong slot if the format string drifts.
#[test]
fn a_branch_ahead_of_its_upstream_reports_the_count() {
    let (dir, repo) = init_temp_repo();
    write_file(dir.path(), "a.txt", "one\n");
    Index::stage_path(&repo, "a.txt").expect("stage");
    Commit::create(&repo, "first").expect("commit");

    // A local "remote" is enough: `for-each-ref` reads the tracking
    // config, not the network.
    git(&repo, &["branch", "upstream-of-main"]);
    git(&repo, &["branch", "--set-upstream-to=upstream-of-main"]);
    write_file(dir.path(), "a.txt", "two\n");
    Index::stage_path(&repo, "a.txt").expect("stage");
    Commit::create(&repo, "second").expect("commit");

    let refs = lattice_vcs::Reference::list(&repo).expect("list refs");
    let head = refs.iter().find(|r| r.head).expect("checked-out branch");
    assert_eq!(head.upstream, "upstream-of-main");
    assert_eq!(
        head.track, "ahead 1",
        "one commit ahead, with git's brackets already stripped"
    );
}

// ── MG.37: git notes against a real repository ───────────────────────

/// The round trip, and the case that matters most: a SECOND edit.
///
/// `git notes add` without `--force` refuses when a note already
/// exists, so an implementation that omitted it would work exactly once
/// per commit and then silently stop saving.
#[test]
fn a_note_can_be_written_read_back_and_rewritten() {
    let (dir, repo) = init_temp_repo();
    write_file(dir.path(), "a.txt", "one\n");
    Index::stage_path(&repo, "a.txt").expect("stage");
    Commit::create(&repo, "first").expect("commit");
    let head = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();

    assert_eq!(
        lattice_vcs::Note::show(&repo, &head),
        None,
        "no note yet — and that is `None`, not an error"
    );

    lattice_vcs::Note::set(&repo, &head, "first note\n").expect("set");
    assert_eq!(
        lattice_vcs::Note::show(&repo, &head)
            .as_deref()
            .map(str::trim),
        Some("first note")
    );

    lattice_vcs::Note::set(&repo, &head, "rewritten\n").expect("rewrite");
    assert_eq!(
        lattice_vcs::Note::show(&repo, &head)
            .as_deref()
            .map(str::trim),
        Some("rewritten"),
        "a second edit must overwrite, not be refused"
    );
}

/// Clearing the buffer means "no note". `git notes add -F` errors on
/// empty input, so this has to translate to a remove — and removing a
/// note that was never there must not fail either, or clearing an empty
/// buffer would error.
#[test]
fn saving_an_empty_note_removes_it_and_is_safe_when_there_was_none() {
    let (dir, repo) = init_temp_repo();
    write_file(dir.path(), "a.txt", "one\n");
    Index::stage_path(&repo, "a.txt").expect("stage");
    Commit::create(&repo, "first").expect("commit");
    let head = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();

    lattice_vcs::Note::set(&repo, &head, "temporary\n").expect("set");
    assert!(lattice_vcs::Note::show(&repo, &head).is_some());

    lattice_vcs::Note::set(&repo, &head, "   \n").expect("blank clears it");
    assert_eq!(lattice_vcs::Note::show(&repo, &head), None);

    lattice_vcs::Note::set(&repo, &head, "").expect("clearing an absent note is not an error");
    lattice_vcs::Note::remove(&repo, &head).expect("explicit remove is idempotent too");
}

/// `git show` displays notes by default, which is why the revision view
/// needs no work to show them — pinned so a future `--no-notes` or a
/// format change does not silently remove the only place they surface.
#[test]
fn a_note_appears_in_git_show_output() {
    let (dir, repo) = init_temp_repo();
    write_file(dir.path(), "a.txt", "one\n");
    Index::stage_path(&repo, "a.txt").expect("stage");
    Commit::create(&repo, "first").expect("commit");
    let head = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    lattice_vcs::Note::set(&repo, &head, "visible in the revision view\n").expect("set");

    let shown = git(&repo, &["show", "--stat", "-p", &head]);
    assert!(
        shown.contains("visible in the revision view"),
        "the revision view shows notes for free: {shown}"
    );
}

/// Prune drops notes whose object is gone. `--dry-run` must report
/// without removing — it is the only way to review the operation.
#[test]
fn prune_dry_run_reports_without_removing() {
    let (dir, repo) = init_temp_repo();
    write_file(dir.path(), "a.txt", "one\n");
    Index::stage_path(&repo, "a.txt").expect("stage");
    Commit::create(&repo, "first").expect("commit");
    let head = git(&repo, &["rev-parse", "HEAD"]).trim().to_string();
    lattice_vcs::Note::set(&repo, &head, "kept\n").expect("set");

    lattice_vcs::Note::prune(&repo, true).expect("dry run");
    assert_eq!(
        lattice_vcs::Note::show(&repo, &head)
            .as_deref()
            .map(str::trim),
        Some("kept"),
        "a dry run must not remove a reachable commit's note"
    );
}
