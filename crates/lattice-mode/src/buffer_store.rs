//! `BufferStore`: host primitive for mode-owned buffer lifecycles.
//!
//! Modes that synthesize their own buffers (LSP log family,
//! `*messages*`, future `*scratch*`, plugin-installed log streams)
//! need a thread-safe way to:
//!
//! - Find a buffer by its synthetic name (idempotency check).
//! - Create a new Document with a given major mode active on it.
//! - Look up a Document's handle so the mode can append to it from
//!   a background tokio task.
//!
//! Why a trait + handle pair: the concrete buffer registry lives
//! in the renderer crate (`lattice-ui-tui::buffer_registry`), but
//! mode crates (`lattice-lsp`, eventually plugin modes) shouldn't
//! depend on the renderer. The trait carries the host-side
//! contract; the renderer registers an `Arc<dyn BufferStore>` into
//! [`crate::ServiceRegistry`] at App boot; modes pull
//! `Arc<BufferStoreHandle>` via `ctx.service::<BufferStoreHandle>()`
//! and call through it.
//!
//! ## Threading model
//!
//! Operations on the trait are `&self`; the implementation is
//! responsible for whatever synchronisation it needs (the
//! `lattice-ui-tui` impl wraps the relevant App state in
//! `Arc<Mutex<...>>`). Modes can call from any thread — the App
//! thread inside `on_activate`, a tokio task draining an event
//! subscription, etc.
//!
//! ## Mutation surface
//!
//! Only three operations:
//!
//! - [`BufferStore::find_by_name`] — read-only registry probe.
//! - [`BufferStore::ensure_named_document`] — find-or-create. If
//!   creating, allocates a fresh `RopeDocumentHandle`, registers the
//!   buffer with `name = Some(name.into())`, activates `major` as
//!   the buffer's major mode (which transitively runs the major's
//!   `on_activate`), and returns the id.
//! - [`BufferStore::handle_for`] — clone of the actor handle so a
//!   mode can write to the buffer from outside the App's borrow.
//!
//! Mode authors call these from their lifecycle hooks. The
//! mode-ownership contract: a mode that creates a synthetic
//! buffer is the only thing that writes to it (subsystem writes
//! via the handle; user writes are gated by `ReadOnly`).

use std::sync::Arc;

use lattice_core::{BufferFlags, BufferId, BufferKind};

use crate::ModeId;

/// Host-implemented trait. The renderer crate provides an impl
/// that wraps the App's buffer registry + mode-activation state.
///
/// Methods take `&self` and must be safe to call from any thread
/// (the impl is responsible for synchronisation).
pub trait BufferStore: Send + Sync {
    /// Look up the buffer id whose `name` matches exactly.
    /// Returns `None` when no buffer with that synthetic name is
    /// registered yet.
    fn find_by_name(&self, name: &str) -> Option<BufferId>;

    /// Find-or-create a Document buffer with the given synthetic
    /// `name` and `major` mode. Idempotent: a second call with
    /// the same name returns the existing id, regardless of
    /// whether `major` matches (the existing buffer's major is
    /// not overwritten).
    ///
    /// On first creation:
    /// 1. Allocate a `BufferId::next()`.
    /// 2. Spawn an empty `Document` and register a
    ///    `BufferEntry { id, flags, data: Document, name: Some(name) }`
    ///    in the buffer registry.
    /// 3. Activate `major` on the new buffer (runs the major's
    ///    `on_activate` via the mode registry; recomputes the
    ///    buffer's resolved-options cache).
    /// 4. Return the id.
    fn ensure_named_document(&self, name: &str, major: ModeId, flags: BufferFlags) -> BufferId;

    /// Get a clone of the `Arc<dyn Document>` for `id`,
    /// suitable for holding across thread boundaries. M.0: the
    /// return type is the polymorphic shape so the same
    /// surface serves both regular-document handles and (M.1+)
    /// multibuffer handles uniformly. The handle is the only
    /// way for a mode-owned background task to write into the
    /// buffer (`handle.apply_edit_batch(...)`).
    ///
    /// `None` when `id` is not a Document in the registry.
    fn handle_for(&self, id: BufferId) -> Option<Arc<dyn lattice_runtime::Document>>;

