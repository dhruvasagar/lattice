//! M.2.c (2026-06-01): excerpt-jump motion benches.
//!
//! Pure-helper latency for the four motions registered in
//! `lattice-multibuffer::motions`. CI gate per
//! `multibuffer-views.md` §7: ≤ 10µs at 50 excerpts (motion is
//! per-keystroke; well under the one-frame ceiling, 8.3 ms at 120Hz).
//!
//! The motion handlers themselves wrap these helpers with
//! `MultibufferRegistry::handle(buffer_id)` (an `RwLock::read` +
//! `HashMap::get` + `Arc::clone`, sub-µs); the benches measure
//! the geometry walk that dominates latency at large excerpt
//! counts.

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use lattice_core::BufferId;
use lattice_multibuffer::motions::{
    excerpt_start_rows, next_excerpt_start_row, next_file_boundary_row, prev_excerpt_start_row,
    prev_file_boundary_row,
};
use lattice_multibuffer::{Excerpt, ExcerptHeader};

fn build_excerpts(count: usize, sources: usize) -> Vec<Excerpt> {
    // Allocate `sources` distinct BufferIds; round-robin assign
    // each excerpt to one of them.
    let source_ids: Vec<BufferId> = (0..sources).map(|_| BufferId::next()).collect();
    (0..count)
        .map(|i| {
            let s = source_ids[i % sources];
            Excerpt::new(s, 0, 4).with_header(ExcerptHeader::default())
        })
        .collect()
}

fn bench_motions(c: &mut Criterion) {
    let mut group = c.benchmark_group("multibuffer_motion");
    for &n in &[50usize, 500, 5_000] {
        let excerpts = build_excerpts(n, 5);
        let last_row = excerpt_start_rows(&excerpts).last().copied().unwrap_or(0);
        let mid_row = last_row / 2;

        group.bench_with_input(BenchmarkId::new("next_excerpt_start", n), &n, |b, _| {
            b.iter(|| {
                black_box(next_excerpt_start_row(
                    black_box(&excerpts),
                    black_box(mid_row),
                    black_box(1),
                ))
            });
        });

        group.bench_with_input(BenchmarkId::new("prev_excerpt_start", n), &n, |b, _| {
            b.iter(|| {
                black_box(prev_excerpt_start_row(
                    black_box(&excerpts),
                    black_box(mid_row),
                    black_box(1),
                ))
            });
        });

        group.bench_with_input(BenchmarkId::new("next_file_boundary", n), &n, |b, _| {
            b.iter(|| {
                black_box(next_file_boundary_row(
                    black_box(&excerpts),
                    black_box(mid_row),
                    black_box(1),
                ))
            });
        });

        group.bench_with_input(BenchmarkId::new("prev_file_boundary", n), &n, |b, _| {
            b.iter(|| {
                black_box(prev_file_boundary_row(
                    black_box(&excerpts),
                    black_box(mid_row),
                    black_box(1),
                ))
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_motions);
criterion_main!(benches);
