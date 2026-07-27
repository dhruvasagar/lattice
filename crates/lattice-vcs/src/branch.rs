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
    /// If `checkout` is `true`, equivalent to `git checkout -b <name>
    /// [<from>]`. Otherwise equivalent to `git branch <name> [<from>]`.
    /// `from` is the base ref to branch off of; `None` uses git's own
    /// default (HEAD).
    pub fn create(repo: &Repository, name: &str, checkout: bool, from: Option<&str>) -> Result<()> {
        let mut args = if checkout {
            vec!["checkout", "-b", name]
        } else {
            vec!["branch", name]
        };
        if let Some(base) = from {
            args.push(base);
        }
        repo.run_git(args)
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
