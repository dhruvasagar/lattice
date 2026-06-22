//! M.2.c (2026-06-01): multibuffer view-build + translation
//! rebuild benches.
//!
//! CI gates per `multibuffer-views.md` §7:
//! - `multibuffer_compose_p99_us` (M.1): ≤ 200µs at 50 visible
//!   excerpts (hot path is the renderer's per-frame snapshot
//!   read; we bench the underlying construction).
//! - `multibuffer_translation_rebuild_p99_us` (M.1): ≤ 2000µs
//!   at the 20k-row corpus (1k excerpts × 20 rows each — the
//!   stress shape architecture §7 calls out).
//!
//! The compose path runs once per view-creation (cold) +
//! once per `replace_excerpts` (provider-driven refresh).
//! Translation rebuild runs on every recompose and on
//! `append_excerpts`.

use std::collections::HashMap;
use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use lattice_core::{BufferId, Document as CoreDocument};
use lattice_grammar::CommandRegistry;
use lattice_multibuffer::{Excerpt, ExcerptHeader, MultibufferDocumentHandle};
use lattice_runtime::{Document, spawn_document};

fn empty_grammar() -> Arc<CommandRegistry> {
    Arc::new(CommandRegistry::new())
}

fn build_source(lines: u32) -> Arc<dyn Document> {
    let mut text = String::with_capacity(lines as usize * 8);
    for i in 0..lines {
        text.push_str(&format!("line-{i}\n"));
    }
    let id = BufferId::next();
    let handle = spawn_document(id, CoreDocument::from_text(&text), empty_grammar());
    Arc::new(handle)
}

fn build_view(
    excerpt_count: usize,
    sources_count: usize,
    excerpt_rows: u32,
) -> MultibufferDocumentHandle {
    let (view, _) = build_view_with_sample(excerpt_count, sources_count, excerpt_rows);
    view
}

/// Build a view AND return a sample source's `Arc<dyn Document>`
/// so the source-edit bench can read its `DocumentId` for the
/// synthetic `Event::DocumentChanged` publish.
fn build_view_with_sample(
    excerpt_count: usize,
    sources_count: usize,
    excerpt_rows: u32,
) -> (MultibufferDocumentHandle, Arc<dyn Document>) {
    let mut sources: HashMap<BufferId, Arc<dyn Document>> = HashMap::new();
    let mut source_ids: Vec<BufferId> = Vec::with_capacity(sources_count);
    let mut sample: Option<Arc<dyn Document>> = None;
    for _ in 0..sources_count {
        let src = build_source(excerpt_rows * 8);
        let id = BufferId::next();
        if sample.is_none() {
            sample = Some(src.clone());
        }
        sources.insert(id, src);
        source_ids.push(id);
    }
    let mut excerpts = Vec::with_capacity(excerpt_count);
    for i in 0..excerpt_count {
        let s = source_ids[i % sources_count];
        let start = ((i / sources_count) as u32) * (excerpt_rows + 1);
        let end = start + excerpt_rows - 1;
        excerpts.push(Excerpt::new(s, start, end).with_header(ExcerptHeader::default()));
    }
    let registry = Arc::new(CommandRegistry::new());
    let view = MultibufferDocumentHandle::new(sources, excerpts, registry)
        .expect("valid construction");
    (view, sample.expect("at least one source"))
}

/// M.1 architecture-§7 compose bench: build a 50-excerpt view
/// from scratch. Measures `MultibufferDocumentHandle::new`
/// (compose_snapshot + row_translation::build).
fn bench_compose_50_excerpts(c: &mut Criterion) {
    c.bench_function("multibuffer_compose_50_excerpts", |b| {
        b.iter(|| {
            let view = build_view(black_box(50), black_box(10), black_box(20));
            black_box(view);
        });
    });
}

/// Translation-rebuild bench at the M.1 architecture-§7 stress
/// corpus: 1k excerpts × 20 rows each = 20k composed rows. Runs
/// `recompose()` over a pre-built view (measures rebuild cost
/// without the source-handle spawn overhead).
fn bench_translation_rebuild(c: &mut Criterion) {
    let mut group = c.benchmark_group("multibuffer_translation_rebuild");
    for &n in &[100usize, 1_000] {
        let view = build_view(n, 50, 20);
        group.bench_with_input(BenchmarkId::new("excerpts", n), &n, |b, _| {
            b.iter(|| {
                view.recompose();
                black_box(&view);
            });
        });
    }
    group.finish();
}

