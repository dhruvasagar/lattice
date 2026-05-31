#![allow(clippy::unwrap_used, clippy::panic)]
//! Criterion benchmarks for `LspMode` activation latency.
//!
//! M-async.5 moved the LSP `initialize` round-trip into
//! `LspMode::on_activate`. This bench measures the dispatch
//! latency for that path against:
//!
//! - **`no_server_config`**: lsp-mode activates on a buffer
//!   whose path doesn't match any registered server. The
//!   supervisor's `open_buffer` round-trips its mailbox and
//!   returns `Ok(empty)`. Measures the bare dispatch +
//!   spawn-then-completion latency.
//! - **`unregistered_supervisor`**: same shape but the App
//!   doesn't register the `LspSupervisorHandle` service.
//!   `LspMode::on_activate` short-circuits before the
//!   mailbox round-trip; the future is immediately ready and
//!   the try-sync-then-spawn driver completes on the App
//!   thread. Measures the lower bound.
//!
//! Real-server cold/warm activation requires a binary on the
//! benchmark host (e.g. rust-analyzer) and is not in this
//! file. The two cases above pin the dispatch overhead so
//! regressions in the registry / cascade / epoch counter
//! surface in CI.

use std::sync::Arc;

use criterion::{Criterion, black_box, criterion_group, criterion_main};

use lattice_core::BufferFlags;
use lattice_lsp::completion::register_lsp_completion_mode;
use lattice_lsp::modes::{LspMode, register_lsp_log_modes};
use lattice_lsp::{LspLogger, LspSupervisor, LspSupervisorHandle};
use lattice_mode::{
    ActiveModes, BufferStore, BufferStoreHandle, CapabilitySet, GuardStoreHandle, ModeId,
    ModeRegistry, ServiceRegistry,
};
use lattice_protocol::ids::BufferId;
use lattice_runtime::EventBus;

/// Minimal `BufferStore` that satisfies the service-lookup
/// requirement of `LspMode::on_activate` without spinning up a
/// Document actor. Every accessor returns `None` /
/// `BufferId::default()`; `LspMode` skips the `open_buffer`
/// call entirely when `handle_for` is `None`.
#[derive(Debug)]
struct NullBufferStore;

impl BufferStore for NullBufferStore {
    fn find_by_name(&self, _name: &str) -> Option<lattice_core::BufferId> {
        None
    }
    fn ensure_named_document(
        &self,
        _name: &str,
        _major: ModeId,
        _flags: BufferFlags,
    ) -> lattice_core::BufferId {
        // Unreachable for the bench's lsp-mode path; satisfy the
        // trait contract with a placeholder.
        lattice_core::BufferId(0)
    }
    fn name_for(&self, _id: lattice_core::BufferId) -> Option<String> {
        None
    }
    fn handle_for(&self, _id: lattice_core::BufferId) -> Option<lattice_runtime::RopeDocumentHandle> {
        None
    }
}

/// Build a `LspSupervisorHandle` with no server configs. Both
/// the registry (which needs the handle to register
/// `lsp-completion-mode`) and the per-case `ServiceRegistry`
/// (one with, one without) share this single supervisor.
fn build_supervisor(rt: &tokio::runtime::Handle) -> LspSupervisorHandle {
    let logger = LspLogger::with_defaults();
    let sup = LspSupervisor::new(logger);
    sup.spawn(rt)
}

fn build_services_with_supervisor(handle: LspSupervisorHandle) -> Arc<ServiceRegistry> {
    let mut s = ServiceRegistry::new();
    s.register(handle);
    s.register(LspLogger::with_defaults());
    let store: Arc<dyn BufferStore> = Arc::new(NullBufferStore);
    s.register(BufferStoreHandle::new(store));
    Arc::new(s)
}

fn build_services_without_supervisor() -> Arc<ServiceRegistry> {
    let mut s = ServiceRegistry::new();
    s.register(LspLogger::with_defaults());
    let store: Arc<dyn BufferStore> = Arc::new(NullBufferStore);
    s.register(BufferStoreHandle::new(store));
    Arc::new(s)
}

fn build_registry(supervisor: LspSupervisorHandle) -> ModeRegistry {
    let mut r = ModeRegistry::new();
    register_lsp_log_modes(&mut r);
    // `LspCompletionMode` is source-contributing and registered
    // separately with a supervisor handle. Even the no-server-
    // config case needs a (non-functional) handle here to
    // satisfy the implies-cascade validation.
    register_lsp_completion_mode(&mut r, supervisor);
    r
}

/// Activate then deactivate lsp-mode + its 15 implied sub-modes.
/// The full cascade drives the dispatcher's try-sync-then-spawn
/// path; with no server config the supervisor returns
/// immediately and the spawn future is short.
fn activate_deactivate(c: &mut Criterion) {
    // Build a multi-threaded runtime so `block_on` inside the
    // App's mode dispatch path doesn't panic.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    let supervisor = build_supervisor(rt.handle());
    let registry = build_registry(supervisor.clone());
    let services = build_services_with_supervisor(supervisor);
    let config = Arc::new(lattice_config::ConfigRegistry::new());
    let events = Arc::new(EventBus::new());
    let lsp_mode_id = LspMode::mode_id();

    c.bench_function("lsp_mode::activate_deactivate::no_server_config", |b| {
        b.iter(|| {
            let mut active = ActiveModes::new();
            let guards = GuardStoreHandle::new();
            let buffer = BufferId::new(1);
            registry
                .activate_minor(
                    &mut active,
                    &guards,
                    &config,
                    &events,
                    &services,
                    buffer,
                    lsp_mode_id,
                    CapabilitySet::empty(),
                )
                .unwrap();
            // Even with no path-bearing buffer (NullBufferStore),
            // the dispatcher walks the 16-step cascade.
            black_box(active.has_minor(lsp_mode_id));
            registry
                .deactivate_minor(&mut active, &guards, &events, buffer, lsp_mode_id)
                .unwrap();
        });
    });

    let services_no_sup = build_services_without_supervisor();
    c.bench_function(
        "lsp_mode::activate_deactivate::unregistered_supervisor",
        |b| {
            b.iter(|| {
                let mut active = ActiveModes::new();
                let guards = GuardStoreHandle::new();
                let buffer = BufferId::new(1);
                registry
                    .activate_minor(
                        &mut active,
                        &guards,
                        &config,
                        &events,
                        &services_no_sup,
                        buffer,
                        lsp_mode_id,
                        CapabilitySet::empty(),
                    )
                    .unwrap();
                black_box(active.has_minor(lsp_mode_id));
                registry
                    .deactivate_minor(&mut active, &guards, &events, buffer, lsp_mode_id)
                    .unwrap();
            });
        },
    );
}

criterion_group!(benches, activate_deactivate);
criterion_main!(benches);
