//! PH7.7d — the grammar-extension round-trip bench (the §7 `< 5µs p99` seam).
//!
//! Warm end-to-end dispatch of a plugin motion through the **sync** trampoline:
//! `execute_motion_only` → project the `MotionContext` → the sync guest
//! `apply-motion` call (canonical-ABI lift/lower, no runtime) → `MotionResult::
//! from_wit`. This is the whole cost a plugin motion pays on every keystroke that
//! invokes it (the PH7.7 fork), the descriptive companion to the
//! `grammar_round_trip_stays_within_ceiling` ratchet (`tests/perf_ratchet.rs`).
//! Distinct from `benches/grammar.rs` (PH7.7a), which times the *marshalling*
//! halves in isolation with no guest. Skips when the `wasm32-wasip2` grammar
//! fixture wasn't built (see build.rs).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use lattice_core::buffer::Buffer;
use lattice_core::buffers::BufferId;
use lattice_grammar::CancellationToken;
use lattice_grammar::command::{CommandInvocation, Count};
use lattice_grammar::dispatcher::execute_motion_only;
use lattice_grammar::registry::{CommandRegistry, TextObjectEnv};
use lattice_mode::CapabilitySet;
use lattice_plugin_host::{PluginHost, PluginManifest, TrustTier};
use lattice_protocol::position::Position;

fn grammar_round_trip(c: &mut Criterion) {
    let path = env!("GRAMMAR_GUEST_WASM");
    if path.is_empty() {
        eprintln!("SKIP: grammar round-trip bench — plugin not built (add wasm32-wasip2)");
        return;
    }

    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();
    let manifest = PluginManifest::new("grammar-fixture", Vec::new(), CapabilitySet::empty());
    let set = host
        .instantiate_grammar_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            &std::sync::Arc::new(lattice_runtime::EventBus::new()),
            None,
        )
        .unwrap();
    // Leak the host so its engine + epoch ticker outlive the dispatched closures.
    Box::leak(Box::new(host));

    let mut registry = CommandRegistry::new();
    set.register_all(&mut registry);
    let motion_id = registry
        .id_by_name("down-n")
        .expect("fixture motion registered");

    let buffer = Buffer::from_text("l0\nl1\nl2\nl3\nl4\nl5\n");
    let cancel = CancellationToken::never();
    let cursor = Position { line: 1, byte: 0 };
    let invocation = CommandInvocation::of(motion_id).with_count(Count(3));

    c.bench_function("grammar_motion_round_trip", |b| {
        b.iter(|| {
            let out = execute_motion_only(
                &registry,
                &buffer,
                BufferId(1),
                cursor,
                black_box(invocation.clone()),
                &cancel,
                TextObjectEnv::default(),
            )
            .expect("plugin motion dispatches");
            black_box(out);
        })
    });
}

criterion_group!(benches, grammar_round_trip);
criterion_main!(benches);
