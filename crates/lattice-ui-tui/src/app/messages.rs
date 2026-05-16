//! `*messages*` buffer -- the emacs `*Messages*` analogue.
//!
//! The renderer-agnostic message stream types
//! ([`lattice_runtime::MessageRecord`],
//! [`lattice_runtime::MessagesRing`], the
//! [`lattice_runtime::MessagePushed`] typed event) live in
//! `lattice-runtime` so any host-side crate (this TUI
//! renderer, a future GPU renderer, plugins via the WIT
//! bridge, telemetry hooks) can subscribe without taking a
//! dep on `lattice-ui-tui`.
//!
//! ## Buffer model (Slice E)
//!
//! `*messages*` is a subsystem-owned Document buffer in the
//! unified registry — the same shape as `*lsp*` and friends.
//! Created eagerly at App boot so `:b *messages*` works the
//! moment the editor starts; appended to via the per-tick drain
//! that consumes [`lattice_runtime::MessagePushed`] events.
//!
//! - `name = Some("*messages*")` — surfaces in modeline, `:ls`,
//!   `:b` picker.
//! - `flags.listed = false` — `:bn`/`:bp` skip it (vim's
//!   `nobuflisted` semantic) but `:b *messages*` and the picker
//!   still reach it.
//! - Major: `text-mode` + minor `read-only-mode` so user
//!   keystrokes can't mutate the buffer; subsystem writes go
//!   through `apply_edit_batch_blocking` which bypasses the
//!   modal dispatcher's read-only gate.
//! - `:w <path>` saves a snapshot to disk; the streaming buffer
//!   continues to receive records.

use crate::app::App;
use crate::buffers::BufferId;

/// Synthetic name for the messages buffer in the registry.
pub const MESSAGES_BUFFER_NAME: &str = "*messages*";

impl App {
    /// `:messages` -- activate the `*messages*` Document buffer.
    /// Drains any queued events first so the view is up to date.
    pub fn do_open_messages(&mut self) {
        self.drain_message_events();
        let id = self.ensure_messages_buffer();
        self.activate_buffer(id);
    }

