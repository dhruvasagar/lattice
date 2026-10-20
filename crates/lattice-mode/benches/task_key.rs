#![allow(clippy::unwrap_used)]
//! CG.6: what keyed supersede costs per trigger.
//!
//! `ForegroundCancel::begin` runs once per user-triggered task — a push, a
//! search, a format — and the ticket's drop once per finished run: a fresh
//! run, a supersede (new inputs) and a refusal (same inputs) are measured. Neither
//! is on the keystroke path, but both take the one lock every producer
//! shares, so the cost should stay flat as unrelated tasks pile up. Swept
//! across how many OTHER tasks are in flight: the map lookup must not grow
//! with them.

use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use lattice_mode::{ForegroundCancel, TaskKey};

fn bench_begin(c: &mut Criterion) {
    let mut group = c.benchmark_group("task_key_begin_drop");
    for others in [0usize, 8, 64] {
        group.bench_with_input(
            BenchmarkId::from_parameter(others),
            &others,
            |b, &others| {
                let fc = Arc::new(ForegroundCancel::default());
                let _in_flight: Vec<_> = (0..others)
                    .map(|i| {
                        fc.begin(TaskKey::supersede("other", i.to_string()))
                            .unwrap()
                    })
                    .collect();
                // A fresh run of one task, then its finish: the whole per-trigger
                // lifecycle.
                b.iter(|| {
                    let ticket = fc
                        .begin(TaskKey::supersede("magit:push", "lattice origin"))
                        .unwrap();
                    black_box(ticket.is_current());
                });
            },
        );
    }
    // New inputs: the run in flight is cancelled and replaced.
    group.bench_function("supersede", |b| {
        let fc = Arc::new(ForegroundCancel::default());
        let key = |n: u64| TaskKey::supersede("search", "view 1").with_inputs(n.to_string());
        let mut n = 0u64;
        let mut current = fc.begin(key(n)).unwrap();
        b.iter(|| {
            n += 1;
            current = fc.begin(key(n)).unwrap();
        });
        black_box(current.is_current());
    });
    // The same inputs again: refused, the run in flight left alone — the
    // double-press path.
    group.bench_function("refuse_same_inputs", |b| {
        let fc = Arc::new(ForegroundCancel::default());
        let key = || TaskKey::supersede("magit:push", "lattice origin").with_inputs("push");
        let _running = fc.begin(key()).unwrap();
        b.iter(|| black_box(fc.begin(key()).is_err()));
    });
    group.finish();
}

criterion_group!(benches, bench_begin);
criterion_main!(benches);
