//! PtyHandle — writer + resize for the master PTY side.
//! Cheaply clonable (Arc-backed); fire-and-forget semantics
//! so host code can write keystrokes synchronously without
//! await.

use std::io::Write;
use std::sync::Arc;

use parking_lot::Mutex;
use portable_pty::{MasterPty, PtySize};
use thiserror::Error;

/// Errors returned by [`PtyHandle`] operations. Wrap stdlib /
/// portable-pty errors so consumers don't need to depend on
/// portable-pty directly.
#[derive(Debug, Error)]
pub enum PtyHandleError {
    #[error("pty write failed: {0}")]
    Write(#[source] std::io::Error),
    #[error("pty resize failed: {0}")]
    Resize(String),
    #[error("pty handle dropped (process already exited)")]
    Closed,
}

/// Inner state behind the Arc. Keeps the master PTY + the
/// writer handle alive together.
struct PtyHandleInner {
    /// Boxed-trait so different platforms' MasterPty impls
    /// all fit. Held to keep the PTY alive (close on drop).
    master: Mutex<Box<dyn MasterPty + Send>>,
    /// Pre-extracted writer — `take_writer` consumes from the
    /// master at spawn time. parking_lot::Mutex keeps the
    /// write path lock-free of a tokio runtime; the OS pipe
    /// is the actual backpressure mechanism.
    writer: Mutex<Box<dyn Write + Send>>,
    /// Stashed last-known size; used to detect no-op resizes.
    last_size: Mutex<(u16, u16)>,
}

/// Cheap-to-clone handle. All clones share the same master
/// PTY + writer.
#[derive(Clone)]
pub struct PtyHandle {
    inner: Arc<PtyHandleInner>,
}

impl PtyHandle {
    /// Construct from the post-spawn pieces. Called by
    /// [`crate::spawner::spawn`] only.
    pub(crate) fn new(
        master: Box<dyn MasterPty + Send>,
        writer: Box<dyn Write + Send>,
        rows: u16,
        cols: u16,
    ) -> Self {
        Self {
            inner: Arc::new(PtyHandleInner {
                master: Mutex::new(master),
                writer: Mutex::new(writer),
                last_size: Mutex::new((rows, cols)),
            }),
        }
    }

    /// Write bytes to the PTY's stdin. Fire-and-forget — the
    /// OS buffers; backpressure surfaces as an `Err` only on
    /// catastrophic failure (broken pipe, etc.).
    pub fn write(&self, bytes: &[u8]) -> Result<(), PtyHandleError> {
        let mut w = self.inner.writer.lock();
        w.write_all(bytes).map_err(PtyHandleError::Write)?;
        // Flush so the shell sees the bytes immediately —
        // line-buffered stdin defeats interactive use.
        w.flush().map_err(PtyHandleError::Write)?;
        Ok(())
    }

    /// Resize the PTY. The child receives SIGWINCH; programs
    /// that subscribe (vim, less, htop) re-layout.
    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), PtyHandleError> {
        {
            let last = self.inner.last_size.lock();
            if *last == (rows, cols) {
                return Ok(());
            }
        }
        self.inner
            .master
            .lock()
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| PtyHandleError::Resize(e.to_string()))?;
        *self.inner.last_size.lock() = (rows, cols);
        Ok(())
    }

    /// Current best-known size.
    pub fn size(&self) -> (u16, u16) {
        *self.inner.last_size.lock()
    }
}
