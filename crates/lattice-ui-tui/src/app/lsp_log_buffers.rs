//! LSP subsystem-owned log buffers (slice B of the synthetic-buffer
//! migration; see `feedback-synthetic-buffers` memory).
//!
//! The LSP subsystem owns three flavours of synthetic Document
//! buffer:
//!
//! - `*lsp*` — subsystem-wide log; mirrors every record published
//!   to [`lattice_lsp::LspLogger`]. Created eagerly at App boot so
//!   `:b *lsp*` works the moment the editor starts.
//! - `*lsp:<server>*` — per-server log. Lazy-created when the
//!   first record for that server arrives, OR when the user runs
//!   `:lsp-log <server>`. Records prefixed with the server id are
//!   filtered to the matching per-server buffer (subsystem-wide
//!   chatter without a server tag does not appear here).
//! - `*lsp:<server>:trace*` — per-server JSON-RPC trace buffer.
//!   Created eagerly when `:lsp-trace <server>` toggles ON so the
//!   buffer is visible in `:ls` from the moment the user enables
//!   tracing.
//!
//! All three live in the unified [`crate::buffer_registry`] as
//! `BufferKind::Document` entries with the `name` slot set to
//! their synthetic label. The major mode contributes
//! `ReadOnly = true` so user-driven Insert / operator paths echo
//! `"buffer is read-only"`; subsystem writes go through
//! [`App::append_to_owned_buffer`] which bypasses the modal
//! dispatcher naturally (direct `DocumentHandle::apply_edit_batch`
//! call, not a user action).
//!
//! `:w <path>` works through the existing Document save path — the
//! buffer behaves like any unsaved Document; saving produces a
//! regular editable file while the streaming buffer keeps its
//! read-only-by-subsystem identity.

use lattice_protocol::edit::Edit;
use lattice_protocol::position::Position;

use crate::app::App;
use crate::buffer_registry::{BufferData, BufferEntry, DocumentEntry};
use crate::buffers::{BufferFlags, BufferId};

/// Synthetic name for the subsystem-wide LSP log buffer.
pub const LSP_SUBSYSTEM_LOG_NAME: &str = "*lsp*";

/// Build the synthetic name for a per-server LSP log buffer.
pub fn lsp_server_log_name(server_id: &str) -> String {
    format!("*lsp:{server_id}*")
}

/// Build the synthetic name for a per-server LSP trace buffer.
pub fn lsp_server_trace_log_name(server_id: &str) -> String {
    format!("*lsp:{server_id}:trace*")
}

impl App {
    /// Find-or-create the subsystem-wide `*lsp*` Document buffer.
    /// Idempotent: subsequent calls return the existing id.
    pub(crate) fn ensure_lsp_subsystem_log_buffer(&mut self) -> BufferId {
        self.ensure_lsp_log_owned_buffer(
            LSP_SUBSYSTEM_LOG_NAME,
            lattice_lsp::modes::LspLogMode::mode_id(),
        )
    }

    /// Find-or-create the per-server `*lsp:<server>*` Document
    /// buffer. The major mode is `lsp-log-mode` (read-only).
    pub(crate) fn ensure_lsp_server_log_buffer(&mut self, server_id: &str) -> BufferId {
        let name = lsp_server_log_name(server_id);
        self.ensure_lsp_log_owned_buffer(&name, lattice_lsp::modes::LspLogMode::mode_id())
    }

    /// Find-or-create the per-server `*lsp:<server>:trace*`
    /// Document buffer. The major mode is `lsp-trace-log-mode`
    /// (read-only).
    pub(crate) fn ensure_lsp_server_trace_buffer(&mut self, server_id: &str) -> BufferId {
        let name = lsp_server_trace_log_name(server_id);
        self.ensure_lsp_log_owned_buffer(&name, lattice_lsp::modes::LspTraceLogMode::mode_id())
    }

