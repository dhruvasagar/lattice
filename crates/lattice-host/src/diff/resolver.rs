//! The host-owned `BufferRegistry`-backed impl of the diff subsystem's
//! [`BufferTextProvider`] seam (DX.6, coupling C6).
//!
//! The diff subsystem (now `lattice-diff`) depends only on the
//! abstraction. Its production impl bridges to
//! [`crate::buffer_registry::BufferRegistry`] — a host type — so it CANNOT
//! live in `lattice-diff`; it stays here, re-exported under
//! `crate::diff::subsystem::*` (see `diff/mod.rs`).
//!
//! The peer `DocumentBufferResolver` needs no registry: a document's id is
//! its buffer's id, so `lattice_diff::subsystem::DocumentIdIsBufferId`
//! converts one to the other.

use lattice_core::BufferId;
use ropey::Rope;

use lattice_diff::subsystem::BufferTextProvider;

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
