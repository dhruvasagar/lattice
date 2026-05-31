//! Synthetic-buffer management on `Editor`.
//!
//! Phase 5.7.B.9: migrates the synthetic-buffer plumbing
//! (`SYNTHETIC_BUFFER_FLAGS`, `seed_empty_document_locals`,
//! `activate_major_by_id`, `append_to_owned_buffer`,
//! `ensure_named_synthetic_document`) from `impl App` (TUI,
//! `lattice-ui-tui::app::lsp_log_buffers` +
//! `lattice-ui-tui::app::lifecycle`) to `impl Editor` (host).
//!
//! The TUI peer keeps thin wrappers so existing call sites
//! (`App::new` boot, ex-command handlers like
//! `do_open_messages`, `:b *lsp*` ensure paths) stay compiling
//! while both renderer peers reach the same canonical bodies.
//!
//! ## What synthetic buffers are
//!
//! "Synthetic" = subsystem-owned Document buffers that don't
//! correspond to an on-disk file. Examples:
//!
//! - `*lsp*` -- the global LSP subsystem log (one per editor).
//! - `*lsp:<server>:<workspace>*` -- per-LSP-instance log.
//! - `*messages*` -- the editor's echo / `tracing::*` transcript.
//! - `*scratch*` (future) -- a user-visible scratch buffer.
//!
//! All of them register in the same [`crate::buffer_registry::BufferRegistry`]
//! keyed by `BufferId`, marked with [`SYNTHETIC_BUFFER_FLAGS`]
//! (`listed: false, hidden: false` -- skip from `:bn`/`:bp`
//! cycles, show in `:ls` with a marker, reach via `:b <name>`).
//!
//! Each synthetic buffer's content + lifecycle is driven by its
//! major mode (e.g. `messages-mode`, `lsp-log-mode`); the host
//! plumbing here is renderer-neutral.

use lattice_core::Document;
use lattice_mode::ModeId;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::Position;
use lattice_runtime::spawn_document;

use crate::action::EchoLevel;
use crate::buffer_registry::{BufferData, BufferEntry, DocumentEntry};
use crate::buffers::{BufferFlags, BufferId};
use crate::dispatch::last_addressable_line;
use crate::editor::Editor;

/// Unlisted, non-hidden flags -- the canonical shape every
/// mode-owned synthetic buffer wants. `:bn` / `:bp` cycles skip,
/// `:ls` shows with a `u` marker, `:b <name>` still reaches.
///
/// Phase 5.7.B.9: promoted from `pub(crate) const` on the TUI
/// `App` to a public `const` on the host substrate so both
/// renderer peers (and any synthetic-buffer creators in
/// subsystem crates) reach the same flag value.
pub const SYNTHETIC_BUFFER_FLAGS: BufferFlags = BufferFlags {
    listed: false,
    hidden: false,
};

impl Editor {
    /// Convenience accessor for [`SYNTHETIC_BUFFER_FLAGS`] from
    /// callers that already have `&Editor` in scope -- saves a
    /// `use` import for the const at every call site (boot,
    /// `ensure_messages_buffer`, the LSP log-buffer creators,
    /// the GPUI peer's `finalize_boot`, ...).
    pub const SYNTHETIC_BUFFER_FLAGS: BufferFlags = SYNTHETIC_BUFFER_FLAGS;

    /// M.3.2.c.5: seed an empty set of document mode-locals for a
    /// freshly-registered document buffer. Subsequent activation
    /// transitions read through these slots; if the slot is
    /// missing the accessor returns the type's natural default.
    /// Idempotent (replace-on-collision).
    ///
    /// Phase 5.7.B.9: migrated from
    /// `lattice-ui-tui::app::lifecycle::App::seed_empty_document_locals`.
    /// The body touches only renderer-neutral editor state
    /// (`buffer_locals`) + host-owned local types
    /// (`crate::modes::Document*`).
    pub fn seed_empty_document_locals(&mut self, buffer_id: BufferId) {
        let locals = self.buffer_locals.entry(buffer_id).or_default();
        locals.insert(crate::modes::DocumentSyntax(None));
        locals.insert(crate::modes::DocumentLastParsedTextVersion(0));
        locals.insert(crate::modes::DocumentLastSyncedSyntaxVersion(0));
        locals.insert(crate::modes::DocumentFolds(Vec::new()));
    }

