//! CB.2 / CB.4 (`docs/dev/architecture/clipboard.md` §4): the TUI's
//! [`lattice_core::Clipboard`] backend. Two implementations composed by
//! [`TuiClipboard::detect`]:
//!
//! - [`Osc52Clipboard`] — terminal escape-sequence write. No link deps,
//!   always compiled in. Read-back is unsupported (most terminals block the
//!   OSC52 read half for security reasons), so `read` always returns
//!   `None` and callers fall back to the in-memory register. This is the
//!   TUI-specific half (writing escape codes to stdout only makes sense for
//!   a terminal), so it lives here rather than in the shared host module.
//! - [`lattice_host::clipboard::ArboardClipboard`] (behind the
//!   `system-clipboard` feature) — native OS clipboard via `arboard`, the
//!   **shared** backend both renderer peers use (CB.4 moved it to
//!   `lattice-host` so the bounded-read logic isn't duplicated; see that
//!   module's doc). Feature-gated because it pulls X11/Wayland link libs;
//!   `lattice-ui-tui/system-clipboard` forwards to
//!   `lattice-host/system-clipboard`.
//!
//! `TuiClipboard::detect()` prefers OSC52 under SSH even when `arboard`
//! would technically succeed: over `ssh -X`, `arboard` can connect to the
//! *forwarded* X server, but writes there land in the remote X server's
//! clipboard, not the user's local machine clipboard. OSC52 tunnels through
//! the terminal escape codes to the local terminal emulator directly, which
//! is the semantically correct backend for SSH sessions.

use lattice_core::Clipboard;
#[cfg(feature = "system-clipboard")]
use lattice_host::clipboard::ArboardClipboard;

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
