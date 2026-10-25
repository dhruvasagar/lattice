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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lattice_grammar::Effect;
use lattice_mode::SubsystemBoot;
use lattice_mode::inbound::InboundBus;
use lattice_vcs::InFlightOp;

/// CC.2: a finished git operation asking for `workdir`'s commit buffer.
pub struct ComposeRequest {
    pub workdir: PathBuf,
}

/// Service alias — register and look up through this exact type
/// (`feedback_servicesregistry_arc_typeid`).
///
/// A service the trigger looks up and hands its spawner, not a static
/// captured at install as `EVENT_BUS` is. A `OnceLock` keeps the FIRST
/// editor's bus, so with two editors in one process — every test binary
/// — a pull in the second would open its commit buffer in the first.
pub type ComposeBusHandle = Arc<InboundBus<ComposeRequest>>;

/// Install the bus a stopped operation asks for its commit buffer on.
///
/// **`inbound`, not `tick_callback`:** the wake is baked into `send`, so
/// the buffer opens the moment git stops, not on the user's next key
/// (`boot-composition.md` §3). The handler is the whole mapping — the
/// repository's commit buffer, named for it, in `magit-commit-mode`,
/// whose activation finds the pending commit and seeds from it (CC.1).
pub(crate) fn install_compose_bus(
    boot: &mut impl SubsystemBoot,
    scopes: crate::repo_scope::RepoScopesHandle,
) -> ComposeBusHandle {
    let bus = boot.inbound::<ComposeRequest, _>(move |req| {
        vec![Effect::OpenSyntheticBuffer {
            name: crate::repo_scope::repo_view_name_for("commit", &scopes, &req.workdir),
            mode_id: crate::magit_commit_mode::MagitCommitMode::mode_id()
                .as_str()
                .to_string(),
            content: None,
            cursor: None,
            activate_minor: None,
        }]
    });
    let bus: ComposeBusHandle = Arc::new(bus);
    boot.register_service::<ComposeBusHandle>(bus.clone());
    bus
}

/// CC.2: after an operation that can stop short of a commit, open the
/// commit buffer if it did — and there is nothing left to resolve.
/// A conflicted stop is the user's to resolve first; the status buffer
/// shows it, and `c c` composes once it is staged.
///
/// Blocking (two small git reads): call it on the blocking pool, where
/// the operation itself ran.
pub(crate) fn compose_if_pending(workdir: &Path, bus: &ComposeBusHandle) {
    // The repository's root, not `workdir` as given: the buffer is named
    // for the repository, and a subdirectory would name another one.
    let Some(root) = should_compose(workdir) else {
        return;
    };
    // A failed send means the editor is going away; the stop is still
    // in the repository, and `c c` composes from it.
    let _ = bus.send(ComposeRequest { workdir: root });
}

