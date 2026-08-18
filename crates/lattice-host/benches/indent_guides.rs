#![allow(clippy::unwrap_used, clippy::panic)]
//! IG.6: what indentation guides cost, on both sides of the publish.
//!
//! Guides add work in two places, and they are governed by different
//! constraints:
//!
//! - **The worker side** (`build_indent_guides`) runs in the pass that builds
//!   each pane's `DisplayMatrix` — off the UI thread, but on every rebuild,
//!   which in practice means every keystroke. It is bounded by the *covered
//!   window*, not by file size, and `guide_build/*` is the bench that would
//!   catch it if that stopped being true. A version that walked the whole
//!   rope would look fine on a 500-line fixture and fall over on a real file.
//!
//! - **The renderer side** (`guide_active_pick/*`) runs per frame, per pane,
//!   on the UI thread. It is the price of having zero lag on the active-block
//!   highlight: rather than publish an "is active" flag and rerun the worker
//!   on every cursor move, each renderer picks the enclosing block from the
//!   cursor row it already holds. That trade is only correct while the pick
//!   stays trivially cheap, which is what this measures.
//!
//! A third measurement, `guide_walk/*`, isolates the pure block walk over
//! file size. It exists because the first formulation of that walk (the one
//! `compute_indent_folds` used, "scan forward to find the end") is quadratic
//! on deeply nested input; the stack version is linear, and this is the shape
//! that shows the difference.
//!
//! Numbers land in `docs/dev/operations/benchmarks.md`.

use std::hash::{Hash, Hasher};

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use lattice_cells::MatrixVersion;
use lattice_core::IndentUnit;
use lattice_core::indent_blocks::{indent_blocks, line_indents};
use lattice_host::indent_guides::build_indent_guides;

/// Nested source of roughly `blocks * 8` lines, three levels deep, with the
/// blank lines that make the block walk do real work (they are transparent,
/// so the walk cannot stop at the first one).
fn nested_source(blocks: usize) -> Vec<String> {
    let mut out = Vec::with_capacity(blocks * 8);
    for i in 0..blocks {
        out.push(format!("fn function_{i}(arg: i32) -> i32 {{"));
        out.push("    let mut total = arg;".into());
        out.push(String::new());
        out.push("    if total > 0 {".into());
        out.push("        for _ in 0..total {".into());
        out.push("            total += 1;".into());
        out.push(String::new());
        out.push("        }".into());
        out.push("    }".into());
        out.push("    total".into());
        out.push("}".into());
        out.push(String::new());
    }
    out
}

fn unit() -> IndentUnit {
    IndentUnit::new(4, true, 4)
}

/// The pure walk, swept over file size. Linear is the claim.
fn bench_walk(c: &mut Criterion) {
    let mut group = c.benchmark_group("guide_walk");
    for blocks in [50usize, 200, 800] {
        let lines = nested_source(blocks);
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let indents = line_indents(refs.iter().copied(), &unit());
        group.bench_function(format!("{}_lines", lines.len()), |b| {
            b.iter(|| black_box(indent_blocks(black_box(&indents), 4)));
        });
    }
    group.finish();
}

/// The worker-side build over a realistic covered window. The window is what
/// bounds this, so the file grows underneath while the window does not — a
/// build that scaled with the file would show up here as a rising curve.
fn bench_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("guide_build");
    for blocks in [50usize, 200, 800] {
        let lines = nested_source(blocks);
        let count = lines.len() as u32;
        // Two viewports' worth, the cells worker's chunked-mode coverage,
        // anchored in the middle so the look-back has somewhere to walk.
        let lo = count / 2;
        let hi = (lo + 250).min(count);
        group.bench_function(format!("{count}_lines_250_row_window"), |b| {
            b.iter(|| {
                black_box(build_indent_guides(
                    |i| {
                        lines
                            .get(i as usize)
                            .map(|l| lattice_core::LineShape::from_line(l, &unit()))
                    },
                    count,
                    &unit(),
                    lo,
                    hi,
                    MatrixVersion::ZERO,
                ))
            });
        });
    }
    group.finish();
}

/// The per-frame renderer pick. Bounded by blocks in the window, and the
/// reason cursor motion never wakes the worker.
fn bench_active_pick(c: &mut Criterion) {
    let lines = nested_source(200);
    let count = lines.len() as u32;
    let lo = count / 2;
    let hi = (lo + 250).min(count);
    let guides = build_indent_guides(
        |i| {
            lines
                .get(i as usize)
                .map(|l| lattice_core::LineShape::from_line(l, &unit()))
        },
        count,
        &unit(),
        lo,
        hi,
        MatrixVersion::ZERO,
    );
    let mut group = c.benchmark_group("guide_active_pick");
    group.bench_function("cursor_row", |b| {
        b.iter(|| black_box(guides.active_block(black_box(lo + 120))));
    });
    // The whole per-frame cost for a 120-row viewport: one active pick plus a
    // walk of every visible row's marks, which is what each renderer does.
    group.bench_function("120_row_viewport", |b| {
        b.iter(|| {
            let active = guides.active_block(lo + 120);
            let mut acc = 0u64;
            for row in lo..(lo + 120).min(hi) {
                for mark in guides.marks_for_line(row) {
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    (mark.col, active == Some(mark.block)).hash(&mut h);
                    acc = acc.wrapping_add(h.finish());
                }
            }
            black_box(acc)
        });
    });
    group.finish();
}

criterion_group!(benches, bench_walk, bench_build, bench_active_pick);
criterion_main!(benches);
