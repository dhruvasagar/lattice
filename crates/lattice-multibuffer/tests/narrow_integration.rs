//! N.1.1 (2026-06-10) integration tests for narrow mode.
//!
//! Exercises `create_narrow_view` end-to-end against a `MockActivator`
//! (mirrors `m2b2_integration.rs`): a one-excerpt multibuffer focused
//! on a line range, with `NarrowMode` activated and the
//! headerline set. The `:narrow` / `:widen` range-resolution + host
//! activation is covered by `lattice-host` dispatch tests; here we
//! pin the substrate contract.

#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use lattice_core::{BufferFlags, BufferId, BufferKind};
use lattice_grammar::CommandRegistry;
use lattice_mode::{BufferStore, BufferStoreHandle, ModeActivator, ModeId, ServiceRegistry};
use lattice_multibuffer::providers::narrow::{
    NarrowMode, create_narrow_view, register_narrow_ex_commands,
};
use lattice_multibuffer::{
    HeaderlineStatus, InMemoryMultibufferRegistry, MultibufferRegistryHandle,
};
use lattice_runtime::Document;

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

struct MockActivator {
    services: Arc<ServiceRegistry>,
    minor_calls: Vec<(BufferId, ModeId)>,
    mb_registry: MultibufferRegistryHandle,
}

impl MockActivator {
    fn new() -> Self {
        let stub_store = Arc::new(StubBufferStore::default());
        let mb_registry: MultibufferRegistryHandle = Arc::new(InMemoryMultibufferRegistry::new());

        let mut services = ServiceRegistry::new();
        let store_dyn: Arc<dyn BufferStore> = stub_store;
        services.register(BufferStoreHandle::new(store_dyn));
        services.register(mb_registry.clone());

        Self {
            services: Arc::new(services),
            minor_calls: Vec::new(),
            mb_registry,
        }
    }
}

impl ModeActivator for MockActivator {
    fn activate_major_for_kind(&mut self, _buffer: BufferId, _kind: BufferKind) {}
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

/// Spawn an in-memory source document with the given text and return
/// its id + an `Arc<dyn Document>` handle (the shape `:narrow`'s host
/// arm fetches from the buffer store).
fn make_source(text: &str) -> (BufferId, Arc<dyn Document>) {
    let id = BufferId::next();
    let document = lattice_core::DocumentBuilder::default()
        .with_text(text)
        .build();
    let handle = lattice_runtime::spawn_document(
        id,
        document,
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new())),
    );
    let dyn_handle: Arc<dyn Document> = Arc::new(handle);
    (id, dyn_handle)
}

const EIGHT_LINES: &str = "l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7\n";

#[test]
fn narrow_renders_only_the_requested_range() {
    let mut activator = MockActivator::new();
    let (source_id, source_handle) = make_source(EIGHT_LINES);

    let view_id = create_narrow_view(
        &mut activator,
        source_id,
        source_handle,
        2,
        5,
        "src.rs",
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new())),
        None,
    );

    let handle = activator
        .mb_registry
        .handle(view_id)
        .expect("registry holds the narrow view");

    // Exactly one excerpt, spanning the requested inclusive range.
    assert_eq!(handle.excerpt_count(), 1);
    let excerpts = handle.excerpts();
    assert_eq!(excerpts.len(), 1);
    assert_eq!(excerpts[0].source, source_id);
    assert_eq!(excerpts[0].start_line, 2);
    assert_eq!(excerpts[0].end_line, 5);
}

#[test]
fn narrow_activates_narrow_mode() {
    let mut activator = MockActivator::new();
    let (source_id, source_handle) = make_source(EIGHT_LINES);

    let view_id = create_narrow_view(
        &mut activator,
        source_id,
        source_handle,
        0,
        3,
        "",
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new())),
        None,
    );

    assert!(
        activator
            .minor_calls
            .iter()
            .any(|(b, m)| *b == view_id && *m == NarrowMode::mode_id()),
        "create_narrow_view must activate narrow-mode on the view; got {:?}",
        activator.minor_calls
    );
}

#[test]
fn narrow_headerline_shows_the_range() {
    let mut activator = MockActivator::new();
    let (source_id, source_handle) = make_source(EIGHT_LINES);

    let view_id = create_narrow_view(
        &mut activator,
        source_id,
        source_handle,
        2,
        5,
        "src.rs",
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new())),
        None,
    );

    let handle = activator.mb_registry.handle(view_id).unwrap();
    match &*handle.headerline() {
        HeaderlineStatus::Complete { summary, .. } => {
            // 1-indexed range in the headerline: lines 2..=5 -> "L3–6".
            assert!(
                summary.contains("src.rs") && summary.contains("L3") && summary.contains('6'),
                "headerline summary missing label/range: {summary:?}"
            );
        }
        other => panic!("expected Complete headerline, got {other:?}"),
    }
}

#[test]
fn empty_label_headerline_omits_label() {
    let mut activator = MockActivator::new();
    let (source_id, source_handle) = make_source(EIGHT_LINES);

    let view_id = create_narrow_view(
        &mut activator,
        source_id,
        source_handle,
        0,
        0,
        "",
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new())),
        None,
    );

    let handle = activator.mb_registry.handle(view_id).unwrap();
    match &*handle.headerline() {
        HeaderlineStatus::Complete { summary, .. } => {
            assert_eq!(summary, "[narrow] L1–1");
        }
        other => panic!("expected Complete headerline, got {other:?}"),
    }
}

#[test]
fn register_narrow_ex_commands_registers_narrow_and_widen() {
    let mut registry = CommandRegistry::new();
    register_narrow_ex_commands(&mut registry);
    assert!(
        registry.id_by_name("narrow").is_some(),
        "`:narrow` ex-command must register"
    );
    assert!(
        registry.id_by_name("widen").is_some(),
        "`:widen` ex-command must register"
    );
}