    /// Activate `major_id` on `buffer_id` directly, bypassing the
    /// language-detection path. Used by synthetic-buffer creators
    /// that already know which major mode they want (LSP log
    /// buffers want `lsp-log-mode` / `lsp-trace-log-mode`;
    /// `*messages*` wants `messages-mode`).
    ///
    /// Errors from `mode_registry.activate_major` surface as an
    /// `EchoLevel::Warn` set_message; activation never panics on
    /// per-mode hook failures.
    ///
    /// Phase 5.7.B.9: migrated from
    /// `lattice-ui-tui::app::lsp_log_buffers::App::activate_major_by_id`.
    /// All host-callable; the `set_message` + `recompute_options_for_buffer`
    /// deps already live on `Editor`.
    pub fn activate_major_by_id(&mut self, buffer_id: BufferId, major_id: ModeId) {
        let proto_id = lattice_protocol::ids::BufferId::new(buffer_id.0 as u64);
        let mut active = self.active_modes.remove(&buffer_id).unwrap_or_default();
        if let Err(e) = self.mode_registry.activate_major(
            &mut active,
            &self.mode_guards,
            &self.config,
            &self.event_bus,
            &self.services,
            proto_id,
            major_id,
            lattice_mode::CapabilitySet::empty(),
        ) {
            self.set_message(
                EchoLevel::Warn,
                format!(
                    "mode: activate_major({}) for buffer {} failed: {}",
                    major_id, buffer_id.0, e,
                ),
            );
        }
        self.active_modes.insert(buffer_id, active);
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
    /// whose major mode (`lsp-log-mode` / `messages-mode` / ...)
    /// does not attach a syntax handle.
    ///
    /// No-op when `buffer_id` does not resolve to a Document in
    /// the registry, or when `text` is empty.
    ///
    /// Phase 5.7.B.9: migrated from
    /// `lattice-ui-tui::app::lsp_log_buffers::App::append_to_owned_buffer`.
    /// Uses host's [`last_addressable_line`] + `Buffer::line_byte_len`
    /// to compute the EOL insertion point.
    pub fn append_to_owned_buffer(&mut self, buffer_id: BufferId, text: &str) {
        if text.is_empty() {
            return;
        }
        let Some(handle) = self.buffers.document_handle(buffer_id) else {
            return;
        };
        let snap = handle.snapshot();
        let last_line = last_addressable_line(&snap.buffer);
        let line_len = snap.buffer.line_byte_len(last_line);
        let pos = Position::new(last_line, line_len);
        let edit = Edit::insert(pos, text);
        let _ = lattice_runtime::block_on(handle.apply_edit_batch(vec![edit]));
    }

    /// Find-or-create a Document buffer named `name`, register it
    /// in the registry with `flags`, seed empty document locals,
    /// and activate `major_id` directly (skipping the
    /// `activate_major_for_buffer_kind` language-detection path
    /// because synthetic buffers have no on-disk path).
    ///
    /// Returns the resolved [`BufferId`]; idempotent on subsequent
    /// calls (re-uses the existing entry).
    ///
    /// Activation runs the major's `on_activate` synchronously,
    /// so any subscription / spawn the mode does is in place by
    /// the time this function returns. The major mode is what
    /// derives the buffer's identity (instance key for LSP log
    /// variants); the host does not stash any subsystem-shaped
    /// buffer-local before activation.
    ///
    /// Phase 5.7.B.9: migrated from
    /// `lattice-ui-tui::app::lsp_log_buffers::App::ensure_named_synthetic_document`.
    pub fn ensure_named_synthetic_document(
        &mut self,
        name: &str,
        major_id: ModeId,
        flags: BufferFlags,
    ) -> BufferId {
        self.ensure_named_synthetic_doc_with_variant(
            name,
            major_id,
            flags,
            SyntheticDocVariant::Document,
        )
    }

    /// Same as [`Self::ensure_named_synthetic_document`] but
    /// inserts as [`BufferData::Messages`] so the kind tag is
    /// `BufferKind::Messages`. Used by `ensure_messages_buffer`
    /// so `:ls` / modeline / introspection can distinguish the
    /// transcript from user-edited documents.
    pub fn ensure_named_messages_document(
        &mut self,
        name: &str,
        major_id: ModeId,
        flags: BufferFlags,
    ) -> BufferId {
        self.ensure_named_synthetic_doc_with_variant(
            name,
            major_id,
            flags,
            SyntheticDocVariant::Messages,
        )
    }

    fn ensure_named_synthetic_doc_with_variant(
        &mut self,
        name: &str,
        major_id: ModeId,
        flags: BufferFlags,
        variant: SyntheticDocVariant,
    ) -> BufferId {
        if let Some(id) = self.buffers.by_name(name) {
            return id;
        }
        let id = BufferId::next();
        let document = Document::empty();
        let handle = spawn_document(id, document, self.registry.clone());
        // M.0: BufferRegistry stores `Arc<dyn Document>` so the
        // entry slot accepts either a regular handle (here) or
        // (M.1+) a multibuffer handle.
        let handle: std::sync::Arc<dyn lattice_runtime::Document> =
            std::sync::Arc::new(handle);
        let data = match variant {
            SyntheticDocVariant::Document => {
                BufferData::Document(DocumentEntry { id, handle })
            }
            SyntheticDocVariant::Messages => {
                BufferData::Messages(DocumentEntry { id, handle })
            }
        };
        self.buffers.insert(BufferEntry {
            id,
            flags,
            data,
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
}

/// Discriminator for `ensure_named_synthetic_doc_with_variant`:
/// which `BufferData` variant to use. Storage is identical
/// (`DocumentEntry`); only the kind tag differs.
enum SyntheticDocVariant {
    Document,
    Messages,
}
