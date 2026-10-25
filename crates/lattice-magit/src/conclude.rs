//! CC.1: finish a commit git stopped short of making.
//!
//! Git stops before a merge, cherry-pick, revert or rebase pick is
//! committed — on purpose (`--no-commit`) or on a conflict — and leaves
//! the operation in flight with its proposed message in `MERGE_MSG`. The
//! commit buffer seeds from that message; this is what its confirm runs.
//!
//! Two paths, because git takes the message two ways and neither works
//! for every operation (`magit-concluding-commits.md` §2.1, each verified
//! against git):
//!
//! - **merge / cherry-pick / revert:** `git commit --cleanup=strip -F -`.
//!   `cherry-pick --continue` and `revert --continue` do not run
//!   `GIT_EDITOR` off a terminal, so they cannot be handed a message; a
//!   commit can, and keeps the picked commit's author. A sequence then
//!   carries on with `--continue`.
//! - **rebase:** `git rebase --continue` with `GIT_EDITOR` as `cp <msg>`.
//!   A rebase stop leaves no `CHERRY_PICK_HEAD`, so a bare commit would
//!   record the user as author; `--continue` applies the stop's
//!   `author-script`.
//!
//! `--cleanup=strip` either way: a conflicted merge's message carries
//! `# Conflicts:` lines, which the editor session git would have opened
//! strips.

use std::path::Path;

use lattice_vcs::InFlightOp;

/// Conclude `op` with `message`. The report is what `finish_task` shows:
/// a sequence that stops again on its next commit reports that stop.
pub(crate) fn conclude(workdir: &Path, op: InFlightOp, message: &str) -> Result<String, String> {
    let argv = |args: &[&str]| args.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match op {
        InFlightOp::Merge | InFlightOp::CherryPick | InFlightOp::Revert => {
            let committed = crate::magit_global_mode::run_git_op(
                workdir,
                &argv(&["commit", "--cleanup=strip", "-F", "-"]),
                "true",
                Some(message.as_bytes().to_vec()),
            )?;
            let sequence = match op {
                InFlightOp::CherryPick => Some("cherry-pick"),
                InFlightOp::Revert => Some("revert"),
                _ => None,
            };
            // Only a sequence of several commits leaves `sequencer/` once
            // the stopped one is committed; one commit leaves nothing to
            // continue, and `--continue` would fail saying so.
            match sequence {
                Some(verb) if sequencer_remains(workdir) => crate::magit_global_mode::run_git_op(
                    workdir,
                    &argv(&[verb, "--continue"]),
                    "true",
                    None,
                ),
                _ => Ok(committed),
            }
        }
        InFlightOp::Rebase => {
            static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("lattice-conclude-msg-{}-{seq}", std::process::id()));
            std::fs::write(&path, message).map_err(|e| e.to_string())?;
            let editor = format!("cp {}", crate::magit_rebase_mode::sh_quoted_path(&path));
            let result = crate::magit_global_mode::run_git_op(
                workdir,
                &argv(&["rebase", "--continue"]),
                &editor,
                None,
            );
            let _ = std::fs::remove_file(&path);
            result
        }
        // `PendingCommit` never reports one: `am --continue` commits the
        // patch's own message, so there is nothing to compose.
        InFlightOp::ApplyPatch => Err("a stopped `git am` has no message to conclude".to_string()),
    }
}

