//! Run a child process that a [`CancellationToken`] can stop (CG.6).
//!
//! Design: `docs/dev/architecture/cancellation.md` §9.3.
//!
//! `Command::output()` blocks until the child exits and hands back no
//! handle, so work started that way — git, cargo, grep, an external
//! formatter — cannot be stopped by anything: a superseded push runs to
//! the end, and so does one the user pressed `<C-g>` on. [`run_cancellable`]
//! is `output()` with a way out.
//!
//! Three things make the way out real rather than nominal:
//!
//! - **The child can lead its own process group** ([`ProcessGroup::Own`]),
//!   and a cancel then kills the whole group. A shell's grandchildren
//!   otherwise outlive it and keep its pipes open, which is exactly how
//!   compilation's re-run came to stream an old run's output into a new
//!   one. [`ProcessGroup::Shared`] is for a child that may need the
//!   terminal — see its docs.
//! - **Output is drained on reader threads**, so a child that fills a pipe
//!   cannot wedge the wait loop that is watching the token; and a cancel
//!   does not wait for them, so a grandchild still holding a pipe cannot
//!   delay it either.
//! - **The token is polled**, every [`POLL`], from the thread that waits.
//!   That thread is the caller's — a blocking-pool thread in practice —
//!   so nothing new is spawned to watch.
//!
//! The kill goes through `kill(1)` / `taskkill`, not `libc::kill`: the
//! workspace denies `unsafe`, and one short-lived process per cancel is a
//! fair price for keeping it that way (the plugin host's process job made
//! the same trade first).

use std::io::{self, Read, Write};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::Duration;

use lattice_protocol::CancellationToken;

/// How often a running child's token is checked. A cancel lands within
/// this; the cost is one atomic load and one `try_wait` per tick on a
/// thread that would otherwise be blocked anyway.
pub const POLL: Duration = Duration::from_millis(10);

/// Whether the child gets a process group of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessGroup {
    /// The child leads a new process group, and a cancel kills all of it —
    /// the shell and everything the shell started. The default for work
    /// that never talks to the terminal: builds, compilation, formatters.
    Own,
    /// The child stays in the editor's process group, and a cancel kills
    /// the child alone. For a child whose descendants may prompt on the
    /// terminal lattice was started from: `ssh` under `git push` asks for
    /// a key's passphrase there, and from a group of its own that read
    /// would suspend it (`SIGTTIN`) — a push that hangs silently instead
    /// of asking. A grandchild left behind by a cancel ends on its own
    /// (an `ssh` once its prompt is answered or its git is gone).
    Shared,
}

/// How [`run_cancellable`] ended.
#[derive(Debug)]
pub enum RunOutcome {
    /// The child exited on its own. Its status may still be a failure —
    /// that is the caller's to read, exactly as with `Command::output()`.
    Finished(Output),
    /// The token was cancelled first, and the child's process group was
    /// killed. Whatever it printed is discarded: a cancelled run has no
    /// result to report.
    Cancelled,
}

/// Run `command` to completion, or until `token` is cancelled; on cancel,
/// `group` decides what is killed.
///
/// `stdout` and `stderr` are captured, as `Command::output()` does.
/// `stdin` is `input` when given — written on its own thread, so a child
/// that only reads after writing cannot deadlock against it — and closed
/// otherwise: nothing here can answer a prompt, and a child waiting on one
/// would never finish.
///
/// A token cancelled before the call returns [`RunOutcome::Cancelled`]
/// without starting anything.
///
/// # Errors
///
/// When the child cannot be started, or waiting on it fails — the same
/// conditions `Command::output()` reports.
///
/// # Examples
///
/// ```
/// # #[cfg(unix)] {
/// use std::process::Command;
/// use lattice_core::process::{run_cancellable, ProcessGroup, RunOutcome};
/// use lattice_protocol::CancellationToken;
///
/// let mut echo = Command::new("echo");
/// echo.arg("hi");
/// match run_cancellable(echo, None, ProcessGroup::Own, &CancellationToken::new()).unwrap() {
///     RunOutcome::Finished(out) => assert_eq!(out.stdout, b"hi\n"),
///     RunOutcome::Cancelled => unreachable!("nothing cancelled it"),
/// }
/// # }
/// ```
pub fn run_cancellable(
    mut command: Command,
    input: Option<Vec<u8>>,
    group: ProcessGroup,
    token: &CancellationToken,
) -> io::Result<RunOutcome> {
    if token.is_cancelled() {
        return Ok(RunOutcome::Cancelled);
    }
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if group == ProcessGroup::Own {
        own_process_group(&mut command);
    }
    let mut child = command.spawn()?;

    if let (Some(bytes), Some(mut stdin)) = (input, child.stdin.take()) {
        // A write error means the child closed its end early, which is its
        // business — the exit status says how it went.
        thread::spawn(move || {
            let _ = stdin.write_all(&bytes);
        });
    }
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    loop {
        if token.is_cancelled() {
            match group {
                ProcessGroup::Own => kill_tree(&mut child),
                ProcessGroup::Shared => {
                    let _ = child.kill();
                }
            }
            let _ = child.wait();
            // The readers are NOT joined: a grandchild the kill did not
            // reach may still hold a pipe, and the cancel must not wait on
            // it. They end, and drop what they read, when the pipes close.
            drop((stdout, stderr));
            return Ok(RunOutcome::Cancelled);
        }
        if let Some(status) = child.try_wait()? {
            return Ok(RunOutcome::Finished(Output {
                status,
                stdout: stdout.join().unwrap_or_default(),
                stderr: stderr.join().unwrap_or_default(),
            }));
        }
        thread::sleep(POLL);
    }
}

