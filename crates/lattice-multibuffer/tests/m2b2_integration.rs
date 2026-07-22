//! M.2.b.2 (2026-06-01) integration tests.
//!
//! Exercises the end-to-end contract documented in
//! `docs/dev/architecture/multibuffer-views.md` §3.7:
//!
//! - `create_multibuffer_view(activator, sources, excerpts, name, flags)`
//!   atomically inserts the buffer + registers the typed handle +
//!   activates the major.
//! - The typed handle is reachable post-create via
//!   `MultibufferRegistryHandle::handle(view_id)`.
//! - Empty inputs are valid (async-provider pattern).
//! - `append_excerpts` extends an empty view in place.
//! - The activator records each `activate_major_for_kind` call
//!   so the test can assert the activation step fired with the
//!   right kind.
//!
//! Uses a `MockActivator` instead of spinning a full
//! `lattice_host::Editor` — the design's seam keeps the trait
//! consumer-side simple.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lattice_core::{BufferFlags, BufferId, BufferKind};
use lattice_grammar::CommandRegistry;
use lattice_mode::{BufferStore, BufferStoreHandle, ModeActivator, ModeId, ServiceRegistry};
use lattice_multibuffer::{
    Excerpt, InMemoryMultibufferRegistry, MultibufferRegistryHandle, create_multibuffer_view,
};
use lattice_runtime::Document;

/// Records every `BufferStore::insert_document_buffer` invocation
/// so the test can assert which `BufferKind` arrived. Other
/// `BufferStore` methods aren't exercised by the M.2.b.2 surface
/// — they return placeholders.
#[derive(Debug, Default)]
struct StubBufferStore {
    inserts: Mutex<Vec<(BufferId, BufferKind)>>,
}

impl BufferStore for StubBufferStore {
    fn find_by_name(&self, _name: &str) -> Option<BufferId> {
        None
    }
    fn name_for(&self, _id: BufferId) -> Option<String> {
        None
    }
    fn handle_for(&self, _id: BufferId) -> Option<Arc<dyn Document>> {
        None
    }
    fn insert_document_buffer(
        &self,
        id: BufferId,
        kind: BufferKind,
        _handle: Arc<dyn Document>,
        _flags: BufferFlags,
        _name: Option<String>,
    ) {
        self.inserts.lock().unwrap().push((id, kind));
    }
}

/// Mock `ModeActivator` that records each activation call. Holds a
/// fresh `ServiceRegistry` populated with `BufferStoreHandle` +
/// `MultibufferRegistryHandle` so `create_multibuffer_view` can
/// look both up via `services()`.
struct MockActivator {
    services: Arc<ServiceRegistry>,
    major_calls: Vec<(BufferId, BufferKind)>,
    minor_calls: Vec<(BufferId, ModeId)>,
    stub_store: Arc<StubBufferStore>,
    mb_registry: MultibufferRegistryHandle,
}

impl MockActivator {
    fn new() -> Self {
        let stub_store = Arc::new(StubBufferStore::default());
        let mb_registry: MultibufferRegistryHandle = Arc::new(InMemoryMultibufferRegistry::new());

        let mut services = ServiceRegistry::new();
        let store_dyn: Arc<dyn BufferStore> = stub_store.clone();
        services.register(BufferStoreHandle::new(store_dyn));
        services.register(mb_registry.clone());

        Self {
            services: Arc::new(services),
            major_calls: Vec::new(),
            minor_calls: Vec::new(),
            stub_store,
            mb_registry,
        }
    }
}

impl ModeActivator for MockActivator {
    fn activate_major_for_kind(&mut self, buffer: BufferId, kind: BufferKind) {
        self.major_calls.push((buffer, kind));
    }
    fn activate_minor_by_id(&mut self, buffer: BufferId, mode: ModeId) {
        self.minor_calls.push((buffer, mode));
    }
    fn services(&self) -> Arc<ServiceRegistry> {
        Arc::clone(&self.services)
    }
    fn ensure_named_document(
        &mut self,
        _name: &str,
        _major: ModeId,
        _flags: lattice_core::BufferFlags,
    ) -> BufferId {
        unimplemented!("MockActivator: ensure_named_document is unused in these tests")
    }
}

#[test]
fn empty_view_creates_inserts_and_activates_major() {
    let mut activator = MockActivator::new();
    let view_id = create_multibuffer_view(
        &mut activator,
        HashMap::new(),
        Vec::new(),
        Some("*test:empty*".into()),
        BufferFlags::default(),
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new())),
        None,
    );

    // Step 4: buffer-registry insert recorded the right kind.
    let inserts = activator.stub_store.inserts.lock().unwrap().clone();
    assert_eq!(inserts.len(), 1);
    assert_eq!(inserts[0].0, view_id);
    assert_eq!(inserts[0].1, BufferKind::Multibuffer);

    // Step 3: typed handle is reachable.
    let handle = activator
        .mb_registry
        .handle(view_id)
        .expect("registry should hold the view");
    assert_eq!(handle.buffer_id(), view_id);
    assert_eq!(handle.excerpt_count(), 0);

    // Step 5: activator's major-for-kind fired exactly once.
    assert_eq!(activator.major_calls.len(), 1);
    assert_eq!(activator.major_calls[0], (view_id, BufferKind::Multibuffer));

    // No minor activations from create_multibuffer_view itself.
    assert!(activator.minor_calls.is_empty());
}

#[test]
fn view_returns_unique_buffer_ids_per_call() {
    let mut activator = MockActivator::new();
    let a = create_multibuffer_view(
        &mut activator,
        HashMap::new(),
        Vec::new(),
        None,
        BufferFlags::default(),
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new())),
        None,
    );
    let b = create_multibuffer_view(
        &mut activator,
        HashMap::new(),
        Vec::new(),
        None,
        BufferFlags::default(),
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new())),
        None,
    );
    assert_ne!(a, b);
    assert_eq!(activator.mb_registry.len(), 2);
    assert!(activator.mb_registry.handle(a).is_some());
    assert!(activator.mb_registry.handle(b).is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn append_excerpts_after_create_extends_view() {
    use lattice_core::Document as CoreDocument;
    use lattice_runtime::spawn_document;

    let mut activator = MockActivator::new();
    let view_id = create_multibuffer_view(
        &mut activator,
        HashMap::new(),
        Vec::new(),
        None,
        BufferFlags::default(),
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new())),
        None,
    );
    let handle = activator.mb_registry.handle(view_id).unwrap();
    assert_eq!(handle.excerpt_count(), 0);

    // Spin up one source after-the-fact (provider-style streaming).
    let src_id = BufferId::next();
    let src_handle = spawn_document(
        src_id,
        CoreDocument::from_text("a\nb\nc\n"),
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new())),
    );
    handle.add_source(src_id, Arc::new(src_handle));
    handle.append_excerpts(vec![Excerpt::new(src_id, 0, 1)]);

    assert_eq!(handle.excerpt_count(), 1);
    assert_eq!(handle.snapshot().buffer.as_string(), "a\nb\n");
}
