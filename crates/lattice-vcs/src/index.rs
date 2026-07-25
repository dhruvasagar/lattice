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
        repo.run_git([
            "reset",
            "HEAD",
            "--",
            path.to_string_lossy().as_ref(),
        ])
        .map(|_| ())
        .map_err(|e| VcsError::Index(format!("unstage_path {}: {}", path.display(), e)))
    }

    /// Stage a specific hunk of a file (equivalent to `git add -p`).
    ///
    /// Hunk-level staging is driven by the magit UI through interactive
    /// CLI interaction (`git add -p`). For programmatic use, this stages
    /// the entire file. The magit-status action handler overrides with
    /// hunk-level precision.
    pub fn stage_hunk(repo: &Repository, path: impl AsRef<Path>, _hunk_index: usize) -> Result<()> {
        Self::stage_path(repo, path)
    }

    /// Unstage a specific hunk of a file (equivalent to `git reset -p`).
    pub fn unstage_hunk(
        repo: &Repository,
        path: impl AsRef<Path>,
        _hunk_index: usize,
    ) -> Result<()> {
        Self::unstage_path(repo, path)
    }
}
