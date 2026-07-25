use std::path::{Path, PathBuf};

use crate::{Repository, Result, VcsError};

/// The status of a path in the working tree relative to the index and HEAD.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathStatus {
    /// File is tracked and unmodified.
    Clean,
    /// File is tracked and has unstaged modifications in the working tree.
    Modified,
    /// File is newly staged (in the index but not in HEAD).
    Added,
    /// File was tracked but has been removed from the working tree.
    Deleted,
    /// File is not tracked by git.
    Untracked,
    /// File is ignored via `.gitignore`.
    Ignored,
    /// File has unmerged entries (conflict markers present, e.g. during
    /// merge/rebase).
    Unmerged,
    /// File has both staged and unstaged changes (modified in index AND
    /// working tree).
    Conflicted,
}

/// Working tree and index status operations.
///
/// Uses `git status --porcelain=v1` for status classification.
pub struct WorkingTree;

impl WorkingTree {
    /// Return the status of a single path in the repository.
    ///
    /// Uses `git status --porcelain=v1 -- <path>`.
    pub fn path_status(repo: &Repository, path: impl AsRef<Path>) -> Result<PathStatus> {
        let path = path.as_ref();
        let output = repo.run_git_str([
            "status",
            "--porcelain=v1",
            "--",
            &path.to_string_lossy(),
        ])?;

        if output.trim().is_empty() {
            // File might be tracked but clean, or nonexistent.
            // Check if it's tracked at all.
            match repo.run_git_str([
                "ls-files",
                "--error-unmatch",
                "--",
                &path.to_string_lossy(),
            ]) {
                Ok(_) => Ok(PathStatus::Clean),
                Err(_) => Err(VcsError::StatusParse(format!(
                    "path not in repository: {}",
                    path.display()
                ))),
            }
        } else {
            let status = parse_porcelain_line(output.lines().next().unwrap_or(""));
            Ok(status)
        }
    }

    /// Return the status of every file in the working tree that has
    /// changed relative to the index or HEAD.
    ///
    /// Includes untracked files. Uses `git status --porcelain=v1`.
    pub fn statuses(repo: &Repository) -> Result<Vec<(PathBuf, PathStatus)>> {
        let output = repo.run_git_str(["status", "--porcelain=v1"])?;
        let mut results = Vec::new();

        for line in output.lines() {
            if line.len() < 4 {
                continue;
            }
            let status = parse_porcelain_line(line);
            // Extract path from porcelain line (skip status chars + space)
            let path_str = line[3..].trim();
            // Handle rename entries ("R  old -> new")
            let path_str = if path_str.contains(" -> ") {
                path_str.split(" -> ").last().unwrap_or(path_str)
            } else {
                path_str
            };
            results.push((PathBuf::from(path_str), status));
        }

        Ok(results)
    }
}

/// Parse a single `git status --porcelain=v1` line into a [`PathStatus`].
///
/// Porcelain format: `XY PATH` where X is the index status and Y is the
/// working-tree status.
fn parse_porcelain_line(line: &str) -> PathStatus {
    let chars: Vec<char> = line.chars().take(2).collect();
    if chars.len() < 2 {
        return PathStatus::Modified;
    }
    let x = chars[0]; // index (staging area) status
    let y = chars[1]; // working tree status

    match (x, y) {
        (' ', 'M') => PathStatus::Modified,
        ('M', ' ') => PathStatus::Added, // staged in index, clean in worktree
        ('M', 'M') => PathStatus::Conflicted, // both staged and unstaged changes
        ('A', ' ') => PathStatus::Added,
        ('A', 'M') => PathStatus::Conflicted,
        ('D', ' ') => PathStatus::Deleted,
        ('D', 'M') => PathStatus::Conflicted,
        (' ', 'D') => PathStatus::Deleted,
        ('R', ' ') => PathStatus::Added, // renamed, staged
        ('R', 'M') => PathStatus::Conflicted,
        ('C', ' ') => PathStatus::Added, // copied, staged
        (' ', 'A') => PathStatus::Added, // added in worktree, not yet staged
        ('?', '?') => PathStatus::Untracked,
        ('!', '!') => PathStatus::Ignored,
        ('U', _) | (_, 'U') | ('A', 'A') | ('D', 'D') => PathStatus::Unmerged,
        _ => {
            // Any other status — treat as modified
            PathStatus::Modified
        }
    }
}
