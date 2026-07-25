use crate::{Repository, Result, VcsError};

/// Stash operations — list, apply, pop, drop, create.
///
/// Uses the git CLI.
pub struct Stash;

/// A single stash entry.
#[derive(Debug, Clone)]
pub struct StashEntry {
    /// Zero-based index in the stash list (0 is the most recent).
    pub index: usize,
    /// The stash message (first line of the stash commit message).
    pub message: String,
}

impl Stash {
    /// List all stash entries, newest first.
    ///
    /// Equivalent to `git stash list`.
    pub fn list(repo: &Repository) -> Result<Vec<StashEntry>> {
        let lines = repo.run_git_str(["stash", "list"])?;
        let mut entries = Vec::new();
        for (i, line) in lines.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            entries.push(StashEntry {
                index: i,
                message: line.trim().to_string(),
            });
        }
        Ok(entries)
    }

    /// Apply a stash entry without removing it from the stash list.
    ///
    /// Equivalent to `git stash apply stash@{<index>}`.
    pub fn apply(repo: &Repository, index: usize) -> Result<()> {
        let refspec = format!("stash@{{{}}}", index);
        repo.run_git(["stash", "apply", &refspec])
            .map(|_| ())
            .map_err(|e| VcsError::Stash(format!("stash apply {}: {}", index, e)))
    }

    /// Apply a stash entry and remove it from the stash list.
    ///
    /// Equivalent to `git stash pop stash@{<index>}`.
    pub fn pop(repo: &Repository, index: usize) -> Result<()> {
        let refspec = format!("stash@{{{}}}", index);
        repo.run_git(["stash", "pop", &refspec])
            .map(|_| ())
            .map_err(|e| VcsError::Stash(format!("stash pop {}: {}", index, e)))
    }

    /// Drop a stash entry without applying it.
    ///
    /// Equivalent to `git stash drop stash@{<index>}`.
    pub fn drop(repo: &Repository, index: usize) -> Result<()> {
        let refspec = format!("stash@{{{}}}", index);
        repo.run_git(["stash", "drop", &refspec])
            .map(|_| ())
            .map_err(|e| VcsError::Stash(format!("stash drop {}: {}", index, e)))
    }

    /// Create a new stash from the current working tree state.
    ///
    /// If `include_untracked` is `true`, untracked files are also stashed.
    /// If `message` is provided, it is used as the stash message.
    ///
    /// Equivalent to `git stash push [-u] [-m <message>]`.
    pub fn create(
        repo: &Repository,
        message: Option<&str>,
        include_untracked: bool,
    ) -> Result<()> {
        let mut args: Vec<String> = vec!["stash".into(), "push".into()];
        if include_untracked {
            args.push("--include-untracked".into());
        }
        if let Some(msg) = message {
            args.push("-m".into());
            args.push(msg.into());
        }
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        repo.run_git(refs)
            .map(|_| ())
            .map_err(|e| VcsError::Stash(format!("stash create: {}", e)))
    }
}