/// Is a cherry-pick / revert sequence still in flight?
fn sequencer_remains(workdir: &Path) -> bool {
    lattice_vcs::Repository::discover(workdir)
        .map(|repo| repo.gitdir().join("sequencer").exists())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_vcs::PendingCommit;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .env("GIT_EDITOR", "true")
            .current_dir(dir)
            .output()
            .expect("git runs");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().expect("tempdir");
        let p = d.path();
        git(p, &["init", "-q", "-b", "main"]);
        git(p, &["config", "user.name", "Me"]);
        git(p, &["config", "user.email", "me@example.com"]);
        git(p, &["config", "commit.gpgsign", "false"]);
        std::fs::write(p.join("f"), "0\n").unwrap();
        git(p, &["add", "f"]);
        git(p, &["commit", "-q", "-m", "base"]);
        d
    }

    fn commit_as(dir: &Path, author: &str, file: &str, text: &str, subject: &str) {
        std::fs::write(dir.join(file), text).unwrap();
        git(dir, &["add", file]);
        git(
            dir,
            &[
                "-c",
                &format!("user.name={author}"),
                "commit",
                "-q",
                "-m",
                subject,
            ],
        );
    }

    fn pending(dir: &Path) -> PendingCommit {
        PendingCommit::detect(&lattice_vcs::Repository::discover(dir).unwrap())
            .expect("git stopped with a commit pending")
    }

    /// A conflicted merge: the composed message lands, the `# Conflicts:`
    /// lines git proposed do not, and the commit has both parents.
    #[test]
    fn a_stopped_merge_concludes_with_the_message_and_both_parents() {
        let d = repo();
        let p = d.path();
        git(p, &["checkout", "-q", "-b", "side"]);
        commit_as(p, "Me", "f", "side\n", "on side");
        git(p, &["checkout", "-q", "main"]);
        commit_as(p, "Me", "f", "main\n", "on main");
        git(p, &["merge", "side"]);
        let stopped = pending(p);
        assert_eq!(stopped.op, InFlightOp::Merge);
        assert!(
            stopped.message.contains("# Conflicts:"),
            "{:?}",
            stopped.message
        );

        std::fs::write(p.join("f"), "resolved\n").unwrap();
        git(p, &["add", "f"]);
        // The user rewrites git's subject and leaves its comment lines.
        let message = stopped
            .message
            .replacen("Merge branch 'side'", "Merge side, resolved", 1);
        conclude(p, InFlightOp::Merge, &message).expect("concluded");

        assert_eq!(
            git(p, &["log", "-1", "--format=%B"]),
            "Merge side, resolved"
        );
        assert_eq!(
            git(p, &["rev-list", "--parents", "-n1", "HEAD"])
                .split_whitespace()
                .count(),
            3,
            "a merge commit: itself and two parents"
        );
        assert!(PendingCommit::detect(&lattice_vcs::Repository::discover(p).unwrap()).is_none());
    }

    /// A two-commit cherry-pick that stops on the first: concluding it keeps
    /// the picked commit's author and carries the sequence on.
    #[test]
    fn a_stopped_pick_keeps_its_author_and_the_sequence_carries_on() {
        let d = repo();
        let p = d.path();
        git(p, &["checkout", "-q", "-b", "src"]);
        commit_as(p, "Alice", "f", "1\n", "alice one");
        commit_as(p, "Alice", "g", "2\n", "alice two");
        git(p, &["checkout", "-q", "main"]);
        commit_as(p, "Me", "f", "X\n", "main X");
        git(p, &["cherry-pick", "main..src"]);
        assert_eq!(pending(p).op, InFlightOp::CherryPick);

        std::fs::write(p.join("f"), "R\n").unwrap();
        git(p, &["add", "f"]);
        conclude(p, InFlightOp::CherryPick, "alice one, reconciled\n").expect("concluded");

        assert_eq!(
            git(p, &["log", "-2", "--format=%an|%s"]),
            "Alice|alice two\nAlice|alice one, reconciled"
        );
        assert!(!p.join(".git/sequencer").exists(), "the sequence finished");
    }

    /// A rebase conflict stop: concluded through `rebase --continue`, so the
    /// stop's own author is kept, with the composed message.
    #[test]
    fn a_rebase_stop_concludes_with_the_original_author() {
        let d = repo();
        let p = d.path();
        git(p, &["checkout", "-q", "-b", "side"]);
        commit_as(p, "Alice", "f", "side\n", "alice side");
        git(p, &["checkout", "-q", "main"]);
        commit_as(p, "Me", "f", "main\n", "on main");
        git(p, &["checkout", "-q", "side"]);
        git(p, &["rebase", "main"]);
        assert_eq!(pending(p).op, InFlightOp::Rebase);

        std::fs::write(p.join("f"), "both\n").unwrap();
        git(p, &["add", "f"]);
        conclude(p, InFlightOp::Rebase, "# git's note\nalice side, rebased\n").expect("concluded");

        assert_eq!(
            git(p, &["log", "-1", "--format=%an|%B"]),
            "Alice|alice side, rebased"
        );
        assert!(!p.join(".git/rebase-merge").exists(), "the rebase finished");
    }
}
