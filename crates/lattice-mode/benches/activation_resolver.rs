#![allow(clippy::unwrap_used)]
//! Criterion benchmark for the MA.2 minor-activation resolver core
//! (`ModeRegistry::auto_activatable_minors`).
//!
//! The host resolver runs this once per `Event::MajorEntered` — a
//! *rare* lifecycle event (buffer open / major switch), never on the
//! keystroke path. It is an O(registered-minors) walk: for each
//! registered minor, read its `ActivationPolicy` and test `admits`.
//! This bench pins that linear scaling across registry sizes so a
//! regression (e.g. an accidental O(minors^2) or a per-call
//! allocation blow-up) surfaces in CI.

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use lattice_core::BufferKind;
use lattice_mode::{
    ActivationPolicy, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, ModeRegistry,
};

/// Marker minor with a fixed Global policy. `Guard = ()`, trivial
/// `on_activate` — only its `activation_policy()` matters here.
struct GlobalMinor(ModeId);

impl Mode for GlobalMinor {
    type Guard = ();
    fn id(&self) -> ModeId {
        self.0
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Global
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// A registry pre-populated with `n` Global minor modes.
fn registry_with(n: usize) -> ModeRegistry {
    let mut r = ModeRegistry::new();
    for i in 0..n {
        r.register(GlobalMinor(ModeId::new(&format!("global-minor-{i}"))))
            .unwrap();
    }
    r
}

fn bench_auto_activatable(c: &mut Criterion) {
    let mut group = c.benchmark_group("auto_activatable_minors");
    for &n in &[16usize, 64, 256] {
        let registry = registry_with(n);
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                // Worst case: every minor is Global and the buffer is
                // a Document, so every one passes the gate.
                let minors = registry.auto_activatable_minors(
                    black_box("text-mode"),
                    black_box(BufferKind::Document),
                );
                black_box(minors);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_auto_activatable);
criterion_main!(benches);
