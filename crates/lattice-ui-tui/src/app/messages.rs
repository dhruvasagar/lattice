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
// 5.8.AA.f: `format_message_record` lives host-side now and the
// drain body moved with it. Kept as `#[allow]` so the existing
// `#[cfg(test)]` consumers below still resolve.
#[allow(unused_imports)]
use lattice_host::messages::format_message_record;

/// Re-export of the canonical
/// [`lattice_host::messages::MESSAGES_BUFFER_NAME`] (Phase
/// 5.7.B.9 migration). Kept at this path so existing
/// `crate::app::messages::MESSAGES_BUFFER_NAME` references in
/// downstream call sites + tests resolve unchanged. The
/// `#[allow]` silences rustc's false-positive "unused import"
/// when the only in-file consumers are inside `#[cfg(test)]`.
#[allow(unused_imports)]
pub use lattice_host::messages::MESSAGES_BUFFER_NAME;

impl App {
    /// `:messages` -- activate the `*messages*` Document buffer.
    /// 5.8.AF.3: body migrated to
    /// [`lattice_host::editor::Editor::do_open_messages`]. Wrapper
    /// fans renderer signals through `handle_renderer_signal`.
    pub fn do_open_messages(&mut self) {
        let signals = self.mutate_editor_with(move |e| e.do_open_messages());
        for sig in signals {
            self.handle_renderer_signal(sig);
        }
    }

    /// `:ai-log [provider]` -- open the per-session AI log buffer
    /// (AI-1b, T12b). Thin peer forwarder: the count logic (0 →
    /// info hint, 1 → open, >1 → picker) + the buffer open live
    /// host-side in [`lattice_host::editor::Editor::do_open_ai_log`];
    /// the GPUI peer reaches the same method. Mirrors
    /// `do_open_lsp_log`.
    pub fn do_open_ai_log(&mut self, provider: Option<&str>) {
        let provider = provider.map(|s| s.to_string());
        self.mutate_editor(move |e| e.do_open_ai_log(provider.as_deref()));
    }

    /// `Effect::OpenSyntheticBuffer` -- open a named synthetic buffer under a
    /// major mode. Thin peer forwarder to
    /// [`lattice_host::editor::Editor::open_synthetic_buffer`]; the GPUI peer
    /// reaches the same method.
    pub fn open_synthetic_buffer(&mut self, name: &str, mode_id: &str) {
        let name = name.to_string();
        let mode_id = mode_id.to_string();
        self.mutate_editor(move |e| e.open_synthetic_buffer(&name, &mode_id));
    }

    /// Thin wrapper around
    /// [`lattice_host::editor::Editor::ensure_messages_buffer`]
    /// (Phase 5.7.B.9 migration). The find-or-create body +
    /// backlog seeding live host-side; the GPUI peer reaches
    /// the same logic via `editor.ensure_messages_buffer()`.
    pub(crate) fn ensure_messages_buffer(&mut self) -> crate::buffers::BufferId {
        self.mutate_editor_with(move |e| e.ensure_messages_buffer())
    }

    /// Drain queued [`lattice_runtime::MessagePushed`] events;
    /// append each formatted record to the `*messages*` buffer.
    /// Called from the runtime's per-frame tick. Coalescing
    /// matters during bursts (LSP `$/progress` floods, batch
    /// echo): all records in a tick land in one
    /// `apply_edit_batch` so the actor sees one edit per drain
    /// regardless of event rate.
    pub fn drain_message_events(&mut self) {
        // 5.8.AA.f: migrated to host.
        self.mutate_editor_with(move |e| e.drain_message_events());
    }
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
            .editor
            .buffers
            .document_handle(buffer_id)
            .expect("*messages* is a Document")
            .text();
        assert!(body.contains("alpha"), "got `{body}`");
        assert!(body.contains("bravo"), "got `{body}`");
    }

    #[test]
    fn messages_buffer_appears_in_buffer_registry_with_synthetic_name() {
        // `*messages*` lives in the unified registry tagged as
        // `BufferData::Messages` (the storage variant matching
        // `BufferKind::Messages`); `:b *messages*` reaches it
        // via name lookup; `:bn`/`:bp` skip it (unlisted).
        use crate::app::test_helpers::app_with;
        let mut app = app_with("hi\n", 5);
        let id = app.ensure_messages_buffer();
        assert_eq!(app.editor.buffers.by_name(MESSAGES_BUFFER_NAME), Some(id));
        let (is_messages, name, listed) = app
            .editor
            .buffers
            .with_entry(id, |entry| {
                (
                    matches!(entry.data, crate::buffer_registry::BufferData::Messages(_)),
                    entry.name.clone(),
                    entry.flags.listed,
                )
            })
            .expect("*messages* entry");
        assert!(
            is_messages,
            "*messages* must be stored as BufferData::Messages"
        );
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
            .editor
            .buffers
            .by_name(MESSAGES_BUFFER_NAME)
            .expect("*messages* present");
        assert_ne!(initial, msgs_id);
        assert_eq!(app.active_pane_buffer_id(), msgs_id);
        let pane = *app.editor.pane_tree.active();
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
