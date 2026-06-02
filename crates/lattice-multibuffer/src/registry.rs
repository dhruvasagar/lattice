//! M.2.b.2 (2026-06-01): typed handle lookup for multibuffer views.
//!
//! `BufferStore::handle_for(id)` returns `Arc<dyn Document>` (the
//! kind-agnostic dispatch surface). Providers that need to call
//! typed methods on the multibuffer handle (`append_excerpts`,
//! `replace_excerpts`, `set_headerline` once M.4 lands) reach the
//! concrete `Arc<MultibufferDocumentHandle>` through this
//! registry — keeps `Document` clean (no `Any` / downcast
//! contamination) and isolates the multibuffer-specific lookup to
//! this crate.
//!
//! Same precedent shape as `lattice_terminal::TerminalStoreHandle`.
//!
//! Cleanup runs through a `DocumentClosed` typed-event subscriber
//! wired in [`crate::mode::register_multibuffer_modes`]; when a
//! multibuffer view's underlying buffer closes, the registry
//! entry is removed.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use lattice_core::BufferId;
use lattice_protocol::ids::DocumentId;

use crate::MultibufferDocumentHandle;

/// Typed handle lookup for active multibuffer views, keyed by
/// the view buffer's `BufferId`. Registered as a service in
/// `ServiceRegistry` at boot.
pub trait MultibufferRegistry: Send + Sync + std::fmt::Debug {
    /// Return the typed handle for `view` if registered, else
    /// `None`. Cheap-clone: `Arc::clone`.
    fn handle(&self, view: BufferId) -> Option<Arc<MultibufferDocumentHandle>>;

    /// Register `handle` against `view`. Called by
    /// `create_multibuffer_view` after the handle is built.
    /// Overwrites if the view id already had a handle (last-write-
    /// wins; production code allocates fresh ids per view so a
    /// collision is a developer bug, not a hot-swap).
    fn insert(&self, view: BufferId, handle: Arc<MultibufferDocumentHandle>);

    /// Remove the entry for `view`. Idempotent: removing a
    /// non-existent entry is a no-op.
    fn remove(&self, view: BufferId);

    /// Remove the entry whose handle reports `document_id` as
    /// its `MultibufferDocumentHandle::document_id`. Called by
    /// the `DocumentClosed` subscriber (the event payload carries
    /// `DocumentId`, not `BufferId`). Returns `true` if an entry
    /// was removed. `O(n)` over active views; multibuffer counts
    /// are small so the walk is fine.
    fn remove_by_document_id(&self, document_id: DocumentId) -> bool;

    /// Count of currently-registered views. Test-friendly probe.
    fn len(&self) -> usize;
}

/// Cheap-clone Arc'd alias matching the existing service-handle
/// convention (`BufferStoreHandle`, `LspSupervisorHandle`,
/// `TerminalStoreHandle`).
pub type MultibufferRegistryHandle = Arc<dyn MultibufferRegistry>;

/// Default in-memory `MultibufferRegistry` implementation. One
/// `RwLock<HashMap>` indexed by view BufferId. Lookups take the
/// read lock (no contention with parallel readers); insert /
/// remove take the write lock (rare paths — view creation + view
/// close).
#[derive(Debug, Default)]
pub struct InMemoryMultibufferRegistry {
    inner: RwLock<HashMap<BufferId, Arc<MultibufferDocumentHandle>>>,
}

impl InMemoryMultibufferRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct the registry as a service handle (cheap-clone
    /// Arc'd trait object) ready for `ServiceRegistry::register`.
    pub fn handle() -> MultibufferRegistryHandle {
        Arc::new(Self::new())
    }
}

impl MultibufferRegistry for InMemoryMultibufferRegistry {
    fn handle(&self, view: BufferId) -> Option<Arc<MultibufferDocumentHandle>> {
        self.inner
            .read()
            .expect("MultibufferRegistry RwLock poisoned")
            .get(&view)
            .cloned()
    }

    fn insert(&self, view: BufferId, handle: Arc<MultibufferDocumentHandle>) {
        self.inner
            .write()
            .expect("MultibufferRegistry RwLock poisoned")
            .insert(view, handle);
    }

    fn remove(&self, view: BufferId) {
        self.inner
            .write()
            .expect("MultibufferRegistry RwLock poisoned")
            .remove(&view);
    }

    fn remove_by_document_id(&self, document_id: DocumentId) -> bool {
        let mut guard = self
            .inner
            .write()
            .expect("MultibufferRegistry RwLock poisoned");
        let Some(view) = guard
            .iter()
            .find(|(_, h)| h.document_id() == document_id)
            .map(|(view, _)| *view)
        else {
            return false;
        };
        guard.remove(&view).is_some()
    }

    fn len(&self) -> usize {
        self.inner
            .read()
            .expect("MultibufferRegistry RwLock poisoned")
            .len()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn empty_handle() -> Arc<MultibufferDocumentHandle> {
        Arc::new(MultibufferDocumentHandle::empty(Arc::new(
            lattice_grammar::CommandRegistry::new(),
        )))
    }

    #[test]
    fn insert_and_lookup_roundtrip() {
        let reg = InMemoryMultibufferRegistry::new();
        let handle = empty_handle();
        let id = handle.buffer_id();
        reg.insert(id, handle.clone());
        let looked_up = reg.handle(id).unwrap();
        assert!(Arc::ptr_eq(&handle, &looked_up));
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn missing_lookup_returns_none() {
        let reg = InMemoryMultibufferRegistry::new();
        assert!(reg.handle(BufferId(99)).is_none());
    }

    #[test]
    fn remove_clears_entry() {
        let reg = InMemoryMultibufferRegistry::new();
        let handle = empty_handle();
        let id = handle.buffer_id();
        reg.insert(id, handle);
        assert_eq!(reg.len(), 1);
        reg.remove(id);
        assert_eq!(reg.len(), 0);
        assert!(reg.handle(id).is_none());
    }

    #[test]
    fn remove_missing_id_is_noop() {
        let reg = InMemoryMultibufferRegistry::new();
        reg.remove(BufferId(123));
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn remove_by_document_id_finds_and_drops_entry() {
        let reg = InMemoryMultibufferRegistry::new();
        let handle = empty_handle();
        let view = handle.buffer_id();
        let doc = handle.document_id();
        reg.insert(view, handle);
        assert!(reg.remove_by_document_id(doc));
        assert_eq!(reg.len(), 0);
        // Idempotent second call.
        assert!(!reg.remove_by_document_id(doc));
    }
}
