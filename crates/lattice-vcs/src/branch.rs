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

    /// Rename a branch.
    ///
    /// Equivalent to `git branch -m <old> <new>`. Deliberately **not**
    /// `-M`: the lowercase form refuses when `new` already names a
    /// branch, and the uppercase one overwrites it. Overwriting here
    /// destroys whatever `new` pointed at, silently, so the refusal is
    /// the behaviour we want and the error is propagated rather than
    /// swallowed.
    ///
    /// Renaming the checked-out branch carries HEAD with it (git's own
    /// behaviour); it does not detach.
    pub fn rename(repo: &Repository, old: &str, new: &str) -> Result<()> {
        repo.run_git(["branch", "-m", old, new])
            .map(|_| ())
            .map_err(|e| VcsError::Index(format!("branch rename {} -> {}: {}", old, new, e)))
    }

    /// List all local branches.
    ///
    /// Equivalent to `git branch --format=%(refname:short)`.
    pub fn list(repo: &Repository) -> Result<Vec<String>> {
        repo.run_git_lines(["branch", "--format=%(refname:short)"])
    }
}
