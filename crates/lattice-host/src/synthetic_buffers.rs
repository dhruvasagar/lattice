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
    ephemeral: false,
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
        if let Err(e) = self.mode_registry.load_full().activate_major(
            &mut active,
            &self.mode_guards,
            &self.config,
            &self.event_bus,
            &self.services,
            proto_id,
            major_id,
            self.capabilities_for_proto(proto_id),
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

    /// Replace the ENTIRE content of an owner-written buffer with `text` (one
    /// full-range edit). The idempotent counterpart to
    /// [`Self::append_to_owned_buffer`]: a synthetic buffer opened by name is
    /// reused across calls, so a re-render (e.g. `:export-plugin-api` a second
    /// time) must overwrite rather than append. Bypasses the read-only
    /// dispatcher, exactly like the append path (owner writes).
    pub fn replace_owned_buffer(&mut self, buffer_id: BufferId, text: &str) {
        let Some(handle) = self.buffers.document_handle(buffer_id) else {
            return;
        };
        let snap = handle.snapshot();
        // The TRUE end of the buffer -- `last_addressable_line` deliberately
        // backs up past a trailing-newline line, which would leave that line
        // uncovered here (and on a reused buffer it collapses to line 0). A
        // full replace must span every line, phantom trailing line included.
        // CV.3: ROPE space, and the comment above says why — a full replace
        // must span the phantom trailing line too, or it leaves it behind.
        let lc = snap.buffer.rope_line_count();
        let last_line = lc.saturating_sub(1);
        let line_len = snap.buffer.line_byte_len(last_line);
        let whole = lattice_protocol::position::Range {
            start: Position::new(0, 0),
            end: Position::new(last_line, line_len),
        };
        let edit = Edit::replace(whole, text);
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

    /// PU-B.2: idempotently ensure a named popup buffer under `major_id`,
    /// stored as [`BufferData::Help`] so the popup renderer draws it (the
    /// renderer's popup path is `BufferData::Help`-gated) while `major_id`
    /// (e.g. `ai-permission-mode`) owns the buffer's behaviour + keymap. The
    /// content is empty on creation — the owning mode's `on_activate`
    /// owner-writes the projection. Returns the existing id when a buffer of
    /// this `name` is already registered (re-open reuses it).
    pub fn ensure_named_popup_buffer(
        &mut self,
        name: &str,
        major_id: ModeId,
        flags: BufferFlags,
    ) -> BufferId {
        self.ensure_named_synthetic_doc_with_variant(
            name,
            major_id,
            flags,
            SyntheticDocVariant::Help,
        )
    }

    /// DL.4/DL.5: mint an empty actor-backed Document filed under a
    /// listing kind.
    ///
    /// The listing peer of [`Self::register_help_document`] — same
    /// PU.1a shape (a `DocumentEntry` behind a kind discriminator),
    /// minus the help metadata. The caller seeds the text through the
    /// entries chokepoint, which also publishes the icons, so the rope
    /// and the virtual text cannot drift apart.
    ///
    /// DL.5 generalised it to oil once that kind converged too.
    pub fn register_listing_document(
        &mut self,
        flags: BufferFlags,
        kind: crate::buffer_registry::ListingKind,
    ) -> BufferId {
        let id = BufferId::next();
        let document = Document::empty();
        let handle = spawn_document(id, document, self.registry.clone());
        let handle: std::sync::Arc<dyn lattice_runtime::Document> = std::sync::Arc::new(handle);
        let entry = DocumentEntry { id, handle };
        self.buffers.insert(BufferEntry {
            id,
            flags,
            data: match kind {
                crate::buffer_registry::ListingKind::FileTree => BufferData::FileTree(entry),
                crate::buffer_registry::ListingKind::Oil => BufferData::Oil(entry),
            },
            name: None,
        });
        self.seed_empty_document_locals(id);
        id
    }

    /// PU.1a: register a freshly-built [`lattice_help::HelpContent`]
    /// as an actor-backed synthetic Document ([`BufferData::Help`]),
    /// seeded with the content's text + parsed metadata. Returns the
    /// new [`BufferId`]. This is the single creation path help shares
    /// with `*messages*` and the LSP logs — content lives once in the
    /// Document; the title goes to the registry `name` slot; links /
    /// anchors / highlights go to `buffer_locals`. The caller owns the
    /// popup/pane focus wiring + mode activation.
    pub fn register_help_document(
        &mut self,
        content: lattice_help::HelpContent,
        flags: BufferFlags,
    ) -> BufferId {
        let lattice_help::HelpContent { buffer, metadata } = content;
        let id = BufferId::next();
        let document = Document::empty();
        let handle = spawn_document(id, document, self.registry.clone());
        let handle: std::sync::Arc<dyn lattice_runtime::Document> = std::sync::Arc::new(handle);
        self.buffers.insert(BufferEntry {
            id,
            flags,
            data: BufferData::Help(DocumentEntry { id, handle }),
            name: Some(buffer.title),
        });
        self.seed_empty_document_locals(id);
        // Seed the help text into the actor. A fresh document is
        // empty, so an end-of-buffer append (which bypasses the
        // read-only dispatcher, exactly like the messages backlog
        // seed) lands the text at the top.
        let text = buffer.content.as_string();
        if !text.is_empty() {
            self.append_to_owned_buffer(id, &text);
        }
        // PU.1b-2b: the markdown `SyntaxHandle` (path `help.md` ⇒
        // `Lang::Markdown`) + the link `ExtraHighlights` are both
        // attached/seeded by `seed_help_metadata_locals` below, the
        // single point that ALSO fires on every swap — so the matrix's
        // grammar colour and link styling stay fresh across back-stack /
        // link-follow / in-pane re-seed without a bespoke re-attach.
        self.seed_help_metadata_locals(id, metadata);
        id
    }

    /// DB.2: create the `*dashboard*` buffer. Identical to
    /// [`Self::register_help_document`] except the buffer data is
    /// [`BufferData::Dashboard`] (so `:ls` / introspection tell it apart and
    /// the follow gates group it with help), and `dashboard-mode` is the
    /// major (assigned by the caller). Reuses the help metadata seed for the
    /// markdown `SyntaxHandle` + link `ExtraHighlights`, so `<CR>`-follow
    /// works through the shared help mechanism.
    pub fn register_dashboard_document(
        &mut self,
        content: lattice_help::HelpContent,
        flags: BufferFlags,
    ) -> BufferId {
        let lattice_help::HelpContent { buffer, metadata } = content;
        let id = BufferId::next();
        let document = Document::empty();
        let handle = spawn_document(id, document, self.registry.clone());
        let handle: std::sync::Arc<dyn lattice_runtime::Document> = std::sync::Arc::new(handle);
        self.buffers.insert(BufferEntry {
            id,
            flags,
            data: BufferData::Dashboard(DocumentEntry { id, handle }),
            name: Some(buffer.title),
        });
        self.seed_empty_document_locals(id);
        let text = buffer.content.as_string();
        if !text.is_empty() {
            self.append_to_owned_buffer(id, &text);
        }
        self.seed_help_metadata_locals(id, metadata);
        id
    }

    /// PU.1a: replace the entire content of an owned synthetic
    /// Document at `id` with `text` (bypasses the read-only
    /// dispatcher, like [`Self::append_to_owned_buffer`]). Used by
    /// the popup back-stack / link-follow swap paths to re-seed a
    /// help Document in place without changing its `BufferId`.
    /// No-op when `id` is not a Document.
    pub fn replace_owned_document_text(&mut self, id: BufferId, text: &str) {
        let Some(handle) = self.buffers.document_handle(id) else {
            return;
        };
        let snap = handle.snapshot();
        let last_line = last_addressable_line(&snap.buffer);
        let line_len = snap.buffer.line_byte_len(last_line);
        let range = lattice_protocol::position::Range::new(
            Position::ZERO,
            Position::new(last_line, line_len),
        );
        let _ = lattice_runtime::block_on(
            handle.apply_edit_batch(vec![Edit::replace(range, text.to_string())]),
        );
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
        let handle: std::sync::Arc<dyn lattice_runtime::Document> = std::sync::Arc::new(handle);
        let data = match variant {
            SyntheticDocVariant::Document => BufferData::Document(DocumentEntry { id, handle }),
            SyntheticDocVariant::Messages => BufferData::Messages(DocumentEntry { id, handle }),
            SyntheticDocVariant::Help => BufferData::Help(DocumentEntry { id, handle }),
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
    /// [`BufferData::Help`] — the popup-renderable variant (PU-B.2 popup menus).
    Help,
}