    /// Read the buffer's synthetic name. `None` when the buffer is
    /// unnamed (the default for path-less scratch documents) or
    /// when `id` is not registered. B'.7: lets a mode derive its
    /// own identity from the buffer it's attached to without the
    /// host having to seed a buffer-local first.
    fn name_for(&self, id: BufferId) -> Option<String>;

    /// **H.1 (2026-05-31): generic Document-shaped buffer
    /// insertion.** Used by extension crates (`lattice-multibuffer`
    /// today; future plugin-defined Document-shaped kinds) to
    /// push a `BufferEntry` into the registry without host
    /// knowing the kind exists.
    ///
    /// `kind` must be a Document-shaped kind whose payload is an
    /// `Arc<dyn Document>`: today `BufferKind::Document` /
    /// `BufferKind::Messages` / `BufferKind::Multibuffer`. For
    /// other kinds (`FileTree`, `Oil`, `Terminal`, `Help`) the
    /// payload is structurally different and they keep their
    /// host-internal insertion paths until the v2 plugin-
    /// architecture work designs the generic extension point
    /// (per `docs/dev/architecture/kind-agnostic-buffers.md`
    /// §8 Q2).
    ///
    /// Idempotent: if `id` is already registered the call is a
    /// no-op (consistent with `ensure_named_document` for the
    /// named-singleton case). The caller is responsible for
    /// allocating `id` via `BufferId::next()` before calling.
    ///
    /// Errors fold into the impl's logging path (e.g. the
    /// `lattice-host` impl logs through `tracing` and emits a
    /// host-side message); the trait surface is infallible
    /// because every error here is a programmer error (wrong
    /// kind, duplicate id) and there's no recovery path the
    /// caller could enact.
    fn insert_document_buffer(
        &self,
        id: BufferId,
        kind: BufferKind,
        handle: Arc<dyn lattice_runtime::Document>,
        flags: BufferFlags,
        name: Option<String>,
    );
}

/// Concrete service-registry-friendly wrapper around
/// `Arc<dyn BufferStore>`. Modes register interest by pulling
/// `Arc<BufferStoreHandle>` from
/// [`crate::ServiceRegistry::get::<BufferStoreHandle>()`].
///
/// Cheap to clone — internal `Arc<dyn BufferStore>` is one atomic
/// bump.
#[derive(Clone)]
pub struct BufferStoreHandle {
    inner: Arc<dyn BufferStore>,
}

impl BufferStoreHandle {
    pub fn new(inner: Arc<dyn BufferStore>) -> Self {
        Self { inner }
    }

    pub fn find_by_name(&self, name: &str) -> Option<BufferId> {
        self.inner.find_by_name(name)
    }

    pub fn ensure_named_document(&self, name: &str, major: ModeId, flags: BufferFlags) -> BufferId {
        self.inner.ensure_named_document(name, major, flags)
    }

    pub fn handle_for(&self, id: BufferId) -> Option<Arc<dyn lattice_runtime::Document>> {
        self.inner.handle_for(id)
    }

    pub fn name_for(&self, id: BufferId) -> Option<String> {
        self.inner.name_for(id)
    }

    /// H.1 pass-through wrapper. See
    /// [`BufferStore::insert_document_buffer`].
    pub fn insert_document_buffer(
        &self,
        id: BufferId,
        kind: BufferKind,
        handle: Arc<dyn lattice_runtime::Document>,
        flags: BufferFlags,
        name: Option<String>,
    ) {
        self.inner
            .insert_document_buffer(id, kind, handle, flags, name)
    }
}

impl std::fmt::Debug for BufferStoreHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufferStoreHandle").finish_non_exhaustive()
    }
}
