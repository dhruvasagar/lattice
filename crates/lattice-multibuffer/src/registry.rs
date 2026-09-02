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
            arc_swap::ArcSwap::from_pointee(lattice_grammar::CommandRegistry::new()),
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

/// OA.23 — the [`ExcerptSourceResolver`](lattice_core::ExcerptSourceResolver)
/// a multibuffer-aware host wires into the plugin host.
///
/// One handle, because a view answers the whole question: the registry maps a
/// view to its excerpts (composed line → source buffer + line), and every
/// source is an `Arc<dyn Document>` that carries its own path.
///
/// It first took a `BufferStoreHandle` too, on the reasoning that the registry
/// could say *which buffer* and never *which file*. That was wrong, and wrong
/// in the way that costs a whole slice: a **scan view** — the agenda — mints
/// its sources with `BufferId::next()` and hands them to `add_source`, never to
/// `BufferStore::insert_document_buffer`, because they are not buffers the user
/// opened. So the store answered `none` for every agenda row, and the only
/// tests were the two `none` paths, which pass either way. See
/// `a_scan_views_source_resolves_by_the_documents_own_path`.
///
/// Lives here rather than in the plugin host because this crate owns the
/// composed→source translation; the host cannot depend on it (layering), which
/// is why the trait is abstract in `lattice-core` at all.
#[derive(Clone)]
pub struct MultibufferExcerptSource {
    views: MultibufferRegistryHandle,
}

impl MultibufferExcerptSource {
    pub fn new(views: MultibufferRegistryHandle) -> Self {
        Self { views }
    }
}

impl std::fmt::Debug for MultibufferExcerptSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Neither handle has a useful Debug and both are shared state; the
        // trait requires one, so this says what it is and stops.
        f.write_str("MultibufferExcerptSource")
    }
}

impl lattice_core::ExcerptSourceResolver for MultibufferExcerptSource {
    fn excerpt_source(&self, buffer: BufferId, line: u32) -> Option<(std::path::PathBuf, u32)> {
        // Not a multibuffer: `none`, not the buffer's own path. The question is
        // "which file does this COMPOSED line come from", and a caller wanting
        // the current file already has one.
        let view = self.views.handle(buffer)?;
        // Byte 0 — the translation is line-wise and the caller asked about a
        // line. Passing a real column would invite the answer to depend on it.
        let (source, position) =
            view.translate_composed_to_source(crate::Position { line, byte: 0 })?;
        // The VIEW's source map, not the buffer store. A scan view mints its
        // sources with `BufferId::next()` and registers them with `add_source`
        // alone — they are not buffers the user opened and have no business in
        // `:ls`, so the store has never heard of them. Asking the store was
        // this seam's original shape and it answered `none` for every real
        // agenda row: the one case it exists for.
        let path = view.source_path(source)?;
        Some((path, position.line))
    }
}

#[cfg(test)]
mod excerpt_source_tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use lattice_core::ExcerptSourceResolver;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn a_buffer_that_is_not_a_multibuffer_answers_none() {
        // Not its own path: the question is "which file does this COMPOSED
        // line come from", and a caller wanting the current file already has
        // `document.path()`. Answering the view's own path here would make the
        // agenda's synthetic buffer look like a file on disk.
        let views = InMemoryMultibufferRegistry::handle();
        let r = MultibufferExcerptSource::new(views);
        assert_eq!(r.excerpt_source(BufferId::next(), 0), None);
    }

    #[test]
    fn an_unwired_line_beyond_every_excerpt_answers_none() {
        // A cursor can be anywhere, including past the last row. That is an
        // ordinary answer rather than an error, which is why the seam returns
        // an option rather than a result.
        let views = InMemoryMultibufferRegistry::handle();
        let r = MultibufferExcerptSource::new(views);
        assert_eq!(r.excerpt_source(BufferId::next(), 9_999), None);
    }

    /// The case the seam exists for, and the one the two `None` tests above
    /// could not catch: a view built the way a **scan view** builds one.
    ///
    /// `scan_view::append_sorted` mints its source ids with `BufferId::next()`
    /// and hands the documents to `add_source` — the multibuffer's own source
    /// map. It never calls `BufferStore::insert_document_buffer`, because a
    /// scan's sources are not buffers the user opened and have no business in
    /// `:ls`. So a resolver asking the BUFFER STORE for the path answers `none`
    /// for every real agenda row, which is the only shape anybody asks about.
    ///
    /// The source document carries its own path, so the view alone can answer.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_scan_views_source_resolves_by_the_documents_own_path() {
        let source = BufferId::next();
        let doc = lattice_core::DocumentBuilder::default()
            .with_text("* TODO write it\n  SCHEDULED: <2026-09-03>\n* TODO and it\n")
            .with_path(PathBuf::from("/org/notes.org"))
            .build();
        let registry = Arc::new(arc_swap::ArcSwap::from_pointee(
            lattice_grammar::CommandRegistry::new(),
        ));
        let handle: Arc<dyn lattice_runtime::Document> = Arc::new(lattice_runtime::spawn_document(
            source,
            doc,
            Arc::clone(&registry),
        ));

        // Excerpt the third line, so a wrong answer cannot pass by returning 0.
        let mb = Arc::new(
            crate::MultibufferDocumentHandle::new(
                HashMap::from([(source, Arc::clone(&handle))]),
                vec![crate::Excerpt::new(source, 2, 2)],
                registry,
            )
            .expect("view builds"),
        );
        let view = BufferId::next();
        let views = InMemoryMultibufferRegistry::handle();
        views.insert(view, mb);

        let r = MultibufferExcerptSource::new(views);
        assert_eq!(
            r.excerpt_source(view, 0),
            Some((PathBuf::from("/org/notes.org"), 2)),
        );
    }
}
