//! `*messages*` buffer management on `Editor`.
//!
//! Phase 5.7.B.9: migrates [`MESSAGES_BUFFER_NAME`],
//! `format_message_record`, `one_line`, and
//! `ensure_messages_buffer` from `impl App` (TUI,
//! `lattice-ui-tui::app::messages`) to `impl Editor` (host) so
//! the GPUI peer can eagerly seed the messages transcript
//! buffer at boot through the same code path the TUI peer uses.
//!
//! The TUI peer keeps `do_open_messages` and
//! `drain_message_events` (renderer-coupled — they pull the
//! buffer into focus / drive per-tick draining); those are
//! out of scope for this host migration.

use crate::editor::Editor;
use crate::synthetic_buffers::SYNTHETIC_BUFFER_FLAGS;

/// Synthetic name for the `*messages*` transcript buffer.
/// Matches emacs' `*Messages*` analogue; surfaced via
/// `:messages` (ex-command) and `:b *messages*`.
pub const MESSAGES_BUFFER_NAME: &str = "*messages*";

/// Render one record as `HH:MM:SS.mmm <level> <text>`. The
/// level prefix lets the reader scan for warns / errors at a
/// glance; the timestamp anchors the entry to wall-clock so
/// users can correlate with logs / external tools.
///
/// Phase 5.7.B.9: migrated from
/// `lattice-ui-tui::app::messages::format_message_record`.
/// `pub` so subsystem crates (drain loops, test fixtures) can
/// reach the canonical format.
pub fn format_message_record(r: &lattice_runtime::MessageRecord) -> String {
    let elapsed = r
        .timestamp
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .ok();
    let secs = elapsed.map(|d| d.as_secs()).unwrap_or(0);
    let ms = elapsed.map(|d| d.subsec_millis()).unwrap_or(0);
    let hh = (secs / 3600) % 24;
    let mm = (secs / 60) % 60;
    let ss = secs % 60;
    let level = match r.level {
        lattice_grammar::EchoLevel::Trace => "TRACE",
        lattice_grammar::EchoLevel::Debug => "DEBUG",
        lattice_grammar::EchoLevel::Info => " INFO",
        lattice_grammar::EchoLevel::Warn => " WARN",
        lattice_grammar::EchoLevel::Error => "ERROR",
    };
    let text = one_line(&r.text);
    format!("{hh:02}:{mm:02}:{ss:02}.{ms:03} {level} {text}")
}

/// Collapse internal newlines so multi-line echo bodies (rare
/// but possible -- e.g. a multi-line server error) still
/// render as a single transcript row. Mirrors the formatting
/// the LSP log buffer uses.
fn one_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = false;
    for c in s.chars() {
        if c == '\n' || c == '\r' || c == '\t' {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(c);
            last_was_space = c == ' ';
        }
    }
    out
}

impl Editor {
    /// Eagerly create the editor's standard subsystem buffers
    /// (`*lsp*` + `*messages*`) so name-based lookups like
    /// `:b *lsp*` and `:b *messages*` resolve from t=0 instead
    /// of waiting for lazy creation on first use.
    ///
    /// Phase 5.7.B.9: aggregator added so both renderer peers
    /// run identical eager-seeding without each having to name
    /// the subsystem-specific constants / mode-ids. The TUI
    /// peer historically inlined the two `ensure_*` calls in
    /// `App::new`; both peers now share this entry.
    pub fn ensure_subsystem_buffers(&mut self) {
        self.ensure_named_synthetic_document(
            lattice_lsp::LSP_SUBSYSTEM_LOG_NAME,
            lattice_lsp::modes::LspLogMode::mode_id(),
            SYNTHETIC_BUFFER_FLAGS,
        );
        self.ensure_messages_buffer();
    }

    /// Drain queued `MessagePushed` events; append each formatted
    /// record to the `*messages*` buffer in one
    /// `apply_edit_batch` so the actor sees a single edit per
    /// drain. Phase 5.8.AA.f: hoisted from
    /// `lattice-ui-tui::app::messages::App::drain_message_events`.
    pub fn drain_message_events(&mut self) {
        let Some(mut rx) = self.pending_message_event_rx.take() else {
            return;
        };
        let mut text = String::new();
        while let Ok(ev) = rx.try_recv() {
            text.push_str(&format_message_record(&ev.record));
            text.push('\n');
        }
        self.pending_message_event_rx = Some(rx);
        if text.is_empty() {
            return;
        }
        let id = self.ensure_messages_buffer();
        self.append_to_owned_buffer(id, &text);
    }

