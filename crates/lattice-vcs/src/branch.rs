use crate::{Repository, Result, VcsError};

/// Branch operations — checkout, create, delete.
///
/// Uses the git CLI.
pub struct Branch;

impl Branch {
    /// Check out an existing branch by name.
    ///
    /// Equivalent to `git checkout <name>`.
    pub fn checkout(repo: &Repository, name: &str) -> Result<()> {
        repo.run_git(["checkout", name])
            .map(|_| ())
            .map_err(|e| VcsError::Index(format!("branch checkout {}: {}", name, e)))
    }

    /// Create a new branch and optionally check it out.
    ///
    /// If `checkout` is `true`, equivalent to `git checkout -b <name>`.
    /// Otherwise equivalent to `git branch <name>`.
    pub fn create(repo: &Repository, name: &str, checkout: bool) -> Result<()> {
        if checkout {
            repo.run_git(["checkout", "-b", name])
        } else {
            repo.run_git(["branch", name])
        }
        .map(|_| ())
        .map_err(|e| VcsError::Index(format!("branch create {}: {}", name, e)))
    }

    /// Delete a branch by name.
    ///
    /// Uses `-D` (force-delete). Callers should confirm with the user
    /// before calling this on unmerged branches.
    pub fn delete(repo: &Repository, name: &str) -> Result<()> {
        repo.run_git(["branch", "-D", name])
            .map(|_| ())
            .map_err(|e| VcsError::Index(format!("branch delete {}: {}", name, e)))
    }

    /// List all local branches.
    ///
    /// Equivalent to `git branch --format=%(refname:short)`.
    pub fn list(repo: &Repository) -> Result<Vec<String>> {
        repo.run_git_lines(["branch", "--format=%(refname:short)"])
    }
}
