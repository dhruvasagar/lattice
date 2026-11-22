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
//! (X11 / Wayland can be slow, a Windows helper under WSL slower still).
//! [`Clipboard::try_read`] is called synchronously on the editor actor by an
//! explicit paste, because the paste must land before the next key runs
//! (`.` repeat, macros and typed-ahead keys all depend on that order); each
//! backend bounds its own wait and reports a read that ran out of time as
//! [`ClipboardRead::Failed`]. [`Clipboard::write`] is fire-and-forget — a
//! backend that would block hands the write to its own task or thread and
//! returns immediately.
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
    /// Read the clipboard's current text, saying why when there is none:
    /// [`ClipboardRead::NoText`] lets the register layer fall back to its
    /// in-memory entry, while [`ClipboardRead::Failed`] is reported to the
    /// user (a paste of the *wrong* text is worse than a paste that says it
    /// could not read). Bounded by the backend; see the module doc.
    fn try_read(&self) -> ClipboardRead;

    /// [`Self::try_read`]'s text, for a caller that treats "nothing" and
    /// "could not read" alike.
    fn read(&self) -> Option<String> {
        self.try_read().into_text()
    }

    /// Write `text` to the clipboard. Fire-and-forget: never blocks the
    /// caller. A backend whose write would block spawns its own blocking task.
    fn write(&self, text: String);
}

/// What a [`Clipboard::try_read`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardRead {
    /// The clipboard holds this text.
    Text(String),
    /// Nothing to paste: the clipboard is empty or holds something other than
    /// text, or the backend cannot read at all (OSC52, whose query half
    /// terminals refuse). The register layer uses its in-memory entry.
    NoText,
    /// The read was tried and did not succeed — the backend did not answer in
    /// time, or its helper failed. The reason is written for the user.
    Failed(String),
}

impl ClipboardRead {
    /// The text, if the read found some.
    pub fn into_text(self) -> Option<String> {
        match self {
            Self::Text(text) => Some(text),
            Self::NoText | Self::Failed(_) => None,
        }
    }
}

/// Cheap-clone handle for the clipboard service, stored in the
/// `ServiceRegistry`. Per the ServiceRegistry Arc/TypeId rule, register AND
/// look up under this exact type (`services.get::<ClipboardHandle>()`), never
/// under a concrete backend type.
pub type ClipboardHandle = Arc<dyn Clipboard>;

/// In-memory [`Clipboard`] for tests / headless CI and the default boot
/// binding before a real backend is installed (CB.2 / CB.4). Thread-safe;
/// round-trips text without touching any OS resource.
///
/// # Examples
///
/// ```
/// use lattice_core::{Clipboard, ClipboardHandle, FakeClipboard};
/// use std::sync::Arc;
///
/// let clip: ClipboardHandle = Arc::new(FakeClipboard::new());
/// assert_eq!(clip.read(), None); // starts empty
/// clip.write("yanked".to_string());
/// assert_eq!(clip.read().as_deref(), Some("yanked"));
/// ```
#[derive(Debug, Default)]
pub struct FakeClipboard {
    inner: std::sync::Mutex<Option<String>>,
}

impl FakeClipboard {
    /// An empty clipboard: [`Clipboard::read`] returns `None` until the
    /// first write.
    pub fn new() -> Self {
        Self::default()
    }
}

impl Clipboard for FakeClipboard {
    fn try_read(&self) -> ClipboardRead {
        match self.inner.lock().expect("clipboard mutex poisoned").clone() {
            Some(text) => ClipboardRead::Text(text),
            None => ClipboardRead::NoText,
        }
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
