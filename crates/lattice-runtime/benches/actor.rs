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

/// Snapshot publish standalone: just `from_document` + `store`,
/// isolated from the actor mailbox round-trip. Backs the §5.6.8
/// "snapshot publish (actor side) <2µs" target. Floor: ~500ns
/// (Buffer::clone Arc bump + Arc::new + atomic release-store).
fn snapshot_publish_standalone(c: &mut Criterion) {
    use lattice_runtime::{DocumentSnapshot, PublishedSnapshot};
    let mut g = c.benchmark_group("runtime::snapshot_publish_standalone");
    for size in [10usize, 1_000, 50_000] {
        let text = build_buffer_text(size);
        let doc = Document::from_text(&text);
        let initial = DocumentSnapshot::__bench_from_document(&doc);
        let cell = PublishedSnapshot::__bench_new(initial);
        g.bench_with_input(BenchmarkId::from_parameter(size), &doc, |bencher, d| {
            bencher.iter(|| {
                let snap = DocumentSnapshot::__bench_from_document(black_box(d));
                cell.__bench_store(snap);
            });
        });
    }
    g.finish();
}

/// Snapshot publish: the actor's per-commit cost. We measure
/// `DocumentSnapshot::from_document` + `PublishedSnapshot::store`
/// against a representative document via the public `apply_edit`
/// round-trip; this conflates the publish step with mailbox +
/// scheduler + work, which is intentional -- it backs the §8.2
/// "apply-edit round-trip" row. The standalone publish bench
/// above isolates the publish step itself.
fn snapshot_publish_via_apply_edit(c: &mut Criterion) {
    let mut g = c.benchmark_group("runtime::snapshot_publish_via_apply_edit");
    let registry = Arc::new(CommandRegistry::new());

    for size in [10usize, 1_000, 50_000] {
        let text = build_buffer_text(size);
        g.bench_with_input(BenchmarkId::from_parameter(size), &text, |bencher, t| {
            // One handle reused across iterations -- the actor task
            // stays alive; each iteration only pays the mutation +
            // snapshot publish cost.
            let handle = spawn_document(
                lattice_core::BufferId(0),
                Document::from_text(t),
                registry.clone(),
            );
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

/// Snapshot load via `SnapshotCache` -- the renderer's hot
/// per-frame read after Cache::load migration. Wait-free
/// thread-local-cached: when the writer hasn't published since
/// the last load, the call is one Relaxed atomic compare. Backs
/// §5.6.8's renderer-side floor.
fn snapshot_load_cached(c: &mut Criterion) {
    let mut g = c.benchmark_group("runtime::snapshot_load_cached");
    let registry = Arc::new(CommandRegistry::new());
    let handle = spawn_document(
        lattice_core::BufferId(0),
        Document::from_text("x"),
        registry,
    );
    let mut cache = handle.snapshot_cache();
    g.bench_function("steady", |bencher| {
        bencher.iter(|| {
            let snap = cache.load();
            black_box(snap.version);
        });
    });
    g.finish();
}

/// Snapshot load: the renderer's per-frame read. Wait-free arc-swap
/// load (`RopeDocumentHandle::snapshot` -> `PublishedSnapshot::load`).
/// Should be deep in single-digit nanoseconds -- the §5.6.8 `<5ns
/// p99` target. Independent of buffer size: arc-swap is
/// pointer-sized.
fn snapshot_load(c: &mut Criterion) {
    let mut g = c.benchmark_group("runtime::snapshot_load");
    let registry = Arc::new(CommandRegistry::new());
    let handle = spawn_document(
        lattice_core::BufferId(0),
        Document::from_text("x"),
        registry,
    );
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
            let handle = spawn_document(
                lattice_core::BufferId(0),
                Document::from_text(t),
                registry.clone(),
            );
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
            let handle = spawn_document(
                lattice_core::BufferId(0),
                Document::from_text(t),
                registry.clone(),
            );
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

/// Dispatch round-trip: a motion (`word_forward`) sent through
/// the actor, dispatched, and the resulting Effect returned.
/// Backs §8.2's "dispatch round-trip" row. Distinct from
/// apply_edit_round_trip in that the latter measures the actor
/// envelope around a raw rope op; this measures the envelope
/// around a grammar dispatch (motion → SelectionChange Effect).
fn dispatch_round_trip(c: &mut Criterion) {
    use lattice_grammar::CancellationToken;
    use lattice_grammar::builtins::populate;
    use lattice_grammar::command::CommandInvocation;
    let mut g = c.benchmark_group("runtime::dispatch_round_trip");
    let mut registry_inner = lattice_grammar::CommandRegistry::new();
    let builtins = populate(&mut registry_inner);
    let registry = Arc::new(registry_inner);
    for size in [10usize, 1_000, 50_000] {
        let text = build_buffer_text(size);
        g.bench_with_input(BenchmarkId::from_parameter(size), &text, |bencher, t| {
            let handle = spawn_document(
                lattice_core::BufferId(0),
                Document::from_text(t),
                registry.clone(),
            );
            let inv = CommandInvocation::of(builtins.word_forward.0);
            // Pre-warm the actor task.
            let _ = block_on(handle.dispatch_with_cancel(
                inv.clone(),
                Position::ZERO,
                CancellationToken::never(),
            ));
            bencher.iter(|| {
                let _ = block_on(handle.dispatch_with_cancel(
                    inv.clone(),
                    black_box(Position::ZERO),
                    CancellationToken::never(),
                ))
                .unwrap();
            });
        });
    }
    g.finish();
}

/// Status-segment update: one snapshot load + a small format
/// representative of what the modeline does (`buf NN [path] (line/total)`).
/// Backs §8.2 "status segment update <500ns" and characterises
/// the cost on the editor's per-frame path.
fn status_segment_update(c: &mut Criterion) {
    let mut g = c.benchmark_group("runtime::status_segment_update");
    let registry = Arc::new(CommandRegistry::new());
    let handle = spawn_document(
        lattice_core::BufferId(0),
        Document::from_text(build_buffer_text(1_000).as_str()),
        registry,
    );
    g.bench_function("modeline_format", |bencher| {
        bencher.iter(|| {
            let snap = handle.snapshot();
            // Representative modeline string: buffer id + path + version.
            let s = format!(
                " buf #{}  ver {}  bytes {}",
                black_box(snap.id.0),
                black_box(snap.version),
                black_box(snap.buffer.byte_len()),
            );
            black_box(s.len());
        });
    });
    g.finish();
}

criterion_group!(
    benches,
    snapshot_publish_standalone,
    snapshot_publish_via_apply_edit,
    snapshot_load,
    snapshot_load_cached,
    apply_edit_round_trip,
    snapshot_post_publish_read,
    dispatch_round_trip,
    status_segment_update,
);
criterion_main!(benches);
