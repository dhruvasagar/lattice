use crate::{Repository, Result, VcsError};

/// Reference operations — resolve a named ref to an object id.
pub struct Reference;

impl Reference {
    /// Resolve a reference name (e.g., `"HEAD"`, `"refs/heads/main"`,
    /// `"main"`) to its target object id.
    ///
    /// Uses `git rev-parse --verify <name>`.
    /// Short names like `"main"` are resolved via git's standard ref
    /// resolution rules.
    pub fn resolve(repo: &Repository, name: &str) -> Result<gix::ObjectId> {
        let hex = repo
            .run_git_str(["rev-parse", "--verify", name])
            .map_err(|e| VcsError::ReferenceNotFound(format!("{}: {}", name, e)))?;
        let hex = hex.trim();
        let oid: gix::ObjectId = hex
            .parse()
            .map_err(|_| VcsError::ReferenceNotFound(format!("invalid oid: {}", hex)))?;
        Ok(oid)
    }

    /// Return the symbolic target of a reference, if it is symbolic
    /// (e.g., `"HEAD"` → `"refs/heads/main"`).
    ///
    /// Uses `git symbolic-ref -q <name>`.
    pub fn symbolic_target(repo: &Repository, name: &str) -> Result<Option<String>> {
        match repo.run_git_str(["symbolic-ref", "-q", name]) {
            Ok(target) => Ok(Some(target.trim().to_string())),
            Err(_) => Ok(None),
        }
    }
}
