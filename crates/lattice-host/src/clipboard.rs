//! CB.4 (`docs/dev/architecture/clipboard.md` §4): the native `arboard`
//! clipboard backend, **shared by both renderer peers**.
//!
//! The [`lattice_core::Clipboard`] trait requires `Send + Sync` and its
//! `read` is called synchronously on the editor actor thread (from
//! `Editor::read_register`). `arboard` satisfies `Send + Sync` and reads
//! synchronously, so a single backend serves both the TUI and GPUI peers —
//! this module is the shared home so the load-bearing bounded-read logic
//! (paramount #1) isn't duplicated and can't drift between peers.
//!
//! Why here (`lattice-host`) and not `lattice-core` (where the trait lives):
//! the bounded read needs `tokio` + `lattice_runtime::block_on`, which
//! `lattice-core` (a leaf crate) doesn't depend on. `lattice-host` has both
//! and is depended on by both peers. Compiled only behind the
//! `system-clipboard` feature so host's default build stays free of the
//! X11/Wayland link libs `arboard` pulls on Linux.
//!
//! The GPUI peer uses this directly (it always links display libs, so
//! `arboard` is always available in a GUI build). The TUI peer composes it
//! with its own OSC52 write-only fallback (`lattice-ui-tui`'s
//! `clipboard.rs`) for headless / SSH sessions where there's no display.

use lattice_core::Clipboard;

/// Native OS clipboard via `arboard` (macOS / X11 / Wayland / Windows).
/// Full read + write.
pub struct ArboardClipboard {
    inner: std::sync::Arc<std::sync::Mutex<arboard::Clipboard>>,
}

impl ArboardClipboard {
    /// `None` when no display is reachable (headless, or the display
    /// server refused the connection) — the caller falls back (OSC52 for
    /// the TUI, the in-memory register otherwise).
    pub fn new() -> Option<Self> {
        arboard::Clipboard::new().ok().map(|inner| Self {
            inner: std::sync::Arc::new(std::sync::Mutex::new(inner)),
        })
    }

    /// Bounded wait so a hung display-server round-trip can't stall
    /// dispatch (paramount #1). `Editor::read_register`'s call sits on the
    /// blocking render→actor RPC (`input-pipeline.md`); this bound is what
    /// keeps the synchronous read contract honest for a real backend. 30ms
    /// is generous headroom over the common case (a local
    /// X11/Wayland/macOS clipboard round-trip is sub-5ms) while still
    /// bounding the worst case.
    const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(30);
}

impl Clipboard for ArboardClipboard {
    fn read(&self) -> Option<String> {
        let clipboard = self.inner.clone();
        let fut = async move {
            tokio::time::timeout(
                Self::READ_TIMEOUT,
                tokio::task::spawn_blocking(move || clipboard.lock().ok()?.get_text().ok()),
            )
            .await
        };
        match lattice_runtime::block_on(fut) {
            Ok(Ok(text)) => text,
            // Timed out, the blocking task panicked, or the mutex was
            // poisoned -- any of these degrade to "no clipboard value",
            // never a panic on the hot path.
            _ => None,
        }
    }

    fn write(&self, text: String) {
        // True fire-and-forget: schedule onto the shared runtime and
        // return immediately, no bound needed (nothing awaits the result).
        let clipboard = self.inner.clone();
        lattice_runtime::spawn_task(async move {
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(mut cb) = clipboard.lock() {
                    let _ = cb.set_text(text);
                }
            })
            .await;
        });
    }
}
