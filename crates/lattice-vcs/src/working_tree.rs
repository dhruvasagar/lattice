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

/// A path's porcelain `XY` status, kept as the TWO independent axes git
/// reports rather than collapsed into one value.
///
/// `X` is the index vs HEAD ("what a commit would record") and `Y` is
/// the working tree vs the index ("what staging would add"). They vary
/// independently: `MM` means a file has staged changes *and* further
/// unstaged ones, and it belongs in both of magit's sections with
/// "modified" on each row.
///
/// Collapsing them into a single [`PathStatus`] is what produced the
/// reported bug — a staged modification (`M `) had to be reported as
/// `Added` to make it land in the staged section, so it rendered as
/// "new file", and `MM` had to be reported as `Conflicted` (which means
/// a merge conflict) purely to make it appear in both. One value cannot
/// answer both "which section" and "what label" without lying about one
/// of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathChange {
    /// Index vs HEAD. `None` when nothing is staged for this path.
    pub staged: Option<PathStatus>,
    /// Working tree vs index. `None` when the worktree matches the
    /// index. Also carries [`PathStatus::Untracked`] / `Ignored` /
    /// `Unmerged`, which are whole-path states rather than one axis.
    pub unstaged: Option<PathStatus>,
}

impl PathChange {
    /// Nothing staged, nothing modified.
    pub const CLEAN: Self = Self {
        staged: None,
        unstaged: None,
    };

    /// `true` when git reports no change on either axis.
    pub fn is_clean(&self) -> bool {
        self.staged.is_none() && self.unstaged.is_none()
    }
}

/// Working tree and index status operations.
///
/// Uses `git status --porcelain=v1` for status classification.
pub struct WorkingTree;

