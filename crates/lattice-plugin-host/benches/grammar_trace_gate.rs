//! PO.3 — the hot-path grammar-trace gate bench (design §4, the load-bearing
//! artefact of the slice).
//!
//! The sync grammar trampoline is the keystroke hot path. PO.3 instruments it
//! behind a per-plugin atomic gate whose contract is: **zero-alloc,
//! zero-arg-format when off** — a single relaxed-atomic load and a
//! predicted-not-taken branch. This bench pins that contract by timing the same
//! plugin-motion round-trip (`execute_motion_only` → sync `apply-motion`) in three
//! states:
//!
//!   - `grammar_seam_untraced`     — no tracer wired (the pre-PO.3 baseline).
//!   - `grammar_seam_trace_off`    — tracer wired, default `Info` gate: the gate
//!     load happens but admits nothing, so a successful call emits zero. Must sit
//!     ≈ the untraced baseline (the exit criterion).
//!   - `grammar_seam_trace_debug`  — the same plugin raised to `Debug`: every call
//!     times + enqueues a record. Shows the opt-in on-cost (never on the default
//!     keystroke path).
//!
//! Compare `off` vs `untraced`: the delta is the whole cost the instrumentation
//! adds to every keystroke when a user hasn't opted a plugin into tracing. It must
//! be ≈0. Skips when the `wasm32-wasip2` grammar fixture wasn't built (build.rs).

use std::hint::black_box;
use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use lattice_core::buffer::Buffer;
use lattice_core::buffers::BufferId;
use lattice_grammar::CancellationToken;
use lattice_grammar::command::{CommandInvocation, Count};
use lattice_grammar::dispatcher::execute_motion_only;
use lattice_grammar::registry::{CommandRegistry, TextObjectEnv};
use lattice_mode::CapabilitySet;
use lattice_plugin_host::{
    PluginHost, PluginManifest, PluginTracer, PluginTracerHandle, TraceLevel, TrustTier,
};
use lattice_protocol::position::Position;

/// Instantiate the grammar fixture (optionally traced) and register it. Returns
/// the registry + the plugin id; leaks the host so its engine + epoch ticker
/// outlive the dispatched trampoline closures.
fn setup(path: &str, tracer: Option<&PluginTracerHandle>) -> (CommandRegistry, u32) {
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();
    let manifest = PluginManifest::new("grammar-fixture", Vec::new(), CapabilitySet::empty());
    let set = host
        .instantiate_grammar_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            &Arc::new(lattice_runtime::EventBus::new()),
            tracer,
            None,
        )
        .unwrap();
    let id = set.plugin_id().0;
    Box::leak(Box::new(host));

    let mut registry = CommandRegistry::new();
    set.register_all(&mut registry);
    (registry, id)
}

fn grammar_trace_gate(c: &mut Criterion) {
    let path = env!("GRAMMAR_GUEST_WASM");
    if path.is_empty() {
        eprintln!("SKIP: grammar trace-gate bench — plugin not built (add wasm32-wasip2)");
        return;
    }

    let buffer = Buffer::from_text("l0\nl1\nl2\nl3\nl4\nl5\n");
    let cancel = CancellationToken::never();
    let cursor = Position { line: 1, byte: 0 };

    let run = |registry: &CommandRegistry, invocation: &CommandInvocation| {
        let out = execute_motion_only(
            registry,
            &buffer,
            BufferId(1),
            cursor,
            black_box(invocation.clone()),
            &cancel,
            TextObjectEnv::default(),
        )
        .expect("plugin motion dispatches");
        black_box(out);
    };

    // Baseline: no tracer at all (pre-PO.3).
    {
        let (registry, _) = setup(path, None);
        let invocation =
            CommandInvocation::of(registry.id_by_name("down-n").unwrap()).with_count(Count(3));
        c.bench_function("grammar_seam_untraced", |b| {
            b.iter(|| run(&registry, &invocation))
        });
    }

    // Off: tracer wired, default `Info` gate (the common keystroke). The gate load
    // fires but admits nothing — must sit ≈ the untraced baseline.
    {
        let tracer: PluginTracerHandle = Arc::new(PluginTracer::with_defaults());
        let (registry, _) = setup(path, Some(&tracer));
        let invocation =
            CommandInvocation::of(registry.id_by_name("down-n").unwrap()).with_count(Count(3));
        c.bench_function("grammar_seam_trace_off", |b| {
            b.iter(|| run(&registry, &invocation))
        });
    }

    // On: the same plugin raised to `Debug` — every call times + enqueues a record.
    {
        let tracer: PluginTracerHandle = Arc::new(PluginTracer::with_defaults());
        let (registry, id) = setup(path, Some(&tracer));
        tracer.set_plugin_level(id, TraceLevel::Debug);
        let invocation =
            CommandInvocation::of(registry.id_by_name("down-n").unwrap()).with_count(Count(3));
        c.bench_function("grammar_seam_trace_debug", |b| {
            b.iter(|| run(&registry, &invocation))
        });
    }
}

criterion_group!(benches, grammar_trace_gate);
criterion_main!(benches);
