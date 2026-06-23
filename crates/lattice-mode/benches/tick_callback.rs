#![allow(clippy::unwrap_used)]
//! Criterion benchmark for the IDE-protocol I1.1 tick-callback registry's
//! per-tick `run_all`.
//!
//! The host calls `TickCallbackRegistry::run_all` once per editor tick
//! (inside `run_tick_pending`) — on the async-landed / `Tick` path, not
//! the keystroke path, but still per-tick, so its cost must stay flat and
//! tiny. `run_all` is a single `Mutex` lock + an O(registered-callbacks)
//! walk that concatenates each closure's returned effects. This bench pins
//! that linear scaling across registry sizes so a regression (an accidental
//! per-call allocation blow-up, or a lock-contention change) surfaces in CI.

use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use lattice_grammar::effect::Effect;
use lattice_mode::tick_callback::TickCallbackRegistry;

fn bench_run_all(c: &mut Criterion) {
    let mut group = c.benchmark_group("tick_callback_run_all");
    // 0 = the steady-state cost when no mode has registered a drain (the
    // common case at boot). 1/8/32 pin the linear scaling as drains are
    // added.
    for size in [0usize, 1, 8, 32] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let registry = Arc::new(TickCallbackRegistry::new());
            // Hold the registrations for the bench's lifetime; each drain
            // returns one Effect — the common bounded IdeInbound shape
            // (a few effects applied per tick).
            let _regs: Vec<_> = (0..size)
                .map(|_| registry.register(Box::new(|| vec![Effect::None])))
                .collect();
            b.iter(|| {
                let effects = registry.run_all();
                black_box(effects);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_run_all);
criterion_main!(benches);
