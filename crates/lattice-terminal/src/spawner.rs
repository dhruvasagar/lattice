//! `spawn` — fork a child process under a fresh pseudo-tty.
//! Returns the [`PtyHandle`] (writer + resize) and the
//! published `Arc<ArcSwap<TerminalSnapshot>>` cell the
//! renderer reads from.
//!
//! T1 (2026-05-22): the reader task is a "drain bytes, build
//! a naive snapshot" stub (no full VT/xterm parsing yet).
//! T2 swaps in `alacritty_terminal::Term`; the crate's
//! published interface is unchanged.

use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwap;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use thiserror::Error;

use crate::handle::PtyHandle;
use crate::reader::{SharedTerm, spawn_reader};
use crate::snapshot::TerminalSnapshot;

/// Inputs to [`spawn`].
#[derive(Debug, Clone)]
pub struct SpawnConfig {
    /// Path of the program to exec (e.g. `/usr/bin/zsh`,
    /// `/bin/sh`, `cargo`).
    pub program: String,
    /// Arguments for the program (excluding argv[0]).
    pub args: Vec<String>,
    /// Spawn working directory. `None` = inherit parent's cwd.
    pub cwd: Option<PathBuf>,
    /// Initial PTY size (rows, cols).
    pub rows: u16,
    pub cols: u16,
    /// T3 (2026-05-25): scrollback ring capacity in lines. `0`
    /// disables scrollback. Caller resolves the
    /// `terminal.scrollback-lines` typed option and passes the
    /// result here.
    pub scrollback_lines: u32,
    /// Optional repaint notifier — fired by the reader task
    /// after every published snapshot. Event-driven renderers
    /// (GPUI) need this wake to know terminal output has
    /// arrived; per-tick renderers (TUI) observe the publish
    /// on their next tick and don't strictly need it. Wired by
    /// the host from `Editor::paint_request` so terminal output
    /// drives the same bridge the highlights worker uses.
    pub paint_request: Option<Arc<tokio::sync::Notify>>,
}

#[derive(Debug, Error)]
pub enum SpawnError {
    #[error("pty open failed: {0}")]
    OpenPty(String),
    #[error("child spawn failed: {0}")]
    SpawnChild(String),
    #[error("take writer failed: {0}")]
    TakeWriter(String),
    #[error("clone reader failed: {0}")]
    CloneReader(String),
}

/// Successful spawn handles.
pub struct SpawnHandles {
    pub pty: PtyHandle,
    pub snapshot: Arc<ArcSwap<TerminalSnapshot>>,
    /// T3 (2026-05-25): shared handle to the alacritty `Term`
    /// the reader task drives. Dispatch-side actions (scroll,
    /// resize) lock the inner Mutex; the reader holds an Arc to
    /// the same `Term`. Cheap to clone.
    pub term: SharedTerm,
    /// 2026-05-25: handle to the spawned child the
    /// `TerminalBuffer::Drop` uses to force the shell to exit
    /// on buffer teardown. Without this, the master-side
    /// reader fd (cloned via `try_clone_reader`) keeps the
    /// PTY open even after `PtyHandle` drops — the child
    /// never sees SIGHUP, the reader's blocking `read()`
    /// never returns, and the editor freezes on `:q`. Calling
    /// `killer.kill()` sends SIGKILL to the child; once it
    /// exits, the reader's `read()` returns 0 (EOF), the
    /// spawn_blocking task exits cleanly.
    pub child_killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
    /// 2026-05-25: child PID captured at spawn so Drop can
    /// follow the portable_pty SIGHUP with a SIGKILL fallback
    /// (`libc::kill(pid, SIGKILL)` on Unix). Some shells /
    /// environments don't exit on SIGHUP reliably; SIGKILL is
    /// the guaranteed wake for the reader's blocking read().
    /// `None` when the platform doesn't expose the PID (Windows
    /// ConPTY).
    pub child_pid: Option<u32>,
}

/// Spawn a child under a PTY. Returns:
/// - `PtyHandle` for writing keystrokes + resizing.
/// - `Arc<ArcSwap<TerminalSnapshot>>` for the renderer to
///   `.load()` each frame.
/// - Reader task handle (aborted when the caller drops it).
pub fn spawn(config: SpawnConfig) -> Result<SpawnHandles, SpawnError> {
    let SpawnConfig {
        program,
        args,
        cwd,
        rows,
        cols,
        scrollback_lines,
        paint_request,
    } = config;

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| SpawnError::OpenPty(e.to_string()))?;

    let mut cmd = CommandBuilder::new(&program);
    for arg in &args {
        cmd.arg(arg);
    }
    if let Some(cwd_path) = cwd {
        cmd.cwd(cwd_path);
    }

    // Spawn the child on the slave side of the pair.
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| SpawnError::SpawnChild(e.to_string()))?;
    // 2026-05-25: capture the killer + PID BEFORE dropping the
    // Child handle so TerminalBuffer::Drop can force the shell
    // to exit on `:q` / `:bd!`. The reader-side fd (cloned via
    // `try_clone_reader` below) keeps the master PTY open
    // even after `PtyHandle` drops, so closing the master is
    // not enough to wake the reader's blocking `read()` — the
    // child has to actually exit. portable-pty's `kill()`
    // sends SIGHUP on Unix, which most shells respect, but
    // not all environments deliver it reliably (WSL2 quirks,
    // captured stty, child has SIGHUP trap). Drop sends both:
    // the portable_pty SIGHUP via the cloned killer AND a
    // libc::kill(pid, SIGKILL) as the guaranteed fallback.
    let child_killer = child.clone_killer();
    let child_pid = child.process_id();
    drop(child);
    // Drop the slave on the parent side immediately after
    // spawn — the child inherited its own copy via dup2.
    drop(pair.slave);

    // Pull the writer + reader sides out of the master.
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| SpawnError::TakeWriter(e.to_string()))?;
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| SpawnError::CloneReader(e.to_string()))?;

    let handle = PtyHandle::new(pair.master, writer, rows, cols);
    let snapshot = Arc::new(ArcSwap::from_pointee(TerminalSnapshot::empty()));
    let term = spawn_reader(
        reader,
        Arc::clone(&snapshot),
        rows,
        cols,
        scrollback_lines,
        paint_request,
    );

    Ok(SpawnHandles {
        pty: handle,
        snapshot,
        term,
        child_killer,
        child_pid,
    })
}
