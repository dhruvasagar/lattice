//! Host-side helper for spawning the synthetic Document buffers
//! that mode-owned subsystems (LSP log family, `*messages*`,
//! future `*scratch*`) attach to.
//!
//! B'.7 retires the per-flavour `ensure_lsp_*_buffer` helpers in
//! favour of one subsystem-agnostic
//! [`App::ensure_named_synthetic_document`] entry point. The
//! ex-command handlers and boot path now compute the canonical
//! buffer name (via `lattice_lsp::lsp_server_log_name` and
//! friends) and the major mode id themselves, then call this
//! helper. No subsystem-specific knowledge lives in the host
//! anymore: name format + mode id are inputs; the helper just
//! find-or-creates the Document, activates the named major mode,
//! and returns the id.
//!
//! The major mode is responsible for everything downstream --
//! deriving its identity from the buffer's name, subscribing to
//! the event bus, seeding from the in-memory ring, formatting
//! incoming records, and tearing it all down on
//! `on_deactivate`. The host's only contribution after creation
//! is the `ReadOnly = true` contribution the major mode itself
//! emits via `options()`.
//!
//! `:w <path>` still works through the existing Document save
//! path — the buffer behaves like any unsaved Document; saving
//! produces a regular editable file while the streaming buffer
//! keeps its read-only-by-subsystem identity.

use lattice_protocol::edit::Edit;
use lattice_protocol::position::Position;

use crate::app::App;
use crate::buffer_registry::{BufferData, BufferEntry, DocumentEntry};
use crate::buffers::{BufferFlags, BufferId};

impl App {
    /// Find-or-create a synthetic Document buffer with `name`,
    /// the given `major_id` activated on it, and the supplied
    /// `flags`. Idempotent: a second call with the same `name`
    /// returns the existing id (major mode is not re-activated,
    /// flags are not overwritten).
    ///
    /// Used by every mode-owned synthetic buffer:
    /// - `*lsp*` (boot) via `LspLogMode::mode_id()`.
    /// - `*lsp:<server>:<workspace>*` (ex-command + picker open)
    ///   via `LspServerLogMode::mode_id()`.
    /// - `*lsp:<server>:<workspace>:trace*` (`:lsp-trace` toggle +
    ///   `:lsp-trace-log` open) via `LspTraceLogMode::mode_id()`.
    /// - `*messages*` (boot) via `MessagesMode::mode_id()`.
    ///
    /// Activation runs the major's `on_activate` synchronously,
    /// so any subscription / spawn the mode does is in place by
    /// the time this function returns. The major mode is what
    /// derives the buffer's identity (instance key for LSP log
    /// variants); the host does not stash any subsystem-shaped
    /// buffer-local before activation.
    pub(crate) fn ensure_named_synthetic_document(
        &mut self,
        name: &str,
        major_id: lattice_mode::ModeId,
        flags: BufferFlags,
    ) -> BufferId {
        if let Some(id) = self.buffers.by_name(name) {
            return id;
        }
        let id = BufferId::next();
        let document = lattice_core::Document::empty();
        let handle = lattice_runtime::spawn_document(document, self.editor.registry.clone());
        self.buffers.insert(BufferEntry {
            id,
            flags,
            data: BufferData::Document(DocumentEntry { id, handle }),
            name: Some(name.to_string()),
        });
        // Seed empty mode-owned document locals so downstream
        // accessors (`document_syntax_for` etc.) resolve cleanly
        // through `buffer_locals` for this id.
        self.seed_empty_document_locals(id);
        // Activate `major_id` directly. We can't use
        // `activate_major_for_buffer_kind` because it auto-detects
        // the language from the buffer's path (which is None here)
        // and would pick `text-mode` instead of the caller's
        // intended major.
        self.activate_major_by_id(id, major_id);
        id
    }