/// Bench `append_excerpts` (the provider-streaming path).
/// Measures one batch-append of 10 excerpts onto a view with
/// `n` already-present excerpts. Heavy real-world path for
/// project-search style providers.
///
/// MH.B1 (2026-06-19): `append_excerpts` is now O(batch), not
/// O(total) — it composes + translates ONLY the appended batch
/// and inserts the batch text at the END of composed_doc rather
/// than rebuilding the whole rope from sources. The per-batch
/// cost should therefore be ~FLAT as the baseline `n` grows
/// (the curve was previously linear-in-`n` because every call
/// recomposed all `n` excerpts). The widened `n` range below
/// (50 → 5000) makes the flat-vs-linear contrast visible across
/// two orders of magnitude.
fn bench_append_excerpts(c: &mut Criterion) {
    let mut group = c.benchmark_group("multibuffer_append_excerpts");
    for &n in &[50usize, 500, 5_000] {
        group.bench_with_input(BenchmarkId::new("baseline_excerpts", n), &n, |b, &n| {
            b.iter_batched(
                || {
                    let view = build_view(n, 5, 10);
                    let existing_sources: Vec<BufferId> = view.source_buffer_ids();
                    let batch: Vec<Excerpt> = (0..10)
                        .map(|i| {
                            let s = existing_sources[i % existing_sources.len()];
                            Excerpt::new(s, 0, 4).with_header(ExcerptHeader::default())
                        })
                        .collect();
                    (view, batch)
                },
                |(view, batch)| {
                    view.append_excerpts(batch);
                    black_box(&view);
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

/// M.4.1 (2026-06-01): source-edit propagation bench.
///
/// Architecture §7 CI gate: `multibuffer_source_edit_p99_us`
/// ≤ 200 µs at 1k excerpts × 10 source buffers. Measures the
/// slide_anchors_for_source + recompose path the forwarder
/// task runs on every `DocumentChanged` event.
///
/// The bench builds a view with N excerpts × 20 rows across
/// 10 source documents, then drives one source-edit through
/// the EventBus and times the forwarder catching up. We
/// poll the snapshot to detect the recompose completion;
/// each iteration includes one round-trip through the spawned
/// task.
fn bench_source_edit_propagation(c: &mut Criterion) {
    use lattice_protocol::event::AppliedEdit;
    use lattice_protocol::position::{Position, Range};

    let mut group = c.benchmark_group("multibuffer_source_edit");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    for &n in &[100usize, 1_000] {
        group.bench_with_input(BenchmarkId::new("excerpts", n), &n, |b, &n| {
            // Build everything once outside iter so we measure
            // just the propagation path, not view construction.
            let (view, sample) = build_view_with_sample(n, 10, 20);
            let bus = std::sync::Arc::new(lattice_runtime::EventBus::new());
            rt.block_on(async {
                view.attach_event_subscriptions(&bus);
            });
            let any_doc_id = sample.id();

            b.iter(|| {
                let edit = AppliedEdit {
                    original_range: Range::new(Position::new(0, 0), Position::new(0, 0)),
                    inserted_range: Range::new(Position::new(0, 0), Position::new(1, 0)),
                    replaced_text: String::new(),
                    inserted_text: "X\n".to_string(),
                };
                let starting_version = view.snapshot().version;
                bus.publish(lattice_protocol::Event::DocumentChanged {
                    id: any_doc_id,
                    path: None,
                    version: 1,
                    edits: vec![edit],
                });
                // Wait for the recompose to complete (poll the
                // snapshot's version bump). This is what the
                // gate measures: time-to-propagation.
                rt.block_on(async {
                    for _ in 0..1000 {
                        tokio::task::yield_now().await;
                        if view.snapshot().version != starting_version {
                            return;
                        }
                    }
                });
                criterion::black_box(view.snapshot());
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_compose_50_excerpts,
    bench_translation_rebuild,
    bench_append_excerpts,
    bench_source_edit_propagation,
);
criterion_main!(benches);