    /// Shared create-or-find path. Look the buffer up by its
    /// synthetic `name`; if absent, spawn a fresh empty Document,
    /// register it with `name = Some(name)`, and activate
    /// `major_id` on it.
    fn ensure_lsp_log_owned_buffer(
        &mut self,
        name: &str,
        major_id: lattice_mode::ModeId,
    ) -> BufferId {
        if let Some(id) = self.buffers.by_name(name) {
            return id;
        }
        let id = BufferId::next();
        let document = lattice_core::Document::empty();
        let handle = lattice_runtime::spawn_document(document, self.registry.clone());
        self.buffers.insert(BufferEntry {
            id,
            // Synthetic LSP log buffers are unlisted (vim's
            // `nobuflisted` semantic): `:bn`/`:bp` skip them, `:ls`
            // shows them with a `u` marker, but `:b <name>` and the
            // `:b` picker still reach them. Keeps the cycle order
            // focused on user-opened files while preserving direct
            // access to the synthetic surface.
            flags: BufferFlags {
                listed: false,
                hidden: false,
            },
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
        // and would pick `text-mode` instead of `lsp-log-mode`.
        self.activate_major_by_id(id, major_id);
        id
    }

    /// Activate `major_id` on `buffer_id` directly, bypassing the
    /// language-detection path. Used by synthetic-buffer creators
    /// that already know which major mode they want.
    pub(crate) fn activate_major_by_id(
        &mut self,
        buffer_id: BufferId,
        major_id: lattice_mode::ModeId,
    ) {
        let proto_id = lattice_protocol::ids::BufferId::new(buffer_id.0 as u64);
        let mut active = self.active_modes.remove(&buffer_id).unwrap_or_default();
        let mut locals = self.buffer_locals.remove(&buffer_id).unwrap_or_default();
        if let Err(e) = self.mode_registry.activate_major(
            &mut active,
            &mut locals,
            &self.config,
            &self.event_bus,
            &self.services,
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
        self.active_modes.insert(buffer_id, active);
        self.buffer_locals.insert(buffer_id, locals);
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
        let Some(entry) = self.buffers.document(buffer_id) else {
            return;
        };
        let handle = entry.handle.clone();
        let snap = handle.snapshot();
        let last_line = crate::app::last_addressable_line(&snap.buffer);
        let line_len = crate::app::line_byte_len(&snap.buffer, last_line);
        let pos = Position::new(last_line, line_len);
        let edit = Edit::insert(pos, text);
        let _ = lattice_runtime::block_on(handle.apply_edit_batch(vec![edit]));
    }
}

/// Format one log record line for append to a synthetic LSP log
/// buffer. Mirrors the shape `lattice_lsp::help_views` uses for
/// snapshot rendering: `HH:MM:SS.mmm <level> <source>: <message>`,
/// with the server id prefixed in brackets when known. Trailing
/// newline is the caller's responsibility (the drain batches many
/// records into one buffer-append).
pub(crate) fn format_log_event_line(
    server_id: Option<&str>,
    level: &str,
    source: &str,
    message: &str,
) -> String {
    use std::time::SystemTime;
    let elapsed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok();
    let secs = elapsed.map(|d| d.as_secs()).unwrap_or(0);
    let ms = elapsed.map(|d| d.subsec_millis()).unwrap_or(0);
    let hh = (secs / 3600) % 24;
    let mm = (secs / 60) % 60;
    let ss = secs % 60;
    let prefix = server_id.map(|id| format!("[{id}] ")).unwrap_or_default();
    let msg = one_line(message);
    format!("{hh:02}:{mm:02}:{ss:02}.{ms:03} {prefix}{level} {source:>6}: {msg}")
}

/// Collapse newlines / carriage returns / tabs into spaces so the
/// formatted record fits on one buffer line. Mirrors
/// `lattice_lsp::help_views::one_line`.
fn one_line(s: &str) -> String {
    s.replace(['\n', '\r', '\t'], " ")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::app::test_helpers::app_with;
    use crate::buffer_registry::BufferData;
    use lattice_config::ReadOnly;

    #[test]
    fn boot_creates_subsystem_lsp_buffer() {
        let a = app_with("hi", 5);
        let id = a
            .buffers
            .by_name(LSP_SUBSYSTEM_LOG_NAME)
            .expect("`*lsp*` buffer present at boot");
        // Must be a Document (slice B requirement) with the
        // synthetic name set.
        let entry = a.buffers.get(id).expect("entry registered");
        assert!(matches!(entry.data, BufferData::Document(_)));
        assert_eq!(entry.name.as_deref(), Some(LSP_SUBSYSTEM_LOG_NAME));
        // And unlisted -- `:bn` cycles skip it.
        assert!(!entry.flags.listed);
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

    #[test]
    fn lsp_log_drain_appends_to_subsystem_buffer() {
        let mut a = app_with("hi", 5);
        let id = a.buffers.by_name(LSP_SUBSYSTEM_LOG_NAME).unwrap();
        let before = a.buffers.document(id).unwrap().handle.text();
        a.lsp_logger.log(
            None,
            lattice_lsp::LogLevel::Info,
            lattice_lsp::LogSource::Client,
            "boot-time chatter",
        );
        a.drain_lsp_log_events();
        let after = a.buffers.document(id).unwrap().handle.text();
        assert!(after.len() > before.len());
        assert!(
            after.contains("boot-time chatter"),
            "subsystem log must capture server_id=None records; got:\n{after}"
        );
    }

    #[test]
    fn lsp_trace_toggle_creates_trace_buffer() {
        let mut a = app_with("hi", 5);
        assert!(a.buffers.by_name("*lsp:rust:trace*").is_none());
        // We can't drive `:lsp-trace rust` end-to-end without a
        // matching config; call the ensure helper directly to
        // exercise the slice's create path.
        let id = a.ensure_lsp_server_trace_buffer("rust");
        assert_eq!(a.buffers.by_name("*lsp:rust:trace*"), Some(id));
        // Trace buffer also read-only via lsp-trace-log-mode.
        let ro = *a.resolved_option::<ReadOnly>(id);
        assert!(
            ro,
            "trace buffer must resolve ReadOnly = true via lsp-trace-log-mode"
        );
    }
}
