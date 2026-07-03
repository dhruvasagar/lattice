//! CB.2 (`docs/dev/architecture/clipboard.md` §4): the TUI's
//! [`lattice_core::Clipboard`] backend. Two implementations composed by
//! [`TuiClipboard::detect`]:
//!
//! - [`Osc52Clipboard`] — terminal escape-sequence write. No link deps,
//!   always compiled in. Read-back is unsupported (most terminals block the
//!   OSC52 read half for security reasons), so `read` always returns
//!   `None` and callers fall back to the in-memory register.
//! - [`ArboardClipboard`] (behind the `system-clipboard` feature) — native
//!   OS clipboard (macOS / X11 / Wayland / Windows) via `arboard`. Full
//!   read + write, but pulls X11/Wayland link libs on Linux, so it's
//!   feature-gated (mirrors `lattice-ui-gpui`'s `window` optional-dep
//!   pattern) — the default `cargo test --workspace` CI job doesn't
//!   install those libs.
//!
//! `TuiClipboard::detect()` prefers OSC52 under SSH even when `arboard`
//! would technically succeed: over `ssh -X`, `arboard` can connect to the
//! *forwarded* X server, but writes there land in the remote X server's
//! clipboard, not the user's local machine clipboard. OSC52 tunnels through
//! the terminal escape codes to the local terminal emulator directly, which
//! is the semantically correct backend for SSH sessions.

use lattice_core::Clipboard;

/// OSC52 clipboard-write escape sequence
/// (`ESC ] 52 ; c ; <base64> BEL`, `c` = the clipboard selection). No
/// system-lib dependency; the write-only fallback for headless / SSH
/// sessions and for builds without the `system-clipboard` feature.
#[derive(Debug, Default, Clone, Copy)]
pub struct Osc52Clipboard;

impl Clipboard for Osc52Clipboard {
    fn read(&self) -> Option<String> {
        // Read-back is unsupported: most terminal emulators refuse the
        // OSC52 query half for security reasons (a program could otherwise
        // read whatever the user last copied in another app/window). The
        // register layer falls back to its in-memory entry.
        None
    }

    fn write(&self, text: String) {
        use base64::Engine;
        use std::io::Write;
        let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        let seq = format!("\x1b]52;c;{encoded}\x07");
        // Fire-and-forget: a broken pipe / write error must never
        // propagate into dispatch (graceful degradation, never panic on
        // the hot path).
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(seq.as_bytes());
        let _ = stdout.flush();
    }
}

/// Native OS clipboard via `arboard`. Only compiled in behind the
/// `system-clipboard` feature.
#[cfg(feature = "system-clipboard")]
pub struct ArboardClipboard {
    inner: std::sync::Arc<std::sync::Mutex<arboard::Clipboard>>,
}

#[cfg(feature = "system-clipboard")]
impl ArboardClipboard {
    /// `None` when no display is reachable (headless, or the display
    /// server refused the connection) — the caller falls back to OSC52.
    pub fn new() -> Option<Self> {
        arboard::Clipboard::new().ok().map(|inner| Self {
            inner: std::sync::Arc::new(std::sync::Mutex::new(inner)),
        })
    }

    /// Bounded wait so a hung display-server round-trip can't stall
    /// dispatch (paramount #1). `read_register`'s call sits on the
    /// blocking render→actor RPC (`input-pipeline.md`); this bound is the
    /// CB.2 half of the obligation flagged on
    /// `Editor::read_register` (`lattice-host/src/dispatch.rs`). 30ms is
    /// generous headroom over the common case (a local X11/Wayland/macOS
    /// clipboard round-trip is sub-5ms) while still bounding the worst
    /// case. `spawn_blocking` moves the actual FFI call off whatever
    /// thread drives the future; `lattice_runtime::block_on` is the
    /// existing sync-to-async bridge this codebase already uses from the
    /// editor actor (e.g. `Editor::apply_edit_blocking`).
    const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(30);
}

#[cfg(feature = "system-clipboard")]
impl Clipboard for ArboardClipboard {
    fn read(&self) -> Option<String> {
        let clipboard = self.inner.clone();
        let fut = async move {
            tokio::time::timeout(
                Self::READ_TIMEOUT,
                tokio::task::spawn_blocking(move || {
                    clipboard.lock().ok()?.get_text().ok()
                }),
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
        // return immediately, no bound needed (nothing awaits the
        // result).
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

/// The TUI's clipboard backend: native when available, OSC52 write-only
/// otherwise. See the module doc for the SSH-preference rule.
pub enum TuiClipboard {
    #[cfg(feature = "system-clipboard")]
    Native(ArboardClipboard),
    Fallback(Osc52Clipboard),
}

impl TuiClipboard {
    /// Detect the best backend at boot. Called once from
    /// [`crate::app::App::new`], right after `Editor::boot`, to override
    /// the `FakeClipboard` the host registers by default (CB.0).
    pub fn detect() -> Self {
        let under_ssh =
            std::env::var_os("SSH_TTY").is_some() || std::env::var_os("SSH_CONNECTION").is_some();
        Self::detect_with(under_ssh)
    }

    /// The pure selection logic behind [`Self::detect`], taking the
    /// SSH-session determination as a parameter so it's testable without
    /// mutating process-global env vars (which would race other tests
    /// running in parallel in this binary).
    fn detect_with(under_ssh: bool) -> Self {
        #[cfg(feature = "system-clipboard")]
        {
            if !under_ssh && let Some(native) = ArboardClipboard::new() {
                return Self::Native(native);
            }
        }
        let _ = under_ssh;
        Self::Fallback(Osc52Clipboard)
    }
}

impl Clipboard for TuiClipboard {
    fn read(&self) -> Option<String> {
        match self {
            #[cfg(feature = "system-clipboard")]
            Self::Native(c) => c.read(),
            Self::Fallback(c) => c.read(),
        }
    }

    fn write(&self, text: String) {
        match self {
            #[cfg(feature = "system-clipboard")]
            Self::Native(c) => c.write(text),
            Self::Fallback(c) => c.write(text),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn osc52_read_always_none() {
        assert_eq!(Osc52Clipboard.read(), None);
    }

    #[test]
    fn osc52_write_does_not_panic_on_a_normal_stdout() {
        // Can't assert the escape sequence landed anywhere meaningful in
        // a unit test (stdout isn't a terminal here), but the write path
        // must never panic even when nothing is listening for OSC52.
        Osc52Clipboard.write("hello".to_string());
    }

    #[test]
    fn detect_prefers_osc52_under_ssh() {
        // Even if a native backend would technically connect (X11
        // forwarding), OSC52 is the semantically correct backend under
        // SSH -- arboard would write to the *forwarded* server's
        // clipboard, not the user's local machine clipboard.
        let cb = TuiClipboard::detect_with(true);
        assert!(matches!(cb, TuiClipboard::Fallback(_)));
    }

    #[cfg(not(feature = "system-clipboard"))]
    #[test]
    fn detect_without_ssh_falls_back_when_feature_off() {
        let cb = TuiClipboard::detect_with(false);
        assert!(matches!(cb, TuiClipboard::Fallback(_)));
    }
}
