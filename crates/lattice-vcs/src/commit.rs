use crate::{Repository, Result, VcsError};

/// Commit operations — create and amend commits.
///
/// Uses the git CLI.
pub struct Commit;

impl Commit {
    /// Create a new commit with the given message.
    ///
    /// Only staged changes are committed. Equivalent to
    /// `git commit -m <message>`.
    pub fn create(repo: &Repository, message: &str) -> Result<()> {
        repo.run_git(["commit", "-m", message])
            .map(|_| ())
            .map_err(|e| VcsError::Index(format!("commit create: {}", e)))
    }

    /// Amend the most recent commit, replacing its message and
    /// incorporating any currently staged changes.
    ///
    /// Equivalent to `git commit --amend -m <message>`.
    pub fn amend(repo: &Repository, message: &str) -> Result<()> {
        repo.run_git(["commit", "--amend", "-m", message])
            .map(|_| ())
            .map_err(|e| VcsError::Index(format!("commit amend: {}", e)))
    }
}
