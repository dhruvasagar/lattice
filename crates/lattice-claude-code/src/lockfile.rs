//! Discovery lockfile: `~/.claude/ide/<port>.lock`.
//!
//! An attaching `claude` CLI discovers a running IDE by scanning
//! `~/.claude/ide/` for `<port>.lock` files and reading the connection
//! details (port from the filename, auth token + workspace from the JSON
//! body). The lockfile is RAII: written on `start`, unlinked on `Drop`,
//! so a crashed or stopped server leaves no stale discovery entry.
//!
//! NOTE: the exact JSON field set mirrors the VS Code IDE-integration
//! schema and is **provisional** — verified against a live `claude` CLI
//! before the terminal-launch slice (I5, Risk 1).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Body of a `<port>.lock` discovery file. `camelCase` on the wire to
/// match the VS Code schema the `claude` CLI expects.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LockfileContents {
    /// PID of the editor process hosting the IDE server.
    pub pid: u32,
    /// Absolute paths of the open workspace folders.
    pub workspace_folders: Vec<String>,
    /// Human-readable IDE name shown by the agent.
    pub ide_name: String,
    /// Transport discriminator; always `"ws"` for the loopback WebSocket.
    pub transport: String,
    /// Per-session authorization token (also required in the handshake
    /// header). See [`crate::auth`].
    pub auth_token: String,
    /// Whether the host runs on Windows (affects path handling on the
    /// agent side). Defaults to `false` when absent.
    #[serde(default)]
    pub running_in_windows: bool,
}

/// The default discovery directory, `~/.claude/ide`. `None` when the home
/// directory can't be resolved (the caller logs-and-skips).
pub fn default_lock_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("ide"))
}

/// An RAII discovery lockfile. Created by [`write`](Self::write); the file
/// is removed when this value is dropped.
#[derive(Debug)]
pub struct Lockfile {
    path: PathBuf,
}

impl Lockfile {
    /// Write `<port>.lock` under `dir` (creating `dir` if needed) with the
    /// given contents, returning the RAII handle.
    pub fn write(dir: &Path, port: u16, contents: &LockfileContents) -> Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join(format!("{port}.lock"));
        let json = serde_json::to_vec_pretty(contents)?;
        fs::write(&path, json)?;
        Ok(Self { path })
    }

    /// Path of the written lockfile.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Lockfile {
    fn drop(&mut self) {
        // Log-and-skip: a failed unlink (already removed, perms) must
        // never panic on teardown.
        if let Err(e) = fs::remove_file(&self.path) {
            tracing::debug!(path = %self.path.display(), error = %e, "lockfile unlink failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn unique_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("lattice-cc-{}-{}-{}", tag, std::process::id(), n))
    }

    fn sample(port_token: &str) -> LockfileContents {
        LockfileContents {
            pid: 4242,
            workspace_folders: vec!["/work/project".to_string()],
            ide_name: "Lattice".to_string(),
            transport: "ws".to_string(),
            auth_token: port_token.to_string(),
            running_in_windows: false,
        }
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = unique_dir("roundtrip");
        let contents = sample("deadbeefcafef00ddeadbeefcafef00d");
        let lock = Lockfile::write(&dir, 12345, &contents).expect("write");
        assert!(lock.path().exists());
        assert_eq!(lock.path().file_name().unwrap(), "12345.lock");

        let raw = std::fs::read(lock.path()).expect("read");
        let parsed: LockfileContents = serde_json::from_slice(&raw).expect("parse");
        assert_eq!(parsed, contents);
        // Camel-case key present on the wire.
        let text = String::from_utf8(raw).unwrap();
        assert!(text.contains("\"authToken\""));
        assert!(text.contains("\"workspaceFolders\""));

        drop(lock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unlinks_on_drop() {
        let dir = unique_dir("unlink");
        let path = {
            let lock = Lockfile::write(&dir, 23456, &sample("token")).expect("write");
            let p = lock.path().to_path_buf();
            assert!(p.exists());
            p
            // lock dropped here
        };
        assert!(!path.exists(), "lockfile should be removed on Drop");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
