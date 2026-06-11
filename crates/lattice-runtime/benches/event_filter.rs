#![allow(clippy::unwrap_used, clippy::panic)]
//! Criterion benchmark for the EF.1 `EventFilter` reserved fields.
//!
//! Backs the slice-plan EF.1 bench gate: implementing `path_glob` /
//! `major_modes` / `predicate` must NOT widen the publish scan past
//! O(subscribers-of-kind). The bus still visits exactly the
//! subscribers in the fired kind's bucket; the filter adds a
//! per-candidate constant check, not a wider walk.
//!
//! Two scenarios over the same subscriber count:
//!
//! - **kinds-only** -- the pre-EF.1 baseline (every extra field
//!   `None`, so `ExtraFilter::matches` short-circuits to `true`).
//! - **path_glob** -- each subscription also carries a compiled
//!   `**/*.rs` glob, exercising `event_path` + `GlobSet::is_match`
//!   per candidate.
//!
//! A regression that turned the per-candidate check into anything
//! super-constant (re-scanning all kinds, rebuilding the glob per
//! publish, ...) shows up as the `path_glob` line diverging from
//! `kinds-only` as the subscriber count grows.

use std::path::PathBuf;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use lattice_protocol::ids::DocumentId;
use lattice_protocol::{Event, EventKind};
use lattice_runtime::{EventBus, EventFilter, SubscriptionTarget, compile_glob_set};
use tokio::sync::mpsc;

/// One representative `DocumentSaved` event with a `.rs` path so the
/// `path_glob` scenario's `**/*.rs` set matches every candidate
/// (worst case for the filter -- no early bucket rejection).
fn saved_event() -> Event {
    Event::DocumentSaved {
        id: DocumentId::new(1),
        path: PathBuf::from("src/handler.rs"),
    }
}

/// Build a bus with `n` channel subscribers on `DocumentSaved`. When
/// `with_glob` is set each subscription also carries a `**/*.rs`
/// path-glob constraint. Receivers are returned so their channels
/// stay open (a closed channel would be pruned on first publish and
/// shrink the bucket, defeating the measurement).
fn build_bus(n: usize, with_glob: bool) -> (EventBus, Vec<mpsc::UnboundedReceiver<Event>>) {
    let bus = EventBus::new();
    let mut keepalive = Vec::with_capacity(n);
    for _ in 0..n {
        let (tx, rx) = mpsc::unbounded_channel();
        keepalive.push(rx);
        let filter = if with_glob {
            EventFilter::kind(EventKind::DocumentSaved)
                .with_path_glob(compile_glob_set(["**/*.rs"]))
        } else {
            EventFilter::kind(EventKind::DocumentSaved)
        };
        bus.subscribe(filter, SubscriptionTarget::Channel(tx));
    }
    (bus, keepalive)
}

fn bench_publish(c: &mut Criterion) {
    let mut group = c.benchmark_group("event_filter_publish");
    for &n in &[16usize, 64, 256] {
        // Baseline: kinds-only filter (every extra field None).
        let (bus, mut rxs) = build_bus(n, false);
        group.bench_with_input(BenchmarkId::new("kinds-only", n), &n, |b, _| {
            b.iter(|| {
                bus.publish(black_box(saved_event()));
                // Drain so the unbounded channels don't grow without
                // bound across the long iteration run.
                for rx in rxs.iter_mut() {
                    while rx.try_recv().is_ok() {}
                }
            });
        });

        // EF.1: each subscription also AND-checks a path glob.
        let (bus, mut rxs) = build_bus(n, true);
        group.bench_with_input(BenchmarkId::new("path_glob", n), &n, |b, _| {
            b.iter(|| {
                bus.publish(black_box(saved_event()));
                for rx in rxs.iter_mut() {
                    while rx.try_recv().is_ok() {}
                }
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_publish);
criterion_main!(benches);
