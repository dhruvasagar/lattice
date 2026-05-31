#![allow(clippy::unwrap_used, clippy::panic)]
//! M.-0 (2026-05-31): `Document` hot-path baseline.
//!
//! Two bench groups capture the per-frame and per-keystroke cost
//! `Document` is on today. M.0 (the trait + ArcSwap refactor) must
//! keep both within noise of these numbers — the slice plan's
//! no-regression gate.
//!
//! - `document_read_p99_us` — what the renderer hits every frame:
//!   per-line rope access over a viewport-sized window plus
//!   selections / version reads. M.0 moves this through the trait
//!   method `rope_snapshot()` returning `Arc<Rope>`.
//!
//! - `document_edit_p99_us` — what the dispatcher hits every
//!   keystroke: `apply_edit` (insert / delete) plus
//!   `set_selections` at motion cadence. M.0 moves these to `&self`
//!   methods that allocate a new `Arc<Rope>` / `Arc<SelectionSet>`
//!   on commit; the Arc-churn cost must show up here if it
//!   regresses.
//!
//! Each group runs at three document sizes (10 / 1k / 100k lines)
//! so the linear part of the per-line read scales separately from
//! the per-edit constant.
//!
//! Run:
//!
//!   cargo bench -p lattice-core --bench document_hotpath
//!
//! Baseline numbers land in
//! `docs/dev/operations/benchmarks.md` once M.-0 is merged; M.0's
//! PR description quotes the before/after delta.

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

use lattice_core::Document;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::Position;
use lattice_protocol::selection::{Selection, SelectionSet};

/// Build a `Document` with `n_lines` lines of synthetic but
/// non-trivial content. Mirrors `buffer.rs`'s fixture so the two
/// benches are directly comparable on rope cost.
fn build_doc(n_lines: usize) -> Document {
    let mut s = String::with_capacity(n_lines * 64);
    for i in 0..n_lines {
        s.push_str(&format!("line {i}: the quick brown fox jumps over\n"));
    }
    Document::from_text(s)
}

/// Build a `SelectionSet` with a single collapsed cursor at
/// `(line, 0)`. Motion-cadence writes replace this with a new set
/// pointing at the next line.
fn cursor_at(line: u32) -> SelectionSet {
    SelectionSet::single(Selection::cursor(Position::new(line, 0)))
}

// ─────────────────────────────────────────────────────────────────
// document_read_p99_us — renderer hot path
// ─────────────────────────────────────────────────────────────────

/// Walk a 50-line viewport — what the renderer does once per
/// frame for each visible pane. Reads each line via Buffer's
/// public `line()` API, then reads selections + version (every
/// render needs these for cursor placement + cache invalidation).
fn viewport_walk(c: &mut Criterion) {
    let mut g = c.benchmark_group("document_read_p99_us::viewport_walk");
    for size in [10usize, 1_000, 100_000] {
        let doc = build_doc(size);
        // Viewport: 50 lines starting at row min(size/2, size-50).
        let start = size.saturating_sub(50).min(size / 2) as u32;
        let line_count = doc.buffer().line_count();
        g.throughput(Throughput::Elements(50));
        g.bench_with_input(BenchmarkId::from_parameter(size), &doc, |bencher, d| {
            bencher.iter(|| {
                let buf = d.buffer();
                let mut chars = 0usize;
                for row in 0..50u32 {
                    let line_idx = start + row;
                    if line_idx < line_count {
                        if let Some(s) = buf.line(line_idx) {
                            chars += s.len();
                        }
                    }
                }
                let _ = black_box(d.selections());
                let _ = black_box(d.version());
                let _ = black_box(d.text_version());
                black_box(chars);
            });
        });
    }
    g.finish();
}

// ─────────────────────────────────────────────────────────────────
// document_edit_p99_us — dispatcher hot path
// ─────────────────────────────────────────────────────────────────

/// Single-char insert at the document's midpoint. The typical
/// typing-cadence edit shape.
fn insert_at_middle(c: &mut Criterion) {
    let mut g = c.benchmark_group("document_edit_p99_us::insert_at_middle");
    for size in [10usize, 1_000, 100_000] {
        let mid = (size / 2) as u32;
        g.throughput(Throughput::Elements(1));
        g.bench_with_input(BenchmarkId::from_parameter(size), &size, |bencher, &sz| {
            bencher.iter_with_setup(
                || build_doc(sz),
                |mut d| {
                    d.apply_edit(Edit::insert(black_box(Position::new(mid, 0)), "x"))
                        .unwrap();
                },
            );
        });
    }
    g.finish();
}

/// Single-char delete at the document's midpoint. The symmetric
/// keystroke-cadence write.
fn delete_at_middle(c: &mut Criterion) {
    use lattice_protocol::position::Range;
    let mut g = c.benchmark_group("document_edit_p99_us::delete_at_middle");
    for size in [10usize, 1_000, 100_000] {
        let mid = (size / 2) as u32;
        g.throughput(Throughput::Elements(1));
        g.bench_with_input(BenchmarkId::from_parameter(size), &size, |bencher, &sz| {
            bencher.iter_with_setup(
                || build_doc(sz),
                |mut d| {
                    let range = Range::new(
                        Position::new(mid, 0),
                        Position::new(mid, 1),
                    );
                    d.apply_edit(Edit::delete(black_box(range))).unwrap();
                },
            );
        });
    }
    g.finish();
}

/// `set_selections` at motion cadence — every `hjkl` keystroke
/// fires one. M.0 routes this through `ArcSwap::store(Arc::new(...))`
/// so each call allocates. Bench detects regression if that path
/// grows materially.
fn set_selections_motion(c: &mut Criterion) {
    let mut g = c.benchmark_group("document_edit_p99_us::set_selections_motion");
    for size in [10usize, 1_000, 100_000] {
        g.throughput(Throughput::Elements(1));
        g.bench_with_input(BenchmarkId::from_parameter(size), &size, |bencher, &sz| {
            let mut d = build_doc(sz);
            let mut row = 0u32;
            bencher.iter(|| {
                row = (row + 1) % sz as u32;
                d.set_selections(cursor_at(black_box(row)));
            });
        });
    }
    g.finish();
}

criterion_group!(
    benches,
    viewport_walk,
    insert_at_middle,
    delete_at_middle,
    set_selections_motion
);
criterion_main!(benches);
