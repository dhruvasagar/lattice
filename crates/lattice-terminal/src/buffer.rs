use std::{path::PathBuf, sync::Arc};

use arc_swap::ArcSwap;
use lattice_core::BufferId;

use crate::{PtyHandle, TerminalSnapshot};

/// PTY-backed terminal buffer entry held in the host's
/// buffer registry. Owns the writer handle, the published
/// snapshot cell, and the reader task's `AbortHandle` so
/// dropping the buffer kills the reader (and, transitively,
/// the child via PTY close on SIGHUP).
///
/// Construct via [`TerminalBuffer::from_spawn`] — the host
/// never names the internal field shape directly so adding
/// fields stays non-breaking.
#[derive(Debug)]
pub struct TerminalBuffer {
    pub id: BufferId,
    pub pty: Arc<PtyHandle>,
    pub cwd: Option<PathBuf>,
    pub label: String,
    pub snapshot: Arc<ArcSwap<TerminalSnapshot>>,
    pub created_at: std::time::SystemTime,
    /// Abort handle for the reader task. Held to keep the task
    /// linked to the buffer's lifetime; on Drop the abort fires
    /// so a removed terminal stops draining its PTY.
    reader_abort: tokio::task::AbortHandle,
}

impl Drop for TerminalBuffer {
    fn drop(&mut self) {
        self.reader_abort.abort();
    }
}

pub struct ScrollbackView {
    // TODO: add lifetime if exposing references
    pub total_rows: u32,
    /// 0 = bottom (live); N = N rows up
    pub viewport_row: u32,
}

impl TerminalBuffer {
    /// Build a buffer entry from freshly-spawned PTY handles +
    /// the host-assigned identity. Centralises the
    /// `TerminalBuffer` field list so the host stays insulated
    /// from substrate-internal field changes.
    pub fn from_spawn(
        id: BufferId,
        label: String,
        cwd: Option<PathBuf>,
        handles: crate::spawner::SpawnHandles,
    ) -> Self {
        let crate::spawner::SpawnHandles {
            pty,
            snapshot,
            reader_task,
        } = handles;
        Self {
            id,
            pty: Arc::new(pty),
            cwd,
            label,
            snapshot,
            created_at: std::time::SystemTime::now(),
            reader_abort: reader_task.abort_handle(),
        }
    }

    pub fn scrollback_view(&self) -> ScrollbackView {
        // T1 stub: scrollback not yet implemented
        ScrollbackView {
            total_rows: 0,
            viewport_row: 0,
        }
    }
}
