use std::path::Path;

use crate::{Result, VcsError};

/// Passed to EVERY git invocation, before the subcommand.
///
/// Several commands that only look like reads — `status` above all — take
/// `.git/index.lock` in order to opportunistically rewrite the index with
/// refreshed stat data. That write is optional; the lock it takes is not
/// optional for anybody else. Git does not retry a contended index lock, it
/// fails:
///
/// ```text
/// fatal: Unable to create '.../.git/index.lock': File exists.
/// Another git process seems to be running in this repository [...]
/// ```
///
/// Magit runs its reads on `spawn_blocking` and refreshes every live status
/// buffer after every mutation, so reads and index writes overlap by
/// construction. On a 4418-file repository one refresh measures ~300ms, and
/// staging several entries in a row is the commonest magit workflow — so the
/// window is neither rare nor avoidable by the user, who gets an error
/// blaming them for a race the editor caused. Reported 2026-09-23; racing
/// `git status` against `git add` failed 28 of 200 attempts, and 0 of 200
/// with this flag.
///
/// Applied globally rather than only to reads because it is precisely scoped
/// already: it suppresses *optional* locks only. `git --no-optional-locks add`
/// still takes the index lock and still stages, because there the lock is
/// required. So there is no read/write classification to get wrong, and no
/// call site that can forget it.
///
/// The cost, named honestly: a read no longer persists its refreshed stat
/// cache, so the next one redoes that `lstat` work. That is real, and it is
/// the right trade — the work happens off the UI thread, and a stage that
/// fails outright is a correctness bug the user sees immediately.
///
/// Requires git ≥ 2.15 (2017).
const NO_OPTIONAL_LOCKS: &str = "--no-optional-locks";

/// Wraps a [`gix::Repository`], representing an open git repository.
///
/// Created via [`Repository::discover`], which walks up from `path`
/// until it finds a `.git` directory (matching `git`'s behaviour).
pub struct Repository {
    inner: gix::Repository,
}

impl Repository {
    /// Walk up from `path` to find the nearest git repository.
    ///
    /// Returns an error if no `.git` directory is found in any
    /// ancestor directory.
    pub fn discover(path: impl AsRef<Path>) -> Result<Self> {
        let inner = gix::discover(path)?;
        Ok(Self { inner })
    }

    /// The absolute path of the repository's working tree root.
    ///
    /// Returns `None` for bare repositories.
    pub fn workdir(&self) -> Option<&Path> {
        self.inner.workdir()
    }

    /// The absolute path of the repository's `.git` directory.
    pub fn gitdir(&self) -> &Path {
        self.inner.git_dir()
    }

    /// Access the inner [`gix::Repository`] for operations that need
    /// direct access to the gix API.
    pub fn inner(&self) -> &gix::Repository {
        &self.inner
    }

    /// Check whether this is a bare repository (has no working tree).
    pub fn is_bare(&self) -> bool {
        self.inner.is_bare()
    }

    /// Run a git command in the working directory and return its stdout
    /// as bytes. Runs on the calling thread.
    pub fn run_git<I, S>(&self, args: I) -> Result<Vec<u8>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let workdir = self
            .workdir()
            .ok_or_else(|| VcsError::BareRepo("run_git".into()))?;
        let output = std::process::Command::new("git")
            .arg(NO_OPTIONAL_LOCKS)
            .args(args)
            .current_dir(workdir)
            .output()
            .map_err(|e| VcsError::GitCommand {
                context: "run_git".into(),
                source: e,
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(VcsError::GitCommandFailed {
                stderr: stderr.into_owned(),
            });
        }
        Ok(output.stdout)
    }

    /// MG.18a: run a git command with `input` piped to its stdin,
    /// returning stdout as bytes. Runs on the calling thread.
    ///
    /// Needed by [`crate::Index::apply_patch`]: `git apply -` reads the
    /// patch from stdin, and [`Self::run_git`]'s `.output()` gives the
    /// child a null stdin. Writing the patch to a temp file instead
    /// would leak it on a crash and race a concurrent magit in the same
    /// repository.
    ///
    /// The write is completed and stdin dropped **before** waiting, so a
    /// child that consumes its whole input can exit; holding the pipe
    /// open past the write deadlocks against a child waiting on EOF.
    pub fn run_git_stdin<I, S>(&self, args: I, input: &[u8]) -> Result<Vec<u8>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        use std::io::Write;
        let workdir = self
            .workdir()
            .ok_or_else(|| VcsError::BareRepo("run_git_stdin".into()))?;
        let mut child = std::process::Command::new("git")
            .arg(NO_OPTIONAL_LOCKS)
            .args(args)
            .current_dir(workdir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| VcsError::GitCommand {
                context: "run_git_stdin".into(),
                source: e,
            })?;
        {
            let mut stdin = child.stdin.take().ok_or_else(|| VcsError::GitCommand {
                context: "run_git_stdin: stdin already taken".into(),
                source: std::io::Error::other("no stdin"),
            })?;
            stdin.write_all(input).map_err(|e| VcsError::GitCommand {
                context: "run_git_stdin: write".into(),
                source: e,
            })?;
            // `stdin` drops here, closing the pipe.
        }
        let output = child.wait_with_output().map_err(|e| VcsError::GitCommand {
            context: "run_git_stdin: wait".into(),
            source: e,
        })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(VcsError::GitCommandFailed {
                stderr: stderr.into_owned(),
            });
        }
        Ok(output.stdout)
    }

    /// Run a git command and return stdout as a UTF-8 string.
    pub fn run_git_str<I, S>(&self, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let bytes = self.run_git(args)?;
        String::from_utf8(bytes).map_err(|e| VcsError::Utf8 {
            context: "run_git_str".into(),
            source: e,
        })
    }

    /// Run a git command and return stdout lines as trimmed UTF-8 strings.
    pub fn run_git_lines<I, S>(&self, args: I) -> Result<Vec<String>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let out = self.run_git_str(args)?;
        Ok(out
            .lines()
            .filter(|l| !l.is_empty())
            .map(|s| s.to_string())
            .collect())
    }
}

impl std::fmt::Debug for Repository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Repository")
            .field("workdir", &self.workdir())
            .field("gitdir", &self.gitdir())
            .field("is_bare", &self.is_bare())
            .finish()
    }
}
