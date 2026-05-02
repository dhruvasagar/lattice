#![allow(clippy::unwrap_used, clippy::panic)]
//! Criterion benchmarks for the `lattice-runtime` actor layer.
//!
//! Backs the §5.6.8 + §8.2 latency commitments:
//!
//! - **Snapshot publish (actor side)** -- target `<10us p99` per
//!   §5.6.8. The actor runs this after every committed mutation.
//! - **Snapshot load (renderer side)** -- target `<5ns p99` per
//!   §5.6.8. Wait-free `arc_swap::Cache::load`. The renderer calls
//!   it once per visible document per frame.
//! - **`apply_edit` round-trip** -- target `<100us p99` per §8.2's
//!   "Keystroke to buffer mutation" row. End-to-end:
//!   `block_on(handle.apply_edit(...))` from a sync caller.
//!
//! Sizes vary across small / medium / large buffers so regressions
//! on small-file ergonomics or large-file scaling each surface in
//! CI. The benchmarks intentionally avoid the grammar dispatcher
//! (which has its own benches in `lattice-grammar`) -- they
//! characterise only the actor mailbox + snapshot publish/load
//! plumbing.

use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use lattice_core::Document;
use lattice_grammar::CommandRegistry;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::Position;
use lattice_runtime::{block_on, spawn_document};

/// Build a buffer of `n_lines` repeated lines for representative
/// rope shapes. Same builder as the grammar / search benches so
/// numbers compare apples-to-apples.
fn build_buffer_text(n_lines: usize) -> String {
    let mut s = String::with_capacity(n_lines * 64);
    for i in 0..n_lines {
        s.push_str(&format!(
            "fn handler_{i}(input: &str) -> Result<Output, Error> {{\n"
        ));
    }
    s
}

/// Snapshot publish: the actor's per-commit cost. We measure
/// `DocumentSnapshot::from_document` + `PublishedSnapshot::store`
/// against a representative document. Both are crate-internal to
/// `lattice-runtime`; `spawn_document` exercises them indirectly,
/// but the public surface doesn't expose them. We benchmark via a
/// dedicated handle whose snapshot we read after each mutation --
/// timing one full mutation cycle.
///
/// (Direct `from_document` + `store` measurement isn't possible
/// from outside the crate; the round-trip below is the closest
/// public-API proxy.)
fn snapshot_publish_via_apply_edit(c: &mut Criterion) {
    let mut g = c.benchmark_group("runtime::snapshot_publish_via_apply_edit");
    let registry = Arc::new(CommandRegistry::new());

    for size in [10usize, 1_000, 50_000] {
        let text = build_buffer_text(size);
        g.bench_with_input(BenchmarkId::from_parameter(size), &text, |bencher, t| {
            // One handle reused across iterations -- the actor task
            // stays alive; each iteration only pays the mutation +
            // snapshot publish cost.
            let handle = spawn_document(Document::from_text(t), registry.clone());
            // Pre-warm so the first iteration doesn't include the
            // actor task's spinup.
            let _ = block_on(handle.apply_edit(Edit::insert(Position::ZERO, "x")));
            bencher.iter(|| {
                let _ = block_on(handle.apply_edit(Edit::insert(Position::ZERO, black_box("y"))))
                    .unwrap();
            });
        });
    }
    g.finish();
}

/// Snapshot load: the renderer's per-frame read. Wait-free arc-swap
/// load (`DocumentHandle::snapshot` -> `PublishedSnapshot::load`).
/// Should be deep in single-digit nanoseconds -- the §5.6.8 `<5ns
/// p99` target. Independent of buffer size: arc-swap is
/// pointer-sized.
fn snapshot_load(c: &mut Criterion) {
    let mut g = c.benchmark_group("runtime::snapshot_load");
    let registry = Arc::new(CommandRegistry::new());
    let handle = spawn_document(Document::from_text("x"), registry);
    g.bench_function("load", |bencher| {
        bencher.iter(|| {
            let snap = handle.snapshot();
            black_box(snap.version);
        });
    });
    g.finish();
}

/// End-to-end keystroke-equivalent: caller-side
/// `block_on(handle.apply_edit(...))`. Includes mailbox try_send,
/// actor-side document mutation, snapshot construction +
/// publish, and oneshot reply. §8.2 "keystroke to buffer mutation"
/// row commits to `<100us p99` -- this is the actor's contribution
/// to that path, before the renderer compose phase.
fn apply_edit_round_trip(c: &mut Criterion) {
    let mut g = c.benchmark_group("runtime::apply_edit_round_trip");
    let registry = Arc::new(CommandRegistry::new());

    for size in [10usize, 1_000, 50_000] {
        let text = build_buffer_text(size);
        g.bench_with_input(BenchmarkId::from_parameter(size), &text, |bencher, t| {
            let handle = spawn_document(Document::from_text(t), registry.clone());
            // Warm.
            let _ = block_on(handle.apply_edit(Edit::insert(Position::ZERO, "x")));
            bencher.iter(|| {
                let applied =
                    block_on(handle.apply_edit(Edit::insert(Position::ZERO, black_box("z"))))
                        .unwrap();
                black_box(applied);
            });
        });
    }
    g.finish();
}

/// Snapshot post-publish read: simulates the renderer's per-frame
/// "load + read text". Repeats `Cache::load` plus a small read --
/// matches what `compose_visible_lines` does at frame start (one
/// load, then iterate).
fn snapshot_post_publish_read(c: &mut Criterion) {
    let mut g = c.benchmark_group("runtime::snapshot_post_publish_read");
    let registry = Arc::new(CommandRegistry::new());

    for size in [10usize, 1_000, 50_000] {
        let text = build_buffer_text(size);
        g.bench_with_input(BenchmarkId::from_parameter(size), &text, |bencher, t| {
            let handle = spawn_document(Document::from_text(t), registry.clone());
            bencher.iter(|| {
                let snap = handle.snapshot();
                black_box(snap.buffer.line_count());
                black_box(snap.dirty);
                black_box(snap.text_version);
            });
        });
    }
    g.finish();
}

criterion_group!(
    benches,
    snapshot_publish_via_apply_edit,
    snapshot_load,
    apply_edit_round_trip,
    snapshot_post_publish_read,
);
criterion_main!(benches);
