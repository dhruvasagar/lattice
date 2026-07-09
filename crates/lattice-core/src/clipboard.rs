//! System-clipboard abstraction (CB.0).
//!
//! The editor talks to the OS clipboard through this single trait so the
//! backend (native `arboard`, OSC52 terminal escape, gpui-native, or an
//! in-memory fake for tests) is swappable and renderer-neutral. Registered in
//! the `ServiceRegistry` at boot as [`ClipboardHandle`] so both the host's
//! register layer AND mode crates (`terminal-mode`, which routes paste to the
//! PTY — CB.3) can reach it without depending on `lattice-host`.
//!
//! **Threading contract (paramount #1).** Clipboard I/O is an OS round-trip
//! (X11 / Wayland can be slow); it must never sit on the UI / keystroke path.
//! [`Clipboard::read`] is only ever called from a `spawn_blocking` context (the
//! paste path); [`Clipboard::write`] is fire-and-forget — a backend that would
//! block spawns its own blocking task internally and returns immediately.
//!
//! See `docs/dev/architecture/clipboard.md`.

use std::sync::Arc;

/// The OS clipboard, behind a swappable backend.
///
/// Implementors: `ArboardClipboard` + OSC52 fallback (TUI, CB.2), the
/// gpui-native bridge (GPUI peer, CB.4), and [`FakeClipboard`] (tests / CI).
/// All reads/writes are text-only for v1 (images / other MIME are out of
/// scope).
pub trait Clipboard: Send + Sync {
    /// Read the clipboard's current text. `None` when the clipboard is empty,
    /// holds non-text content, or the backend can't read (e.g. OSC52 over SSH,
    /// where read-back is unsupported — the register layer falls back to its
    /// in-memory entry). Only called from a `spawn_blocking` context.
    fn read(&self) -> Option<String>;

    /// Write `text` to the clipboard. Fire-and-forget: never blocks the
    /// caller. A backend whose write would block spawns its own blocking task.
    fn write(&self, text: String);
}

/// Cheap-clone handle for the clipboard service, stored in the
/// `ServiceRegistry`. Per the ServiceRegistry Arc/TypeId rule, register AND
/// look up under this exact type (`services.get::<ClipboardHandle>()`), never
/// under a concrete backend type.
pub type ClipboardHandle = Arc<dyn Clipboard>;

/// In-memory [`Clipboard`] for tests / headless CI and the default boot
/// binding before a real backend is installed (CB.2 / CB.4). Thread-safe;
/// round-trips text without touching any OS resource.
#[derive(Debug, Default)]
pub struct FakeClipboard {
    inner: std::sync::Mutex<Option<String>>,
}

impl FakeClipboard {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Clipboard for FakeClipboard {
    fn read(&self) -> Option<String> {
        self.inner.lock().expect("clipboard mutex poisoned").clone()
    }

    fn write(&self, text: String) {
        *self.inner.lock().expect("clipboard mutex poisoned") = Some(text);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn fake_clipboard_roundtrips_text() {
        let cb = FakeClipboard::new();
        assert_eq!(cb.read(), None, "empty clipboard reads None");
        cb.write("hello".to_string());
        assert_eq!(cb.read(), Some("hello".to_string()));
        cb.write("world".to_string());
        assert_eq!(cb.read(), Some("world".to_string()), "write overwrites");
    }

    #[test]
    fn fake_clipboard_is_usable_as_handle() {
        // Exercises the object-safe `dyn Clipboard` path the ServiceRegistry
        // stores (`ClipboardHandle`).
        let handle: ClipboardHandle = Arc::new(FakeClipboard::new());
        handle.write("via handle".to_string());
        assert_eq!(handle.read(), Some("via handle".to_string()));
    }
}
