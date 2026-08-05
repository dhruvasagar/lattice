//! MG.43d: magit's commit-moving operations — cherry-pick `h` / `d` /
//! `n` / `s` and branch `s` / `S`.
//!
//! Ported from magit's own `magit--cherry-move` and
//! `magit--branch-spinoff` (lisp/magit-sequence.el, lisp/magit-branch.el)
//! rather than reconstructed from their docstrings: these move and
//! delete commits, and the two halves of each pair differ only in
//! where you end up, which is exactly the kind of detail a paraphrase
//! loses.
//!
//! Every one of these is a SEQUENCE whose later steps depend on state
//! only discoverable part-way through (does the target branch exist,
//! are the commits at the tip of their branch, is there an upstream).
//! They therefore run as one closure inside `spawn_blocking` rather
//! than as a precomputed `Vec<GitStep>` — computing the steps up front
//! would mean reading git on the actor thread.

use std::path::Path;

/// Run one git command, returning its combined output.
fn git(workdir: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .current_dir(workdir)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn git_ok(workdir: &Path, args: &[&str]) -> bool {
    git(workdir, args).is_ok()
}

fn branch_exists(workdir: &Path, branch: &str) -> bool {
    git_ok(
        workdir,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
}

pub(crate) fn current_branch_of(workdir: &Path) -> Option<String> {
    current_branch(workdir)
}

fn current_branch(workdir: &Path) -> Option<String> {
    let b = git(workdir, &["rev-parse", "--abbrev-ref", "HEAD"]).ok()?;
    (b != "HEAD" && !b.is_empty()).then_some(b)
}

fn upstream_of(workdir: &Path, branch: &str) -> Option<String> {
    git(
        workdir,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            &format!("{branch}@{{upstream}}"),
        ],
    )
    .ok()
    .filter(|s| !s.is_empty())
}

fn rev(workdir: &Path, spec: &str) -> Option<String> {
    git(workdir, &["rev-parse", spec])
        .ok()
        .filter(|s| !s.is_empty())
}

/// A revision that would be read as an option is refused rather than
/// passed to git — the same guard `rebase_one_commit` carries.
fn reject_option_like(value: &str, what: &str) -> Result<(), String> {
    if value.starts_with('-') {
        return Err(format!("`{value}` is not a valid {what}"));
    }
    if value.is_empty() {
        return Err(format!("no {what} given"));
    }
    Ok(())
}

/// MG.43d: where a spun-out branch starts.
///
/// Magit reads this from `magit-get-upstream-branch` — the UPSTREAM,
/// not the current branch. That is the point: the new branch must
/// start somewhere the commit does NOT already exist, or the
/// cherry-pick onto it is empty and git stops with "The previous
/// cherry-pick is now empty".
///
/// With no upstream there is nothing to default to, so it falls back
/// to the commit's own parent — the nearest point that is guaranteed
/// not to contain it.
pub(crate) fn spin_start_point(workdir: &Path, commit: &str) -> String {
    current_branch(workdir)
        .and_then(|c| upstream_of(workdir, &c))
        .unwrap_or_else(|| format!("{commit}^"))
}

/// MG.43d: magit's `magit--cherry-move`, for a single commit.
///
/// Copies `commit` onto `dst`, then removes it from `src` when there
/// is one. Lattice resolves one commit at a time (cursor or picker),
/// which collapses magit's list handling but changes none of the
/// semantics.
///
/// - `src` — the branch the commit is REMOVED from. `None` is
///   magit's "harvest only" case: copy, remove nothing.
/// - `dst` — the branch it lands on. Created at `start_point` when it
///   does not exist.
/// - `checkout_dst` — stay on `dst` at the end, rather than returning
///   to `src`. This is the ONLY difference between spinout and
///   spinoff, and between donate and harvest.
pub(crate) fn cherry_move(
    workdir: &Path,
    commit: &str,
    src: Option<&str>,
    dst: &str,
    start_point: Option<&str>,
    checkout_dst: bool,
) -> Result<(), String> {
    reject_option_like(commit, "commit")?;
    reject_option_like(dst, "branch")?;

    let current = current_branch(workdir);

    // Create the destination if it does not exist, tracking the same
    // upstream its start point does — magit sets this so a spun-out
    // branch is not left with no upstream.
    if !branch_exists(workdir, dst) {
        match start_point {
            Some(sp) => {
                reject_option_like(sp, "starting point")?;
                git(workdir, &["branch", dst, sp])?;
            }
            None => {
                git(workdir, &["branch", dst])?;
            }
        }
        if let Some(sp) = start_point
            && let Some(up) = upstream_of(workdir, sp)
        {
            // Best-effort: a start point with no upstream is normal.
            let _ = git(workdir, &["branch", "--set-upstream-to", &up, dst]);
        }
    }

    if current.as_deref() != Some(dst) {
        git(workdir, &["checkout", dst])?;
    }

    // `keep` is the commit BEFORE the one being moved — where `src`
    // must end up once it is removed.
    let keep = format!("{commit}^");
    git(workdir, &["cherry-pick", commit])?;

    let Some(src) = src else {
        // Harvest-only: nothing to remove.
        return Ok(());
    };
    reject_option_like(src, "source branch")?;

    let tip = rev(workdir, src);
    let moved = rev(workdir, commit);

    if tip.is_some() && tip == moved {
        // The commit was at `src`'s tip, so `src` just moves back one.
        //
        // The THREE-argument `update-ref` is a compare-and-swap: it
        // names the value `src` must currently hold, and fails if it
        // does not. Without it a concurrent change to `src` between
        // the read above and this write would be silently discarded —
        // and this is the branch that loses commits.
        let keep_sha = rev(workdir, &keep)
            .ok_or_else(|| format!("{commit} has no parent to reset {src} to"))?;
        let tip_sha = tip.expect("checked above");
        git(
            workdir,
            &[
                "update-ref",
                "-m",
                &format!("reset: moving to {keep}"),
                &format!("refs/heads/{src}"),
                &keep_sha,
                &tip_sha,
            ],
        )?;
        if !checkout_dst {
            git(workdir, &["checkout", src])?;
        }
    } else {
        // The commit is in the middle of `src`'s history, so it has to
        // be rebased out rather than pointed past.
        git(workdir, &["checkout", src])?;
        crate::magit_rebase_mode::rebase_one_commit(workdir, commit, "drop", None)?;
        if checkout_dst {
            git(workdir, &["checkout", dst])?;
        }
    }
    Ok(())
}

/// MG.43d: magit's `magit--branch-spinoff`.
///
/// Creates `branch` from the current branch's UNPUSHED commits, then
/// rewinds the current branch to where it last agreed with its
/// upstream.
///
/// `checkout` is the whole difference between magit's `s` spin-off
/// (end up on the new branch) and `S` spin-out (stay put).
pub(crate) fn branch_spinoff(workdir: &Path, branch: &str, checkout: bool) -> Result<(), String> {
    reject_option_like(branch, "branch name")?;
    if branch_exists(workdir, branch) {
        return Err(format!("cannot spin off {branch}: it already exists"));
    }
    let current = current_branch(workdir).ok_or_else(|| "not on a branch".to_string())?;

    // magit: spin-OUT promotes itself to spin-off when the tree is
    // dirty. Staying on a branch that is about to be hard-reset would
    // destroy the uncommitted work; moving to the new branch keeps it.
    let dirty = !git(workdir, &["status", "--porcelain"])
        .unwrap_or_default()
        .is_empty();
    let checkout = checkout || dirty;

    if checkout {
        git(workdir, &["checkout", "-b", branch, &current])?;
    } else {
        git(workdir, &["branch", branch, &current])?;
    }
    if let Some(up) = upstream_of(workdir, &current) {
        let _ = git(workdir, &["branch", "--set-upstream-to", &up, branch]);
    }

    // With no upstream, or nothing unpushed, the new branch is created
    // and the old one is LEFT ALONE — magit's documented behaviour,
    // and the case where rewinding would discard commits that were
    // never anywhere else.
    let Some(tracked) = upstream_of(workdir, &current) else {
        return Ok(());
    };
    let Some(base) = git(workdir, &["merge-base", &current, &tracked])
        .ok()
        .filter(|s| !s.is_empty())
    else {
        return Ok(());
    };
    if rev(workdir, &current) == Some(base.clone()) {
        return Ok(());
    }

    if checkout {
        // We are on the new branch now, so the old one is moved by ref
        // rather than by reset.
        git(
            workdir,
            &[
                "update-ref",
                "-m",
                &format!("reset: moving to {base}"),
                &format!("refs/heads/{current}"),
                &base,
            ],
        )?;
    } else {
        git(workdir, &["reset", "--hard", &base])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) -> String {
        git(dir, args).unwrap_or_else(|e| panic!("git {args:?} failed: {e}"))
    }

    /// A repo on `main` with `feature` carrying one extra commit.
    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().expect("tempdir");
        let p = d.path();
        run(p, &["init", "-b", "main"]);
        run(p, &["config", "user.email", "t@lattice.dev"]);
        run(p, &["config", "user.name", "lattice-test"]);
        std::fs::write(p.join("base.txt"), "base\n").expect("write");
        run(p, &["add", "."]);
        run(p, &["commit", "-m", "base"]);
        d
    }

    fn commit_file(p: &Path, name: &str) -> String {
        std::fs::write(p.join(name), format!("{name}\n")).expect("write");
        run(p, &["add", name]);
        run(p, &["commit", "-m", name]);
        run(p, &["rev-parse", "HEAD"])
    }

    fn subjects(p: &Path, branch: &str) -> String {
        run(p, &["log", "--format=%s", branch])
    }

    /// MG.43d: **donate moves the commit and leaves you where you
    /// were.**
    ///
    /// The commit must be ON the target and GONE from the source —
    /// "move", not "copy". A copy is what `A` cherry-pick already
    /// does, so a donate that forgot to remove would silently be the
    /// wrong row.
    #[test]
    fn donate_moves_the_commit_and_stays_on_the_current_branch() {
        let d = repo();
        let p = d.path();
        run(p, &["branch", "target"]);
        let sha = commit_file(p, "work.txt");

        cherry_move(p, &sha, Some("main"), "target", None, false).expect("donate");

        assert_eq!(
            run(p, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "main",
            "donate stays on the branch you started from",
        );
        assert!(
            subjects(p, "target").contains("work.txt"),
            "the commit must land on the target: {}",
            subjects(p, "target"),
        );
        assert!(
            !subjects(p, "main").contains("work.txt"),
            "and be REMOVED from the source — otherwise it is a copy: {}",
            subjects(p, "main"),
        );
    }

    /// Harvest is donate's mirror: it pulls a commit here and removes
    /// it there, leaving you on the branch you were already on.
    #[test]
    fn harvest_moves_the_commit_here_and_removes_it_there() {
        let d = repo();
        let p = d.path();
        run(p, &["checkout", "-b", "feature"]);
        let sha = commit_file(p, "work.txt");
        run(p, &["checkout", "main"]);

        cherry_move(p, &sha, Some("feature"), "main", None, true).expect("harvest");

        assert_eq!(run(p, &["rev-parse", "--abbrev-ref", "HEAD"]), "main");
        assert!(subjects(p, "main").contains("work.txt"), "landed here");
        assert!(
            !subjects(p, "feature").contains("work.txt"),
            "removed there: {}",
            subjects(p, "feature"),
        );
    }

    /// MG.43d: **spinout stays put, spinoff checks out.** That is the
    /// only difference between the two rows, and the one thing a user
    /// notices immediately if it is backwards.
    #[test]
    fn spinout_stays_and_spinoff_checks_out() {
        for (checkout_dst, expected) in [(false, "main"), (true, "spun")] {
            let d = repo();
            let p = d.path();
            let sha = commit_file(p, "work.txt");

            let start = spin_start_point(p, &sha);
            cherry_move(p, &sha, Some("main"), "spun", Some(&start), checkout_dst).expect("spin");

            assert_eq!(
                run(p, &["rev-parse", "--abbrev-ref", "HEAD"]),
                expected,
                "checkout_dst={checkout_dst} must end on `{expected}`",
            );
            assert!(subjects(p, "spun").contains("work.txt"));
            assert!(!subjects(p, "main").contains("work.txt"));
        }
    }

    /// A commit in the MIDDLE of the source is rebased out, not
    /// pointed past. `update-ref` only works when the commit is the
    /// tip; using it otherwise would drop every later commit with it.
    #[test]
    fn a_commit_below_the_tip_is_rebased_out_keeping_later_commits() {
        let d = repo();
        let p = d.path();
        run(p, &["branch", "target"]);
        let middle = commit_file(p, "middle.txt");
        commit_file(p, "later.txt");

        cherry_move(p, &middle, Some("main"), "target", None, false).expect("donate");

        let main = subjects(p, "main");
        assert!(
            !main.contains("middle.txt"),
            "the moved commit is gone: {main}"
        );
        assert!(
            main.contains("later.txt"),
            "but the commits after it survive: {main}",
        );
    }

    /// MG.43d: **spin-off rewinds the old branch to its upstream, and
    /// spin-out leaves you on it.**
    #[test]
    fn spinoff_moves_unpushed_commits_off_the_current_branch() {
        let d = repo();
        let p = d.path();
        // A "remote" to be upstream of main.
        run(p, &["checkout", "-b", "upstream"]);
        run(p, &["checkout", "main"]);
        run(p, &["branch", "--set-upstream-to", "upstream", "main"]);
        commit_file(p, "unpushed.txt");

        branch_spinoff(p, "feature", true).expect("spinoff");

        assert_eq!(
            run(p, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "feature",
            "spin-off checks the new branch out",
        );
        assert!(subjects(p, "feature").contains("unpushed.txt"));
        assert!(
            !subjects(p, "main").contains("unpushed.txt"),
            "main is rewound to its upstream: {}",
            subjects(p, "main"),
        );
    }

    /// With NO upstream the old branch is left alone — rewinding it
    /// would discard commits that exist nowhere else.
    #[test]
    fn spinoff_without_an_upstream_leaves_the_old_branch_alone() {
        let d = repo();
        let p = d.path();
        commit_file(p, "work.txt");

        branch_spinoff(p, "feature", true).expect("spinoff");

        assert!(subjects(p, "feature").contains("work.txt"));
        assert!(
            subjects(p, "main").contains("work.txt"),
            "with no upstream there is no safe base to rewind to: {}",
            subjects(p, "main"),
        );
    }

    /// An existing name is refused rather than silently reusing the
    /// branch, which would move commits onto something already in use.
    #[test]
    fn spinoff_refuses_an_existing_branch_name() {
        let d = repo();
        let p = d.path();
        run(p, &["branch", "taken"]);
        assert!(branch_spinoff(p, "taken", true).is_err());
    }

    /// MG.43d: **spin-OUT promotes itself to spin-off when the tree is
    /// dirty**, which is magit's own behaviour.
    ///
    /// Staying on a branch that is about to be `reset --hard` would
    /// destroy the uncommitted work; moving to the new branch keeps
    /// it. This is the one case where the two rows deliberately behave
    /// the same.
    #[test]
    fn spinout_moves_to_the_new_branch_rather_than_hard_resetting_over_dirt() {
        let d = repo();
        let p = d.path();
        run(p, &["checkout", "-b", "upstream"]);
        run(p, &["checkout", "main"]);
        run(p, &["branch", "--set-upstream-to", "upstream", "main"]);
        commit_file(p, "unpushed.txt");
        std::fs::write(p.join("dirty.txt"), "uncommitted\n").expect("write");
        run(p, &["add", "dirty.txt"]);

        branch_spinoff(p, "feature", false).expect("spinout");

        assert_eq!(
            run(p, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "feature",
            "a dirty tree promotes spin-out to spin-off",
        );
        assert!(
            p.join("dirty.txt").exists(),
            "the uncommitted work must survive",
        );
    }

    /// Option-looking values are refused rather than handed to git.
    #[test]
    fn option_looking_arguments_are_refused() {
        let d = repo();
        let p = d.path();
        assert!(cherry_move(p, "--help", None, "x", None, false).is_err());
        assert!(cherry_move(p, "abc", None, "--help", None, false).is_err());
        assert!(branch_spinoff(p, "--help", true).is_err());
    }
}
