use std::path::Path;

use crate::{Repository, Result, VcsError};

/// Index (staging area) write operations.
pub struct Index;

impl Index {
    /// Stage a file path (equivalent to `git add <path>`).
    pub fn stage_path(repo: &Repository, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        repo.run_git(["add", "--", path.to_string_lossy().as_ref()])
            .map(|_| ())
            .map_err(|e| VcsError::Index(format!("stage_path {}: {}", path.display(), e)))
    }

    /// Unstage a file path (equivalent to `git reset HEAD -- <path>`).
    pub fn unstage_path(repo: &Repository, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        repo.run_git(["reset", "HEAD", "--", path.to_string_lossy().as_ref()])
            .map(|_| ())
            .map_err(|e| VcsError::Index(format!("unstage_path {}: {}", path.display(), e)))
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