/// Is git waiting on a commit, with nothing left to resolve? Then the
/// repository's root, to open its commit buffer for.
fn should_compose(workdir: &Path) -> Option<PathBuf> {
    let repo = lattice_vcs::Repository::discover(workdir).ok()?;
    lattice_vcs::PendingCommit::detect(&repo)?;
    // A failed read is treated as unresolved: opening a commit buffer the
    // commit then refuses is worse than leaving it to `c c`.
    let resolved = repo
        .run_git_str(["ls-files", "--unmerged"])
        .is_ok_and(|out| out.trim().is_empty());
    resolved.then(|| repo.workdir().unwrap_or(workdir).to_path_buf())
}

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

    /// CC.2: pull's argv leaves the choice to git's config, and
    /// `--no-commit` changes only the merge case. Each config is run for
    /// real against a diverged clone, through the argv magit sends.
    #[test]
    fn pull_does_what_the_config_says() {
        let pull = crate::magit_global_mode::RemoteOp::PULL.argv(&lattice_grammar::Args::None);
        let diverged = |config: &[(&str, &str)]| {
            let upstream = repo();
            let root = tempfile::tempdir().expect("tempdir");
            let work = root.path().join("work");
            git(
                root.path(),
                &["clone", "-q", &upstream.path().to_string_lossy(), "work"],
            );
            git(&work, &["config", "user.name", "Me"]);
            git(&work, &["config", "user.email", "me@example.com"]);
            git(&work, &["config", "commit.gpgsign", "false"]);
            for (k, v) in config {
                git(&work, &["config", k, v]);
            }
            commit_as(upstream.path(), "Me", "g", "g\n", "upstream g");
            commit_as(&work, "Me", "h", "h\n", "local h");
            let result = crate::magit_global_mode::run_git_op(&work, &pull, "true", None);
            (upstream, root, work, result)
        };

        // Merge: stops before the commit, which is what opens the buffer.
        let (_u, _r, work, result) = diverged(&[("pull.rebase", "false")]);
        assert!(result.is_ok(), "{result:?}");
        assert!(work.join(".git/MERGE_HEAD").exists());
        assert!(should_compose(&work).is_some());

        // Rebase: `--no-commit` is inert; the commits replay, no message.
        let (_u, _r, work, result) = diverged(&[("pull.rebase", "true")]);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            git(&work, &["log", "-2", "--format=%s"]),
            "local h\nupstream g",
            "local replayed on upstream"
        );
        assert!(should_compose(&work).is_none());

        // Nothing local to merge: a fast-forward, no commit to write.
        let upstream = repo();
        let root = tempfile::tempdir().expect("tempdir");
        let work = root.path().join("work");
        git(
            root.path(),
            &["clone", "-q", &upstream.path().to_string_lossy(), "work"],
        );
        git(&work, &["config", "pull.rebase", "false"]);
        commit_as(upstream.path(), "Me", "g", "g\n", "upstream g");
        let result = crate::magit_global_mode::run_git_op(&work, &pull, "true", None);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(git(&work, &["log", "-1", "--format=%s"]), "upstream g");
        assert!(
            should_compose(&work).is_none(),
            "a fast-forward opens nothing"
        );

        // Fast-forward only: git refuses, as it would on the command line.
        let (_u, _r, work, result) = diverged(&[("pull.ff", "only")]);
        assert!(
            result.is_err(),
            "a diverged ff-only pull must fail: {result:?}"
        );
        assert!(should_compose(&work).is_none());
    }

    /// CC.2's gate: a pull opens the commit buffer only when git stopped
    /// with a commit pending AND nothing left to resolve. A conflict is the
    /// user's to resolve first; a fast-forward or an idle repo has nothing
    /// to compose.
    #[test]
    fn compose_waits_for_a_resolved_stop() {
        let d = repo();
        let p = d.path();
        assert!(should_compose(p).is_none(), "idle: nothing to compose");

        git(p, &["checkout", "-q", "-b", "side"]);
        commit_as(p, "Me", "f", "side\n", "on side");
        git(p, &["checkout", "-q", "main"]);
        commit_as(p, "Me", "f", "main\n", "on main");
        git(p, &["merge", "side"]);
        assert!(should_compose(p).is_none(), "conflicted: resolve first");

        std::fs::write(p.join("f"), "resolved\n").unwrap();
        git(p, &["add", "f"]);
        assert!(should_compose(p).is_some(), "resolved and staged: compose");

        // A clean `--no-commit` merge — what a diverged pull now leaves.
        let clean = repo();
        let c = clean.path();
        git(c, &["checkout", "-q", "-b", "side"]);
        commit_as(c, "Me", "g", "g\n", "add g");
        git(c, &["checkout", "-q", "main"]);
        commit_as(c, "Me", "h", "h\n", "add h");
        git(c, &["merge", "--no-commit", "side"]);
        assert!(
            should_compose(c).is_some(),
            "stopped before committing: compose"
        );

        // Named for the repository, wherever in it the operation ran.
        std::fs::create_dir(c.join("sub")).unwrap();
        let root = should_compose(&c.join("sub")).expect("pending from a subdirectory");
        assert_eq!(
            root.canonicalize().unwrap(),
            c.canonicalize().unwrap(),
            "the repository's root, not the subdirectory"
        );
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
