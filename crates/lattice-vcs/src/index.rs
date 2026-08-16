use std::path::Path;

use crate::{Repository, Result, VcsError};

/// Index (staging area) write operations.
pub struct Index;

impl Index {
    /// Stage a file path (equivalent to `git add <path>`).
    pub fn stage_path(repo: &Repository, path: impl AsRef<Path>) -> Result<()> {
        Self::stage_paths(repo, [path.as_ref()])
    }

    /// Stage every path in ONE `git add`.
    ///
    /// Not a convenience wrapper over [`Self::stage_path`] — the number
    /// of git invocations is the point. Each one spawns a process and
    /// takes `.git/index.lock` for the duration, so staging N files as N
    /// commands is N process spawns and N lock cycles, every one of them
    /// a window in which any other git operation in the editor fails
    /// with "Unable to create index.lock: File exists" (reported
    /// 2026-08-16 while staging a visual-mode selection).
    ///
    /// One command is also ATOMIC where the loop was not: a loop can
    /// fail partway and leave half the selection staged, which is why
    /// the magit layer had to model "3 of 5 staged" as an outcome at
    /// all.
    ///
    /// Mirrors [`Self::unstage_paths`], which has always been one
    /// command — it had to be, because a staged rename occupies two
    /// index entries that must reset together.
    pub fn stage_paths<I, P>(repo: &Repository, paths: I) -> Result<()>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let owned: Vec<String> = paths
            .into_iter()
            .map(|p| p.as_ref().to_string_lossy().into_owned())
            .collect();
        if owned.is_empty() {
            return Ok(());
        }
        let mut args: Vec<&str> = vec!["add", "--"];
        args.extend(owned.iter().map(String::as_str));
        repo.run_git(args)
            .map(|_| ())
            .map_err(|e| VcsError::Index(format!("stage_paths {}: {}", owned.join(" "), e)))
    }

    /// Unstage a file path (equivalent to `git reset HEAD -- <path>`).
    pub fn unstage_path(repo: &Repository, path: impl AsRef<Path>) -> Result<()> {
        Self::unstage_paths(repo, [path.as_ref()])
    }

    /// Unstage every path in one `git reset`, which is what a RENAME
    /// requires.
    ///
    /// A staged rename occupies TWO index entries — the new path added
    /// and the old one deleted — and resetting only the new one leaves
    /// `D  old` still staged: a deletion the user never asked for, and
    /// one the next commit would record. Resetting both together
    /// returns the index to HEAD, leaving the rename visible in the
    /// worktree as a delete plus an untracked file, which is exactly
    /// how git reports an unstaged rename (it does not detect them).
    pub fn unstage_paths<I, P>(repo: &Repository, paths: I) -> Result<()>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let owned: Vec<String> = paths
            .into_iter()
            .map(|p| p.as_ref().to_string_lossy().into_owned())
            .collect();
        if owned.is_empty() {
            return Ok(());
        }
        let mut args: Vec<&str> = vec!["reset", "HEAD", "--"];
        args.extend(owned.iter().map(String::as_str));
        repo.run_git(args)
            .map(|_| ())
            .map_err(|e| VcsError::Index(format!("unstage_paths {}: {}", owned.join(" "), e)))
    }

    /// MG.18a: apply a unified-diff `patch` to the index (`cached`) or
    /// the working tree, forward or `reverse`d. This is the unit of
    /// **partial** staging — the caller synthesizes a patch containing
    /// exactly the hunks (or the rewritten hunk) it wants to move, and
    /// this applies it. Git has no "stage hunk N of path P" index
    /// operation; `git add -p` builds a patch and pipes it to
    /// `git apply --cached`, and so do we.
    ///
    /// | Caller intent | `cached` | `reverse` |
    /// |---|---|---|
    /// | stage a hunk | `true` | `false` |
    /// | unstage a hunk | `true` | `true` |
    /// | discard a hunk from the worktree | `false` | `true` |
    ///
    /// `git apply` requires every context line to match the target
    /// exactly, which is the safety property we want: if the worktree
    /// moved under a stale buffer, this fails loudly instead of staging
    /// the wrong lines. Callers surface the error and refresh.
    ///
    /// Replaces the former `stage_hunk` / `unstage_hunk`, which took a
    /// `hunk_index` they discarded and staged the whole file — a
    /// signature that promised precision the body did not deliver. See
    /// `docs/dev/architecture/magit-hunk-staging.md`.
    pub fn apply_patch(repo: &Repository, patch: &str, cached: bool, reverse: bool) -> Result<()> {
        let mut args: Vec<&str> = vec!["apply"];
        if cached {
            args.push("--cached");
        }
        if reverse {
            args.push("--reverse");
        }
        // Read the patch from stdin rather than a temp file: a temp file
        // leaks on crash and races a concurrent magit in the same repo.
        args.push("-");
        repo.run_git_stdin(args, patch.as_bytes())
            .map(|_| ())
            .map_err(|e| {
                VcsError::Index(format!(
                    "apply_patch (cached={cached}, reverse={reverse}): {e}"
                ))
            })
    }
}
