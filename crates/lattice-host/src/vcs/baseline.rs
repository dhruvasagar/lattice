use std::path::PathBuf;

use lattice_diff::subsystem::DiffParticipantSource;
use ropey::Rope;

/// A [`DiffParticipantSource`] that reads file content from a specific
/// git revision (e.g., `"HEAD"`, `"main~3"`) via `git show <rev>:<path>`.
///
/// `snapshot()` is called inside `spawn_blocking` by the diff subsystem,
/// so the blocking I/O of the `git show` child process is acceptable.
///
/// Stores the git workdir path rather than a `gix::Repository` handle
/// because `gix::Repository` is `Send` but not `Sync` (contains `RefCell`s
/// internally), and `DiffParticipantSource` requires `Send + Sync`.
#[derive(Clone, Debug)]
pub struct GitBaseline {
    workdir: PathBuf,
    rev: String,
    rel_path: PathBuf,
}

impl GitBaseline {
    /// `workdir` — the repository working tree root.
    /// `rev` — the git revision (e.g., `"HEAD"`, `"main~3"`).
    /// `rel_path` — file path relative to `workdir`.
    pub fn new(
        workdir: impl Into<PathBuf>,
        rev: impl Into<String>,
        rel_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            workdir: workdir.into(),
            rev: rev.into(),
            rel_path: rel_path.into(),
        }
    }
}

impl DiffParticipantSource for GitBaseline {
    fn snapshot(&self) -> Rope {
        let spec = format!("{}:{}", self.rev, self.rel_path.display());
        let result = std::process::Command::new("git")
            .args(["show", &spec])
            .current_dir(&self.workdir)
            .output();

        match result {
            Ok(output) if output.status.success() => match String::from_utf8(output.stdout) {
                Ok(text) => Rope::from(text),
                Err(_) => Rope::new(),
            },
            _ => {
                tracing::debug!(
                    target: "lattice_host::vcs",
                    "GitBaseline::snapshot failed: {}:{}",
                    self.rev,
                    self.rel_path.display(),
                );
                Rope::new()
            }
        }
    }
}
