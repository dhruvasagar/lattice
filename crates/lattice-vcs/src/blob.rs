use std::path::Path;

use ropey::Rope;

use crate::{Repository, Result};

/// Read the content of a git blob (file at a specific revision).
pub struct GitBlob;

impl GitBlob {
    /// Read the blob identified by `oid` and return its content as a
    /// [`Rope`]. Uses `git cat-file -p <oid>`.
    pub fn read(repo: &Repository, oid: &gix::ObjectId) -> Result<Rope> {
        let oid_str = oid.to_string();
        let text = repo.run_git_str(["cat-file", "-p", &oid_str])?;
        Ok(Rope::from(text))
    }

    /// Read the file at `path` from a specific revision (e.g., `"HEAD"`,
    /// `"main~3"`). Uses `git show <rev>:<path>`.
    pub fn read_path(repo: &Repository, rev: &str, path: impl AsRef<Path>) -> Result<Rope> {
        let spec = format!("{}:{}", rev, path.as_ref().display());
        let text = repo.run_git_str(["show", &spec])?;
        Ok(Rope::from(text))
    }
}