    /// Unlisted, non-hidden flags — the canonical shape every
    /// mode-owned synthetic buffer wants (`:bn` / `:bp` cycles
    /// skip, `:ls` shows with a `u` marker, `:b <name>` still
    /// reaches).
    pub(crate) const SYNTHETIC_BUFFER_FLAGS: BufferFlags = BufferFlags {
        listed: false,
        hidden: false,
    };

    /// Activate `major_id` on `buffer_id` directly, bypassing the
    /// language-detection path. Used by synthetic-buffer creators
    /// that already know which major mode they want.
    pub(crate) fn activate_major_by_id(
        &mut self,
        buffer_id: BufferId,
        major_id: lattice_mode::ModeId,
    ) {
        let proto_id = lattice_protocol::ids::BufferId::new(buffer_id.0 as u64);
        let mut active = self.editor.active_modes.remove(&buffer_id).unwrap_or_default();
        if let Err(e) = self.editor.mode_registry.activate_major(
            &mut active,
            &self.editor.mode_guards,
            &self.editor.config,
            &self.editor.event_bus,
            &self.editor.services,
            proto_id,
            major_id,
            lattice_mode::CapabilitySet::empty(),
        ) {
            self.set_message(
                crate::app::EchoLevel::Warn,
                format!(
                    "mode: activate_major({}) for buffer {} failed: {}",
                    major_id, buffer_id.0, e,
                ),
            );
        }
        self.editor.active_modes.insert(buffer_id, active);
        // Recompute the resolved-options cache so the mode's
        // contributions (e.g. `ReadOnly = true` from
        // `lsp-log-mode`) are visible at the next `resolved_option`
        // read. The full kind-driven `activate_major_for_buffer_kind`
        // calls this too; we mirror the contract here.
        self.recompute_options_for_buffer(buffer_id);
    }

    /// Append `text` to the end of the Document at `buffer_id`.
    /// Used by subsystems that own synthetic buffers to feed
    /// streamed records without going through the modal-dispatch
    /// insert path (which would block on the buffer's read-only
    /// contribution).
    ///
    /// Blocking: this calls into the document actor's
    /// `apply_edit_batch` mailbox via `block_on`. Cheap when text
    /// is small; the actor's reparse path is a no-op for buffers
    /// whose major mode (`lsp-log-mode` / `lsp-trace-log-mode`)
    /// does not attach a syntax handle.
    ///
    /// No-op when `buffer_id` does not resolve to a Document in
    /// the registry, or when `text` is empty.
    pub(crate) fn append_to_owned_buffer(&mut self, buffer_id: BufferId, text: &str) {
        if text.is_empty() {
            return;
        }
        let Some(handle) = self.buffers.document_handle(buffer_id) else {
            return;
        };
        let snap = handle.snapshot();
        let last_line = crate::app::last_addressable_line(&snap.buffer);
        let line_len = crate::app::line_byte_len(&snap.buffer, last_line);
        let pos = Position::new(last_line, line_len);
        let edit = Edit::insert(pos, text);
        let _ = lattice_runtime::block_on(handle.apply_edit_batch(vec![edit]));
    }
}