    /// Find-or-create the `*messages*` Document buffer.
    /// Idempotent; first creation seeds the buffer with the
    /// in-memory ring contents (so `:messages` after some
    /// records have already accumulated shows the backlog).
    /// Activates `messages-mode` (which contributes
    /// `ReadOnly = true` so user keystrokes can't mutate; the
    /// streaming append path goes through
    /// [`Self::append_to_owned_buffer`]).
    ///
    /// Phase 5.7.B.9: migrated from
    /// `lattice-ui-tui::app::messages::App::ensure_messages_buffer`.
    /// Both renderer peers now reach the canonical body; the
    /// TUI peer's `App::ensure_messages_buffer` becomes a thin
    /// wrapper.
    pub fn ensure_messages_buffer(&mut self) -> crate::buffers::BufferId {
        let already_present = self.buffers.by_name(MESSAGES_BUFFER_NAME).is_some();
        // msg-mode.1: the buffer's major mode IS `messages-mode`
        // (symmetric with `lsp-log-mode` for `*lsp*`). The
        // mode contributes `ReadOnly = true` directly --
        // no separate `read-only-mode` minor needed.
        let id = self.ensure_named_synthetic_document(
            MESSAGES_BUFFER_NAME,
            lattice_mode::MessagesMode::mode_id(),
            SYNTHETIC_BUFFER_FLAGS,
        );
        if already_present {
            return id;
        }
        // First-time creation: seed the buffer with any ring
        // backlog records that arrived before the buffer
        // existed. Backlog seeding matters when the buffer is
        // created lazily (e.g. tests); the production boot
        // creates it eagerly so the ring is usually empty at
        // creation time.
        // msg-mode.1: ring is Arc<Mutex<>> so the
        // boot-installed MessagesLayer can push from any
        // thread. Lock briefly to snapshot the backlog into a
        // local Vec, then release before formatting +
        // appending (Drop semantics: keep the critical section
        // short).
        let backlog_records: Vec<lattice_runtime::MessageRecord> = match self.messages.lock() {
            Ok(ring) => ring.records().iter().cloned().collect(),
            Err(_) => Vec::new(),
        };
        let backlog: String = backlog_records
            .iter()
            .map(format_message_record)
            .map(|line| {
                let mut s = line;
                s.push('\n');
                s
            })
            .collect();
        if !backlog.is_empty() {
            self.append_to_owned_buffer(id, &backlog);
        }
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_collapses_internal_newlines() {
        assert_eq!(one_line("hello\nworld"), "hello world");
        assert_eq!(one_line("a\n\nb"), "a b");
        assert_eq!(one_line("tab\there"), "tab here");
        assert_eq!(one_line("crlf\r\nline"), "crlf line");
    }

    #[test]
    fn one_line_preserves_spaces() {
        assert_eq!(one_line("hello world"), "hello world");
        assert_eq!(one_line("a  b"), "a  b");
    }

    #[test]
    fn one_line_no_special_chars_passes_through() {
        assert_eq!(one_line("simple"), "simple");
        assert_eq!(one_line(""), "");
    }

    #[test]
    fn format_message_record_level_prefixes_align() {
        // Width-padded level prefixes so the transcript columns
        // line up regardless of severity. Visible to the user --
        // worth guarding against regressions.
        use std::time::SystemTime;
        let rec = |level: lattice_grammar::EchoLevel| lattice_runtime::MessageRecord {
            level,
            text: "x".into(),
            timestamp: SystemTime::UNIX_EPOCH,
        };
        let line_for = |level: lattice_grammar::EchoLevel| format_message_record(&rec(level));
        // All five rendered lines share the same length.
        let lengths: Vec<usize> = [
            lattice_grammar::EchoLevel::Trace,
            lattice_grammar::EchoLevel::Debug,
            lattice_grammar::EchoLevel::Info,
            lattice_grammar::EchoLevel::Warn,
            lattice_grammar::EchoLevel::Error,
        ]
        .iter()
        .map(|&l| line_for(l).len())
        .collect();
        assert!(
            lengths.windows(2).all(|w| w[0] == w[1]),
            "level prefixes should be width-padded so all rows align; got lengths {lengths:?}"
        );
    }
}
