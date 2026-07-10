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

/// The master PTY writer, shared between the **input path**
/// ([`PtyHandle::write`], user keystrokes) and the terminal's
/// **VT-query responder** (the reader thread writing DSR / DA /
/// DECRQM / colour replies back so query-driven TUIs render —
/// see `reader::PtyResponder`). One shared `Mutex` serialises
/// the two writers so their bytes never interleave mid-sequence.
pub(crate) type SharedPtyWriter = Arc<Mutex<Box<dyn Write + Send>>>;

/// Inner state behind the Arc. Keeps the master PTY + the
/// writer handle alive together.
struct PtyHandleInner {
    /// Boxed-trait so different platforms' MasterPty impls
    /// all fit. Held to keep the PTY alive (close on drop).
    master: Mutex<Box<dyn MasterPty + Send>>,
    /// Pre-extracted writer — `take_writer` consumes from the
    /// master at spawn time. Shared with the reader thread's
    /// VT-query responder via [`SharedPtyWriter`]. parking_lot's
    /// Mutex keeps the write path lock-free of a tokio runtime;
    /// the OS pipe is the actual backpressure mechanism.
    writer: SharedPtyWriter,
    /// Stashed last-known size; used to detect no-op resizes.
    last_size: Mutex<(u16, u16)>,
}

/// Cheap-to-clone handle. All clones share the same master
/// PTY + writer.
#[derive(Clone)]
pub struct PtyHandle {
    inner: Arc<PtyHandleInner>,
}

impl std::fmt::Debug for PtyHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (rows, cols) = self.size();
        f.debug_struct("PtyHandle")
            .field("rows", &rows)
            .field("cols", &cols)
            .finish()
    }
}

impl PtyHandle {
    /// Construct from the post-spawn pieces. Called by
    /// [`crate::spawner::spawn`] only.
    pub(crate) fn new(
        master: Box<dyn MasterPty + Send>,
        writer: SharedPtyWriter,
        rows: u16,
        cols: u16,
    ) -> Self {
        Self {
            inner: Arc::new(PtyHandleInner {
                master: Mutex::new(master),
                writer,
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

    // /// Close the PTY master (child sees SIGHUP); dropping the
    // /// FD typically terminates the subprocess.
    // pub fn kill(&self) -> Result<(), PtyHandleError> {
    //     // 1) Flush any pending writes
    //     if let Err(e) = self.inner.writer.lock().flush() {
    //         return Err(PtyHandleError::Write(e));
    //     }

    //     // 2) Dropping the master FD by replacing it with a no-op
    //     //    will close the PTY. Child should exit on SIGHUP.
    //     //    If you need SIGKILL, you must store the Child handle.
    //     let mut master = self.inner.master.lock();
    //     // Replace with an empty dummy so the old MasterPty is dropped:
    //     *master = {
    //         use portable_pty::PtySize;
    //         // Create a closed pseudo-pty master as a no-op stub
    //         let dummy = Box::new(
    //             portable_pty::native_pty_system()
    //                 .openpty(PtySize {
    //                     rows: 0,
    //                     cols: 0,
    //                     pixel_width: 0,
    //                     pixel_height: 0,
    //                 })
    //                 .unwrap()
    //                 .master,
    //         );
    //         dummy
    //     };
    //     Ok(())
    // }
}