    /// Find-or-create the `*messages*` Document buffer.
    /// Idempotent; first creation seeds the buffer with the
    /// in-memory ring contents (so `:messages` after some
    /// records have already accumulated shows the backlog).
    /// Activates `text-mode` major + `read-only-mode` minor.
    pub(crate) fn ensure_messages_buffer(&mut self) -> BufferId {
        let already_present = self.editor.buffers.by_name(MESSAGES_BUFFER_NAME).is_some();
        // msg-mode.1: the buffer's major mode IS `messages-mode`
        // (symmetric with `lsp-log-mode` for `*lsp*`). The
        // mode contributes `ReadOnly = true` directly --
        // no separate `read-only-mode` minor needed.
        let id = self.ensure_named_synthetic_document(
            MESSAGES_BUFFER_NAME,
            lattice_mode::MessagesMode::mode_id(),
            Self::SYNTHETIC_BUFFER_FLAGS,
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
        let backlog_records: Vec<lattice_runtime::MessageRecord> = match self.editor.messages.lock() {
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

    /// Drain queued [`lattice_runtime::MessagePushed`] events;
    /// append each formatted record to the `*messages*` buffer.
    /// Called from the runtime's per-frame tick. Coalescing
    /// matters during bursts (LSP `$/progress` floods, batch
    /// echo): all records in a tick land in one
    /// `apply_edit_batch` so the actor sees one edit per drain
    /// regardless of event rate.
    pub fn drain_message_events(&mut self) {
        let Some(mut rx) = self.editor.pending_message_event_rx.take() else {
            return;
        };
        let mut text = String::new();
        while let Ok(ev) = rx.try_recv() {
            text.push_str(&format_message_record(&ev.record));
            text.push('\n');
        }
        self.editor.pending_message_event_rx = Some(rx);
        if text.is_empty() {
            return;
        }
        let id = self.ensure_messages_buffer();
        self.append_to_owned_buffer(id, &text);
    }
}

/// Render one record as `HH:MM:SS.mmm <level> <text>`. The
/// level prefix lets the reader scan for warns / errors at a
/// glance; the timestamp anchors the entry to wall-clock so
/// users can correlate with logs / external tools.
fn format_message_record(r: &lattice_runtime::MessageRecord) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::EchoLevel;
    use lattice_grammar::EchoLevel as WireLevel;
    use lattice_runtime::MessageRecord;

    /// End-to-end check that `set_message` streams every echo
    /// over the event bus. Mirrors how a plugin (or any other
    /// renderer-agnostic subscriber) taps in: subscribe a
    /// channel via `event_bus.subscribe_typed::<MessagePushed>`,
    /// drive `set_message`, assert every record landed on
    /// the subscriber's queue in arrival order with wire-
    /// typed levels. The renderer's own buffer view is just
    /// another peer subscriber.
    #[test]
    fn message_pushed_event_streams_to_external_subscriber() {
        use crate::app::test_helpers::app_with;
        use lattice_runtime::MessagePushed;
        let mut app = app_with("hi\n", 5);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<MessagePushed>();
        app.editor.event_bus.subscribe_typed(tx);
        app.set_message(EchoLevel::Info, "first");
        app.set_message(EchoLevel::Warn, "second");
        app.set_message(EchoLevel::Error, "third");
        let mut got: Vec<(WireLevel, String)> = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            got.push((ev.record.level, ev.record.text));
        }
        assert_eq!(
            got,
            vec![
                (WireLevel::Info, "first".to_string()),
                (WireLevel::Warn, "second".to_string()),
                (WireLevel::Error, "third".to_string()),
            ],
        );
    }

    /// Live-tail: when the `*messages*` buffer exists, the drain
    /// appends each event's record. Three echoes + one drain
    /// produce a buffer body containing every record in order.
    #[test]
    fn drain_message_events_appends_to_messages_buffer() {
        use crate::app::test_helpers::app_with;
        let mut app = app_with("hi\n", 5);
        let buffer_id = app.ensure_messages_buffer();
        app.set_message(EchoLevel::Info, "alpha");
        app.set_message(EchoLevel::Warn, "bravo");
        app.drain_message_events();
        let body = app
            .editor.buffers
            .document_handle(buffer_id)
            .expect("*messages* is a Document")
            .text();
        assert!(body.contains("alpha"), "got `{body}`");
        assert!(body.contains("bravo"), "got `{body}`");
    }

    #[test]
    fn messages_buffer_appears_in_buffer_registry_with_synthetic_name() {
        // Slice E: `*messages*` is a Document in the unified
        // registry; `:b *messages*` reaches it via name lookup.
        use crate::app::test_helpers::app_with;
        let mut app = app_with("hi\n", 5);
        let id = app.ensure_messages_buffer();
        assert_eq!(app.editor.buffers.by_name(MESSAGES_BUFFER_NAME), Some(id));
        let (is_doc, name, listed) = app
            .editor.buffers
            .with_entry(id, |entry| {
                (
                    matches!(entry.data, crate::buffer_registry::BufferData::Document(_)),
                    entry.name.clone(),
                    entry.flags.listed,
                )
            })
            .expect("*messages* entry");
        assert!(is_doc);
        assert_eq!(name.as_deref(), Some(MESSAGES_BUFFER_NAME));
        // Unlisted: `:bn`/`:bp` skip it.
        assert!(!listed);
    }

    #[test]
    fn do_open_messages_activates_messages_buffer() {
        // Slice E: `:messages` switches the active pane to the
        // `*messages*` buffer. Modeline shows the synthetic name.
        use crate::app::test_helpers::app_with;
        let mut app = app_with("hi\n", 5);
        let initial = app.active_pane_buffer_id();
        app.do_open_messages();
        let msgs_id = app
            .editor.buffers
            .by_name(MESSAGES_BUFFER_NAME)
            .expect("*messages* present");
        assert_ne!(initial, msgs_id);
        assert_eq!(app.active_pane_buffer_id(), msgs_id);
        let pane = app.editor.pane_tree.active().clone();
        let label = app.pane_status_label(&pane);
        assert!(
            label.contains("*messages*"),
            "modeline must surface *messages*; got `{label}`"
        );
    }

    #[test]
    fn messages_buffer_is_read_only() {
        use crate::app::test_helpers::app_with;
        let mut app = app_with("hi\n", 5);
        let id = app.ensure_messages_buffer();
        let ro = *app.resolved_option::<lattice_config::ReadOnly>(id);
        assert!(
            ro,
            "*messages* buffer must resolve ReadOnly = true via read-only-mode"
        );
    }

    #[test]
    fn format_message_record_collapses_internal_newlines() {
        let r = MessageRecord {
            timestamp: std::time::SystemTime::UNIX_EPOCH,
            level: WireLevel::Error,
            text: "line one\nline two".into(),
        };
        let s = format_message_record(&r);
        assert!(!s.contains('\n'), "newlines collapsed");
        assert!(s.contains("line one line two"));
    }
}
