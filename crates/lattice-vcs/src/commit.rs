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

    /// MG.42-E1: replace the last commit's MESSAGE ONLY, leaving the
    /// index alone — magit's `w` reword.
    ///
    /// `--only` is the whole difference from [`Self::amend`]. Without
    /// it, anything currently staged is swept into the commit being
    /// reworded, which is a content change the user did not ask for
    /// and would not see coming from a row labelled "reword".
    pub fn reword(repo: &Repository, message: &str) -> Result<()> {
        repo.run_git(["commit", "--amend", "--only", "-m", message])
            .map(|_| ())
            .map_err(|e| VcsError::Index(format!("commit reword: {}", e)))
    }
}