// B'.6: `format_log_event_line` lived here in B'.3 as a thin
// alias around `lattice_lsp::format_log_event_line`. After the
// App-side drain stopped formatting log records (the three log
// majors do it now), the alias has no remaining users in this
// crate. The canonical helper stays in `lattice-lsp`.

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use crate::app::test_helpers::app_with;
    use crate::buffer_registry::BufferData;
    use lattice_config::ReadOnly;
    use lattice_lsp::{LSP_SUBSYSTEM_LOG_NAME, lsp_server_log_name, lsp_server_trace_log_name};

    #[test]
    fn boot_creates_subsystem_lsp_buffer() {
        let a = app_with("hi", 5);
        let id = a
            .buffers
            .by_name(LSP_SUBSYSTEM_LOG_NAME)
            .expect("`*lsp*` buffer present at boot");
        // Must be a Document (slice B requirement) with the
        // synthetic name set.
        let (is_doc, name, listed) = a
            .buffers
            .with_entry(id, |entry| {
                (
                    matches!(entry.data, BufferData::Document(_)),
                    entry.name.clone(),
                    entry.flags.listed,
                )
            })
            .expect("entry registered");
        assert!(is_doc);
        assert_eq!(name.as_deref(), Some(LSP_SUBSYSTEM_LOG_NAME));
        // And unlisted -- `:bn` cycles skip it.
        assert!(!listed);
    }

    #[test]
    fn lsp_log_buffer_contributes_read_only() {
        let a = app_with("hi", 5);
        let id = a.buffers.by_name(LSP_SUBSYSTEM_LOG_NAME).unwrap();
        // lsp-log-mode contributes ReadOnly = true. The resolved
        // option for this buffer must reflect that contribution.
        let ro = *a.resolved_option::<ReadOnly>(id);
        assert!(
            ro,
            "*lsp* buffer must resolve ReadOnly = true via lsp-log-mode"
        );
    }

    #[test]
    fn pane_status_label_for_lsp_buffer_uses_synthetic_name() {
        // Slice A + B together: a synthetic Document buffer's
        // modeline shows its `name`, not "[no name]".
        let mut a = app_with("hi", 5);
        let id = a.buffers.by_name(LSP_SUBSYSTEM_LOG_NAME).unwrap();
        a.activate_buffer(id);
        let pane = a.pane_tree.active().clone();
        let label = a.pane_status_label(&pane);
        assert!(
            label.contains("*lsp*"),
            "modeline must surface the synthetic name; got `{label}`"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lsp_log_drain_appends_to_subsystem_buffer() {
        // B'.3: the subsystem-wide `*lsp*` buffer is owned by
        // `LspLogMode`'s subscription. The mode's drain task is
        // spawned via tokio; the test sleeps a few millis so the
        // task gets scheduled before we inspect the buffer.
        let a = app_with("hi", 5);
        let id = a.buffers.by_name(LSP_SUBSYSTEM_LOG_NAME).unwrap();
        let before = a.buffers.document_handle(id).unwrap().text();
        a.lsp_logger.log(
            None,
            lattice_lsp::LogLevel::Info,
            lattice_lsp::LogSource::Client,
            "boot-time chatter",
        );
        // Let the LspLogMode tokio task drain + apply its edit.
        // 50ms is generous; production drain coalesces in <1ms.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let after = a.buffers.document_handle(id).unwrap().text();
        assert!(after.len() > before.len());
        assert!(
            after.contains("boot-time chatter"),
            "subsystem log must capture server_id=None records; got:\n{after}"
        );
    }

    #[test]
    fn lsp_log_no_arg_activates_subsystem_buffer() {
        // Bug #3 fix: `:lsp-log` (no arg) activates the
        // subsystem-wide `*lsp*` buffer directly. Previously the
        // no-arg form opened a picker over running servers; with
        // no servers it errored out and left the user on the
        // initial unnamed Document, where the modeline correctly
        // showed `[no name]` -- but the user expected to land on
        // `*lsp*`. The bug was UX: `:b` showed `*lsp*` in the
        // registry, but the active pane never switched to it.
        //
        // The picker behaviour moves to `:lsp-server-log`; the
        // no-arg `:lsp-log` is the direct subsystem-view entry.
        let mut a = app_with("hi", 5);
        let initial = a.active_pane_buffer_id();
        let lsp_buf = a.buffers.by_name("*lsp*").expect("*lsp* at boot");
        assert_ne!(initial, lsp_buf);
        a.do_open_lsp_log(None);
        assert_eq!(
            a.active_pane_buffer_id(),
            lsp_buf,
            "no-arg :lsp-log must activate *lsp*"
        );
        let pane = a.pane_tree.active().clone();
        let label = a.pane_status_label(&pane);
        assert!(
            label.contains("*lsp*"),
            "modeline must surface *lsp* after :lsp-log no-arg; got `{label}`"
        );
    }

    /// Build the per-instance synthetic key the App's
    /// `resolve_lsp_instance_for` produces in the no-actor case
    /// (cwd-backed fallback). Tests that drive
    /// `open_lsp_log_in_pane("rust")` without spawning an actor
    /// land on this instance; the helper keeps them tied to the
    /// same naming the App uses.
    fn synth_instance(server_id: &str) -> lattice_lsp::InstanceKey {
        lattice_lsp::InstanceKey::new(
            std::sync::Arc::<str>::from(server_id),
            std::sync::Arc::<std::path::Path>::from(
                std::env::current_dir()
                    .unwrap_or_else(|_| std::path::PathBuf::from("/"))
                    .as_path(),
            ),
        )
    }

    #[test]
    fn open_lsp_log_in_pane_modeline_shows_synthetic_name() {
        // Bug #3 repro candidate: navigate to a per-server log
        // via the `open_lsp_log_in_pane` path (same code the
        // `:lsp-log <server>` ex-command runs through after the
        // picker short-circuit). The modeline must surface
        // `*lsp:<server>:<workspace>*` (B'.4), not `[no name]`.
        let mut a = app_with("hi", 5);
        let instance = synth_instance("rust");
        let expected = lsp_server_log_name(&instance);
        a.open_lsp_log_in_pane("rust");
        let log_id = a
            .buffers
            .by_name(&expected)
            .expect("per-instance log buffer registered");
        assert_eq!(a.active_pane_buffer_id(), log_id);
        let pane = a.pane_tree.active().clone();
        let label = a.pane_status_label(&pane);
        assert!(
            label.contains(&expected),
            "modeline must surface synthetic name after :lsp-log; got `{label}`, expected to contain `{expected}`"
        );
    }

    #[test]
    fn open_lsp_trace_log_in_pane_modeline_shows_synthetic_name() {
        // Symmetric to above: `:lsp-trace-log <server>` path.
        let mut a = app_with("hi", 5);
        let instance = synth_instance("rust");
        let expected = lsp_server_trace_log_name(&instance);
        a.open_lsp_trace_log_in_pane("rust");
        let trace_id = a
            .buffers
            .by_name(&expected)
            .expect("per-instance trace buffer registered");
        assert_eq!(a.active_pane_buffer_id(), trace_id);
        let pane = a.pane_tree.active().clone();
        let label = a.pane_status_label(&pane);
        assert!(
            label.contains(&expected),
            "modeline must surface trace buffer name; got `{label}`"
        );
    }

    #[test]
    fn lsp_trace_toggle_creates_trace_buffer() {
        // B'.7: the generic `ensure_named_synthetic_document`
        // path is the canonical create surface. The trace
        // toggle (`:lsp-trace`) drives it with the
        // `lsp_server_trace_log_name`-derived name + the
        // `LspTraceLogMode::mode_id()` major; this test goes
        // straight to that helper because spinning up a real
        // running actor would require LSP config plumbing the
        // test isn't built for.
        let mut a = app_with("hi", 5);
        let instance = lattice_lsp::InstanceKey::new(
            std::sync::Arc::<str>::from("rust"),
            std::sync::Arc::<std::path::Path>::from(std::path::Path::new("/tmp/test-ws")),
        );
        let expected_name = lsp_server_trace_log_name(&instance);
        assert!(a.buffers.by_name(&expected_name).is_none());
        let id = a.ensure_named_synthetic_document(
            &expected_name,
            lattice_lsp::modes::LspTraceLogMode::mode_id(),
            crate::app::App::SYNTHETIC_BUFFER_FLAGS,
        );
        assert_eq!(a.buffers.by_name(&expected_name), Some(id));
        // Trace buffer also read-only via lsp-trace-log-mode.
        let ro = *a.resolved_option::<ReadOnly>(id);
        assert!(
            ro,
            "trace buffer must resolve ReadOnly = true via lsp-trace-log-mode"
        );
    }
}