/// Read a pipe to its end on its own thread.
fn drain(pipe: Option<impl Read + Send + 'static>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    })
}

/// Start the child as the leader of its own process group, so the whole
/// tree can be killed at once.
#[cfg(unix)]
fn own_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

#[cfg(not(unix))]
fn own_process_group(_command: &mut Command) {}

/// Kill the child and everything it started. The direct `Child::kill`
/// afterwards covers a sandbox with no `kill` / `taskkill` on `PATH`.
fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    let tree = Command::new("kill")
        .args(["-KILL", "--", &format!("-{}", child.id())])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    #[cfg(windows)]
    let tree = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &child.id().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    #[cfg(any(unix, windows))]
    if let Err(error) = tree {
        tracing::debug!(%error, "process-tree kill unavailable; killing the child alone");
    }
    let _ = child.kill();
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use std::time::Instant;

    fn sh(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.args(["-c", script]);
        c
    }

    #[test]
    fn a_finished_run_reports_what_output_would() {
        let RunOutcome::Finished(out) = run_cancellable(
            sh("printf out; printf err >&2; exit 3"),
            None,
            ProcessGroup::Own,
            &CancellationToken::new(),
        )
        .unwrap() else {
            panic!("nothing cancelled it");
        };
        assert_eq!(out.stdout, b"out");
        assert_eq!(out.stderr, b"err");
        assert_eq!(out.status.code(), Some(3));
    }

    #[test]
    fn input_reaches_the_childs_stdin() {
        let RunOutcome::Finished(out) = run_cancellable(
            Command::new("cat"),
            Some(b"piped".to_vec()),
            ProcessGroup::Own,
            &CancellationToken::new(),
        )
        .unwrap() else {
            panic!("nothing cancelled it");
        };
        assert_eq!(out.stdout, b"piped");
    }

    #[test]
    fn a_cancelled_token_starts_nothing() {
        let token = CancellationToken::new();
        token.cancel();
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran");
        let outcome = run_cancellable(
            sh(&format!("touch {}", marker.display())),
            None,
            ProcessGroup::Own,
            &token,
        )
        .unwrap();
        assert!(matches!(outcome, RunOutcome::Cancelled));
        assert!(!marker.exists());
    }

    /// The case compilation got wrong: the shell is killed AND the
    /// grandchild it started, so nothing is left holding the pipes — and
    /// the call returns promptly instead of waiting out the sleep.
    #[test]
    fn a_cancel_kills_the_shell_and_its_grandchild() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let token = CancellationToken::new();
        let canceller = token.clone();
        let pidfile_seen = pidfile.clone();
        // Cancel once the grandchild's pid is written, not merely once the
        // file exists — the shell creates it before it writes.
        let cancel = thread::spawn(move || {
            while std::fs::read_to_string(&pidfile_seen).map_or(true, |s| !s.ends_with('\n')) {
                thread::sleep(Duration::from_millis(5));
            }
            canceller.cancel();
        });
        let started = Instant::now();
        let outcome = run_cancellable(
            sh(&format!("sleep 30 & echo $! > {}; wait", pidfile.display())),
            None,
            ProcessGroup::Own,
            &token,
        )
        .unwrap();
        cancel.join().unwrap();
        assert!(matches!(outcome, RunOutcome::Cancelled));
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "returned promptly"
        );

        let grandchild = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .to_string();
        // `kill -0` succeeds while the process exists. A killed orphan can
        // linger a moment as a zombie until init reaps it, so allow that.
        let alive = || {
            Command::new("kill")
                .args(["-0", &grandchild])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(), "the shell's `sleep` died with it");
    }

    /// `Shared`: the child alone is killed — a grandchild that may be
    /// prompting on the terminal is left to finish — and the cancel still
    /// returns at once, though that grandchild holds the output pipes.
    #[test]
    fn a_shared_group_cancel_returns_without_waiting_on_a_grandchild() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let token = CancellationToken::new();
        let canceller = token.clone();
        let pidfile_seen = pidfile.clone();
        let cancel = thread::spawn(move || {
            while std::fs::read_to_string(&pidfile_seen).map_or(true, |s| !s.ends_with('\n')) {
                thread::sleep(Duration::from_millis(5));
            }
            canceller.cancel();
        });
        let started = Instant::now();
        let outcome = run_cancellable(
            sh(&format!("sleep 30 & echo $! > {}; wait", pidfile.display())),
            None,
            ProcessGroup::Shared,
            &token,
        )
        .unwrap();
        cancel.join().unwrap();
        assert!(matches!(outcome, RunOutcome::Cancelled));
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the cancel does not wait for the grandchild's pipes"
        );

        let grandchild = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .to_string();
        let alive = Command::new("kill")
            .args(["-0", &grandchild])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        // Clean up before asserting, so a failure leaves nothing behind.
        let _ = Command::new("kill").args(["-KILL", &grandchild]).status();
        assert!(
            alive,
            "Shared kills the child alone; the grandchild is left to finish"
        );
    }
}
