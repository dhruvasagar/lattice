//! Child-process transport for an LSP server.
//!
//! Spawns the configured server binary, captures its stdio, and
//! exposes split [`LspReader`] / [`LspWriter`] halves so the
//! actor can run `read_loop` and `write_loop` on independent
//! tokio tasks (the canonical full-duplex pattern).
//!
//! ## Cross-platform process discovery
//!
//! `tokio::process::Command::new(name)` consults the host's
//! `PATH` (and on Windows applies the standard `.exe` suffix
//! search). The transport does no manual PATH walking; if a
//! server binary cannot be found, the error returned from
//! `spawn` carries the OS-level reason verbatim.
//!
//! ## stderr capture
//!
//! LSP servers log to stderr. We pipe stderr into a background
//! task that emits each line through `tracing::warn!` with a
//! `server_id` field so users can debug a misbehaving server
//! without seeing its noise on the terminal. The stderr task
//! ends naturally when the server closes the pipe.
//!
//! ## Lifecycle
//!
//! `ChildTransport` owns the `Child` handle. Dropping the
//! transport closes stdin (signalling shutdown to the server in
//! the LSP protocol) but does NOT kill the process -- the actor
//! is responsible for sending the `shutdown`/`exit` LSP sequence
//! and awaiting graceful exit. Use [`ChildTransport::kill`] to
//! force-terminate from a `Drop` implementation in the actor's
//! supervision layer.

use std::ffi::OsStr;
use std::path::PathBuf;
use std::process::Stdio;

use thiserror::Error;
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};

use crate::codec::{LspReader, LspWriter};