impl WorkingTree {
    /// Return the status of a single path in the repository.
    ///
    /// Uses `git status --porcelain=v1 -- <path>`.
    pub fn path_status(repo: &Repository, path: impl AsRef<Path>) -> Result<PathChange> {
        let path = path.as_ref();
        let output =
            repo.run_git_str(["status", "--porcelain=v1", "--", &path.to_string_lossy()])?;

        if output.trim().is_empty() {
            // File might be tracked but clean, or nonexistent.
            // Check if it's tracked at all.
            match repo.run_git_str(["ls-files", "--error-unmatch", "--", &path.to_string_lossy()]) {
                Ok(_) => Ok(PathChange::CLEAN),
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
    pub fn statuses(repo: &Repository) -> Result<Vec<(PathBuf, PathChange)>> {
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

/// Parse a single `git status --porcelain=v1` line into a [`PathChange`].
///
/// Porcelain format: `XY PATH` where X is the index status and Y is the
/// working-tree status. The two are decoded SEPARATELY — see
/// [`PathChange`] for why collapsing them is a bug rather than a
/// simplification.
fn parse_porcelain_line(line: &str) -> PathChange {
    let chars: Vec<char> = line.chars().take(2).collect();
    if chars.len() < 2 {
        return PathChange::CLEAN;
    }
    let x = chars[0]; // index (staging area) status
    let y = chars[1]; // working tree status

    // Whole-path states first: these are not per-axis, and git spells
    // them with the same char in both columns.
    match (x, y) {
        ('?', '?') => {
            return PathChange {
                staged: None,
                unstaged: Some(PathStatus::Untracked),
            };
        }
        ('!', '!') => {
            return PathChange {
                staged: None,
                unstaged: Some(PathStatus::Ignored),
            };
        }
        // Unmerged: `U` on either side, plus the `AA` / `DD` both-added
        // and both-deleted cases. Reported on the unstaged axis because
        // the resolution work is in the worktree.
        ('U', _) | (_, 'U') | ('A', 'A') | ('D', 'D') => {
            return PathChange {
                staged: None,
                unstaged: Some(PathStatus::Unmerged),
            };
        }
        _ => {}
    }

    // Index axis. `R` (renamed) and `C` (copied) both introduce a path
    // that HEAD does not have, so `Added` is the honest label for the
    // new side; git's own `status` says "renamed"/"copied", which needs
    // a `PathStatus` variant this enum does not have yet.
    let staged = match x {
        'M' => Some(PathStatus::Modified),
        'A' => Some(PathStatus::Added),
        'D' => Some(PathStatus::Deleted),
        'R' | 'C' => Some(PathStatus::Added),
        _ => None,
    };

    // Worktree axis.
    let unstaged = match y {
        'M' => Some(PathStatus::Modified),
        'D' => Some(PathStatus::Deleted),
        'A' => Some(PathStatus::Added),
        _ => None,
    };

    PathChange { staged, unstaged }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(xy: &str) -> PathChange {
        parse_porcelain_line(&format!("{xy} some/path.txt"))
    }

    /// The porcelain table, pinned per axis.
    ///
    /// Reported 2026-08-10: a staged modification rendered as "new
    /// file" in magit's Staged-changes section. `('M', ' ')` was mapped
    /// to `PathStatus::Added` — not a typo, but the only way to make a
    /// single collapsed value land the row in the staged section, since
    /// magit's refresh chose the section FROM that value. The label was
    /// the price. `MM` had the same shape: reported as `Conflicted`
    /// (which means a merge conflict) purely to appear in both
    /// sections.
    ///
    /// Both axes are decoded separately now, so "which section" and
    /// "what label" stop fighting over one value.
    #[test]
    fn porcelain_xy_decodes_both_axes_independently() {
        // The reported case: staged modification. Staged, and MODIFIED
        // — not added.
        assert_eq!(
            parse("M "),
            PathChange {
                staged: Some(PathStatus::Modified),
                unstaged: None
            }
        );
        // Unstaged modification.
        assert_eq!(
            parse(" M"),
            PathChange {
                staged: None,
                unstaged: Some(PathStatus::Modified)
            }
        );
        // Both axes at once — two modifications, not a conflict.
        assert_eq!(
            parse("MM"),
            PathChange {
                staged: Some(PathStatus::Modified),
                unstaged: Some(PathStatus::Modified)
            }
        );
        // A genuinely new file, staged.
        assert_eq!(
            parse("A "),
            PathChange {
                staged: Some(PathStatus::Added),
                unstaged: None
            }
        );
        // Staged new file with further unstaged edits.
        assert_eq!(
            parse("AM"),
            PathChange {
                staged: Some(PathStatus::Added),
                unstaged: Some(PathStatus::Modified)
            }
        );
        // Deletions, each side.
        assert_eq!(
            parse("D "),
            PathChange {
                staged: Some(PathStatus::Deleted),
                unstaged: None
            }
        );
        assert_eq!(
            parse(" D"),
            PathChange {
                staged: None,
                unstaged: Some(PathStatus::Deleted)
            }
        );
        // Rename / copy introduce a path HEAD lacks, so `Added` is the
        // honest label for the new side until `PathStatus` grows a
        // `Renamed` variant.
        assert_eq!(parse("R ").staged, Some(PathStatus::Added));
        assert_eq!(parse("C ").staged, Some(PathStatus::Added));
    }

    /// Whole-path states are not per-axis: git spells them in both
    /// columns and they describe the path, not one side of it.
    #[test]
    fn porcelain_whole_path_states() {
        assert_eq!(parse("??").unstaged, Some(PathStatus::Untracked));
        assert_eq!(parse("??").staged, None);
        assert_eq!(parse("!!").unstaged, Some(PathStatus::Ignored));
        // Unmerged, including both-added and both-deleted. Reported on
        // the unstaged axis because the resolution work is in the
        // worktree — matching where magit lists it.
        for xy in ["UU", "AA", "DD", "AU", "UD"] {
            assert_eq!(
                parse(xy).unstaged,
                Some(PathStatus::Unmerged),
                "{xy} is an unmerged state"
            );
        }
        // Too short to carry a status ⇒ nothing claimed.
        assert!(parse_porcelain_line("").is_clean());
    }
}
