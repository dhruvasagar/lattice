//! Host-owned `BufferRegistry`-backed impls of the diff subsystem's
//! resolver seams (DX.6, coupling C6).
//!
//! The diff subsystem (now `lattice-diff`) depends only on the
//! abstractions [`BufferTextProvider`] / [`DocumentBufferResolver`].
//! Their production impls bridge to [`crate::buffer_registry::BufferRegistry`]
//! — a host type — so they CANNOT live in `lattice-diff`; they stay here.
//! Re-exported under `crate::diff::subsystem::*` (see `diff/mod.rs`) so
//! every existing call site reads
//! `crate::diff::subsystem::BufferRegistry{TextProvider,DocumentResolver}`
//! exactly as before the move.

use lattice_core::BufferId;
use lattice_protocol::ids::DocumentId;
use ropey::Rope;

use lattice_diff::subsystem::{BufferTextProvider, DocumentBufferResolver};

/// D.3.a (2026-05-29): the production [`BufferTextProvider`]
/// impl. Bridges the trait to the host's [`crate::buffer_registry::BufferRegistry`]:
/// `buffer_rope(id)` walks `BufferRegistry::document_handle(id)
/// -> RopeDocumentHandle::snapshot() -> snapshot.buffer.to_rope()`.
///
/// All operations are RCU-style reads (registry mutex held only
/// long enough to clone an `Arc<DocumentSnapshot>`; rope clone
/// is `Arc`-share of chunks). Safe to call from
/// `spawn_blocking`. Returns `None` for non-document buffers
/// or for ids the registry has dropped — the diff subsystem's
/// `BufferSource` impl maps `None` to an empty rope per its
/// documented contract.
#[derive(Clone, Debug)]
pub struct BufferRegistryTextProvider {
    registry: crate::buffer_registry::BufferRegistry,
}

impl BufferRegistryTextProvider {
    pub fn new(registry: crate::buffer_registry::BufferRegistry) -> Self {
        Self { registry }
    }
}

impl BufferTextProvider for BufferRegistryTextProvider {
    fn buffer_rope(&self, id: BufferId) -> Option<Rope> {
        let handle = self.registry.document_handle(id)?;
        Some(handle.snapshot().buffer.to_rope())
    }
}

/// D.3.a.1 (2026-05-29): production [`DocumentBufferResolver`]
/// impl. Bridges `DocumentId` → `BufferId` via
/// `BufferRegistry::buffer_id_for_document`. Stored on `Editor`
/// for the editor's lifetime and handed to
/// `DiffSubsystem::bind` so the drainer task can translate
/// bus events.
#[derive(Clone, Debug)]
pub struct BufferRegistryDocumentResolver {
    registry: crate::buffer_registry::BufferRegistry,
}

impl BufferRegistryDocumentResolver {
    pub fn new(registry: crate::buffer_registry::BufferRegistry) -> Self {
        Self { registry }
    }
}

impl DocumentBufferResolver for BufferRegistryDocumentResolver {
    fn buffer_id_for(&self, document_id: DocumentId) -> Option<BufferId> {
        self.registry.buffer_id_for_document(document_id)
    }
}
