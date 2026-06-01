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
    let mut sources: HashMap<BufferId, Arc<dyn Document>> = HashMap::new();
    let mut source_ids: Vec<BufferId> = Vec::with_capacity(sources_count);
    for _ in 0..sources_count {
        let src = build_source(excerpt_rows * 8);
        let id = BufferId::next();
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
    MultibufferDocumentHandle::new(sources, excerpts).expect("valid construction")
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
fn bench_append_excerpts(c: &mut Criterion) {
    let mut group = c.benchmark_group("multibuffer_append_excerpts");
    for &n in &[50usize, 500] {
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

criterion_group!(
    benches,
    bench_compose_50_excerpts,
    bench_translation_rebuild,
    bench_append_excerpts
);
criterion_main!(benches);
