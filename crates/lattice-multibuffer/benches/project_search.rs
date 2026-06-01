//! M.6.1 (2026-06-01) bench: `project_search_first_batch_p99_ms`.
//!
//! CI gate per `multibuffer-views.md` slice plan M.6.1: ≤ 50 ms
//! at a 1k-file corpus. Measures the time from
//! `project_search` triggering to the first
//! `ProjectSearchBatchReady` event hitting the bus.
//!
//! The bench builds a synthetic corpus under `tempdir/lattice-search-bench-<nonce>/`
//! with deterministic content (~10 lines per file, every 10th
//! line contains the query needle). Files are removed once the
//! bench finishes.

#![cfg(feature = "search")]

use std::path::PathBuf;
use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use lattice_core::{BufferFlags, BufferId, BufferKind};
use lattice_mode::{BufferStore, BufferStoreHandle, ModeActivator, ModeId, ServiceRegistry};
use lattice_multibuffer::providers::search::{
    InMemoryProjectSearchService, ProjectSearchBatchReady, ProjectSearchOptions,
    ProjectSearchServiceHandle, project_search,
};
use lattice_multibuffer::{InMemoryMultibufferRegistry, MultibufferRegistryHandle};
use lattice_runtime::{Document, EventBus};

/// Minimal `BufferStore` stub — `create_multibuffer_view` looks
/// up `BufferStoreHandle` from services then calls
/// `insert_document_buffer`. We don't care about registration
/// side-effects in this bench; just satisfy the trait.
#[derive(Debug, Default)]
struct StubBufferStore;

impl BufferStore for StubBufferStore {
    fn find_by_name(&self, _name: &str) -> Option<BufferId> {
        None
    }
    fn ensure_named_document(
        &self,
        _name: &str,
        _major: ModeId,
        _flags: BufferFlags,
    ) -> BufferId {
        BufferId(0)
    }
    fn name_for(&self, _id: BufferId) -> Option<String> {
        None
    }
    fn handle_for(&self, _id: BufferId) -> Option<Arc<dyn Document>> {
        None
    }
    fn insert_document_buffer(
        &self,
        _id: BufferId,
        _kind: BufferKind,
        _handle: Arc<dyn Document>,
        _flags: BufferFlags,
        _name: Option<String>,
    ) {
    }
}

struct BenchActivator {
    services: Arc<ServiceRegistry>,
}

impl BenchActivator {
    fn new(bus: Arc<EventBus>) -> Self {
        let stub_store: Arc<dyn BufferStore> = Arc::new(StubBufferStore::default());
        let mb_registry: MultibufferRegistryHandle =
            Arc::new(InMemoryMultibufferRegistry::new());
        let search_svc: ProjectSearchServiceHandle =
            Arc::new(InMemoryProjectSearchService::new());
        let mut services = ServiceRegistry::new();
        services.register(BufferStoreHandle::new(stub_store));
        services.register(mb_registry);
        services.register(search_svc);
        services.register(bus);
        Self {
            services: Arc::new(services),
        }
    }
}

impl ModeActivator for BenchActivator {
    fn activate_major_for_kind(&mut self, _: BufferId, _: BufferKind) {}
    fn activate_minor_by_id(&mut self, _: BufferId, _: ModeId) {}
    fn services(&self) -> Arc<ServiceRegistry> {
        Arc::clone(&self.services)
    }
}

fn build_corpus(root: &PathBuf, file_count: usize, lines_per_file: usize, needle: &str) {
    std::fs::create_dir_all(root).unwrap();
    for i in 0..file_count {
        let mut text = String::with_capacity(lines_per_file * 16);
        for line in 0..lines_per_file {
            if line % 10 == 0 {
                text.push_str(&format!("{needle} hit at file={i} line={line}\n"));
            } else {
                text.push_str(&format!("line {line} of file {i}\n"));
            }
        }
        let path = root.join(format!("f{i:05}.txt"));
        std::fs::write(&path, text).unwrap();
    }
}

fn bench_first_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("project_search");
    // I/O-bound bench; small sample count keeps total bench
    // time manageable.
    group.sample_size(10);

    let needle = "needle-todo";
    let root = std::env::temp_dir().join(format!(
        "lattice-search-bench-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    build_corpus(&root, 1000, 10, needle);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    group.bench_function("first_batch_p99_ms_1k_files", |b| {
        b.iter(|| {
            // Run the whole iteration inside the runtime — both
            // `project_search` (which internally calls
            // `tokio::spawn`) and the subsequent `rx.recv()`
            // need a current tokio runtime context.
            rt.block_on(async {
                let bus = Arc::new(EventBus::new());
                let (tx, mut rx) =
                    tokio::sync::mpsc::unbounded_channel::<ProjectSearchBatchReady>();
                bus.subscribe_typed::<ProjectSearchBatchReady>(tx);
                let mut activator = BenchActivator::new(bus.clone());
                let options = ProjectSearchOptions {
                    root: root.clone(),
                    case_sensitive: true,
                    max_files: None,
                    max_hits_per_file: 100,
                    regex: false,
                };
                let _view = project_search(&mut activator, needle.to_string(), options);
                let _ = rx.recv().await;
            });
        });
    });

    group.finish();
    let _ = std::fs::remove_dir_all(&root);
}

criterion_group!(benches, bench_first_batch);
criterion_main!(benches);