/// Spawn errors and lifecycle errors for the transport.
#[derive(Debug, Error)]
pub enum TransportError {
    /// `spawn` failed -- usually because the binary isn't on
    /// PATH or isn't executable.
    #[error("failed to spawn LSP server {binary:?}: {source}")]
    Spawn {
        binary: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Spawn succeeded but stdio pipes weren't captured. Should
    /// be impossible given `Stdio::piped()` on all three handles,
    /// but the error path keeps the API total.
    #[error("LSP server stdio not captured")]
    MissingStdio,
    /// `kill` / `wait` failed at lifecycle teardown.
    #[error("transport lifecycle: {0}")]
    Lifecycle(#[source] std::io::Error),
}

/// One spawned LSP server process and its split codec halves.
///
/// Constructed by [`ChildTransport::spawn`]. After
/// [`ChildTransport::split`] the reader, writer, and child handle
/// can be moved into independent tasks. The actor pattern:
///
/// ```ignore
/// let t = ChildTransport::spawn("rust-analyzer", &[], None).await?;
/// let (reader, writer, child) = t.split();
/// tokio::spawn(read_loop(reader));
/// tokio::spawn(write_loop(writer, mailbox_rx));
/// // child handle stays with the supervisor for kill / wait.
/// ```
#[derive(Debug)]
pub struct ChildTransport {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    /// Held until [`Self::split`]; consumed there to spawn the
    /// stderr drain task.
    stderr: Option<ChildStderr>,
}

impl ChildTransport {
    /// Spawn the server binary with the given args. `cwd` is the
    /// working directory passed to the child; LSP servers
    /// typically resolve workspace-relative paths against it.
    /// Pass the resolved workspace root when known.
    pub async fn spawn<P, I, S>(
        binary: P,
        args: I,
        cwd: Option<&std::path::Path>,
    ) -> Result<Self, TransportError>
    where
        P: AsRef<OsStr>,
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let binary_path = PathBuf::from(binary.as_ref());
        let mut cmd = Command::new(&binary_path);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // kill_on_drop matches our supervision contract: if
            // the actor task panics before sending shutdown/exit,
            // we don't leak a server process. The actor's normal
            // path still runs the LSP shutdown handshake.
            .kill_on_drop(true);
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        let mut child = cmd.spawn().map_err(|source| TransportError::Spawn {
            binary: binary_path.clone(),
            source,
        })?;

        let stdin = child.stdin.take().ok_or(TransportError::MissingStdio)?;
        let stdout = child.stdout.take().ok_or(TransportError::MissingStdio)?;
        let stderr = child.stderr.take();
        Ok(Self {
            child,
            stdin,
            stdout,
            stderr,
        })
    }

    /// Process id of the spawned server. Useful for logs /
    /// telemetry. `None` if the child has already been reaped.
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Consume the transport and return the reader, writer, and
    /// retained child handle. The stderr pipe (if captured) is
    /// returned alongside so the caller can spawn its own drain
    /// task with whatever logging context is appropriate
    /// (server_id, language, workspace root). This keeps the
    /// transport free of `tracing` calls -- the actor decides.
    pub fn split(
        mut self,
    ) -> (
        LspReader<tokio::io::BufReader<ChildStdout>>,
        LspWriter<ChildStdin>,
        Option<ChildStderr>,
        Child,
    ) {
        let reader = LspReader::new(self.stdout);
        let writer = LspWriter::new(self.stdin);
        let stderr = self.stderr.take();
        (reader, writer, stderr, self.child)
    }

    /// Force-kill the server process. The actor's normal
    /// shutdown path runs `shutdown` + `exit` LSP requests
    /// instead; this is the supervision-layer fallback when the
    /// server hangs.
    pub async fn kill(mut self) -> Result<(), TransportError> {
        self.child.start_kill().map_err(TransportError::Lifecycle)?;
        self.child
            .wait()
            .await
            .map_err(TransportError::Lifecycle)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `Message`, `Notification`, and `serde_json::json!` are only
    // used by the `#[cfg(unix)]`-gated round-trip test below
    // (spawns `cat`); gate the imports the same way so Windows
    // builds (which skip the test) don't flag them as unused.
    #[cfg(unix)]
    use crate::jsonrpc::{Message, Notification};
    #[cfg(unix)]
    use serde_json::json;

    /// Spawning a non-existent binary surfaces `TransportError::Spawn`.
    /// Cross-platform friendly: doesn't depend on shell semantics.
    #[tokio::test]
    async fn spawn_missing_binary_is_error() {
        let err = ChildTransport::spawn(
            "lattice-lsp-fixture-this-does-not-exist-x7q",
            std::iter::empty::<&str>(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, TransportError::Spawn { .. }));
    }

    /// Spawn `cat` (POSIX) as a stand-in echo server: the LSP
    /// codec writes a frame to stdin, `cat` mirrors it to
    /// stdout, the codec reads it back. Validates the
    /// stdin → stdout round-trip with no LSP-specific server
    /// behaviour.
    #[cfg(unix)]
    #[tokio::test]
    async fn cat_echoes_one_message() {
        let t = ChildTransport::spawn("cat", std::iter::empty::<&str>(), None)
            .await
            .unwrap();
        let (mut reader, mut writer, _stderr, mut child) = t.split();

        let n = Message::Notification(Notification::new(
            "telemetry/event",
            Some(json!({"k": "v"})),
        ));
        writer.write_message(&n).await.unwrap();
        // Close stdin so cat sees EOF and exits; otherwise the
        // child stays alive forever waiting for more input.
        drop(writer);

        let got = reader.read_message().await.unwrap().unwrap();
        match got {
            Message::Notification(n) => assert_eq!(n.method, "telemetry/event"),
            _ => panic!("expected echoed notification"),
        }
        // After cat sees EOF, it exits; verify clean stream end.
        assert!(reader.read_message().await.unwrap().is_none());
        let status = child.wait().await.unwrap();
        assert!(status.success());
    }

    /// PID is exposed while the child is alive.
    #[cfg(unix)]
    #[tokio::test]
    async fn pid_is_exposed() {
        let t = ChildTransport::spawn("cat", std::iter::empty::<&str>(), None)
            .await
            .unwrap();
        assert!(t.pid().is_some());
        t.kill().await.unwrap();
    }
}
