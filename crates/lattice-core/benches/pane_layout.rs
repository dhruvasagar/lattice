#![allow(clippy::unwrap_used, clippy::panic)]
//! Criterion benchmarks for pane-tree layout (ZP.5).
//!
//! `PaneTree::compute_rects` is on the per-frame path in both
//! renderers' peers — the TUI draw path calls it, and so does the
//! per-pane viewport-sizing loop. Zoom (`<C-w>z`) adds a branch at its
//! head, so the number that matters is not "how fast is zoom" but
//! **zoomed vs. unzoomed on the same tree**: zoom should be strictly
//! cheaper, because it returns one rect instead of walking N leaves.
//! A zoomed measurement that came out *slower* would mean the branch
//! is costing more than the walk it replaces.
//!
//! See `docs/dev/architecture/pane-zoom.md` §8.

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use lattice_core::ui::pane::{PaneRect, PaneState, PaneTree, SplitOrientation};

/// An `n`-leaf tree, alternating split orientations so both recursion
/// arms are exercised rather than one deep spine of the same kind.
fn tree_with(n: usize) -> PaneTree {
    let mut t = PaneTree::single(PaneState::default());
    for i in 1..n {
        let orientation = if i % 2 == 0 {
            SplitOrientation::Horizontal
        } else {
            SplitOrientation::Vertical
        };
        let new_idx = t.split_active(orientation);
        t.set_active(new_idx);
    }
    t
}

fn area() -> PaneRect {
    PaneRect {
        x: 0,
        y: 0,
        width: 200,
        height: 60,
    }
}

fn compute_rects_unzoomed(c: &mut Criterion) {
    let mut g = c.benchmark_group("pane::compute_rects_unzoomed");
    for n in [1usize, 2, 4, 8] {
        let t = tree_with(n);
        g.bench_with_input(BenchmarkId::from_parameter(n), &t, |b, t| {
            b.iter(|| black_box(t.compute_rects(black_box(area()))));
        });
    }
    g.finish();
}

fn compute_rects_zoomed(c: &mut Criterion) {
    let mut g = c.benchmark_group("pane::compute_rects_zoomed");
    for n in [2usize, 4, 8] {
        let mut t = tree_with(n);
        assert!(t.toggle_zoom(), "{n}-leaf tree must be zoomable");
        g.bench_with_input(BenchmarkId::from_parameter(n), &t, |b, t| {
            b.iter(|| black_box(t.compute_rects(black_box(area()))));
        });
    }
    g.finish();
}

/// The GPUI peer's entry point — it recurses over `PaneNode` itself
/// rather than calling `compute_rects`, and reaches zoom through
/// `render_root`. Benched so a future change that made the zoomed arm
/// allocate (it returns a `Cow`, and the owned arm is a bare `Leaf`
/// with no boxed children today) shows up here.
fn render_root(c: &mut Criterion) {
    let mut g = c.benchmark_group("pane::render_root");
    for n in [2usize, 8] {
        let mut t = tree_with(n);
        g.bench_with_input(BenchmarkId::new("unzoomed", n), &t, |b, t| {
            b.iter(|| black_box(t.render_root()));
        });
        t.toggle_zoom();
        g.bench_with_input(BenchmarkId::new("zoomed", n), &t, |b, t| {
            b.iter(|| black_box(t.render_root()));
        });
    }
    g.finish();
}

criterion_group!(
    benches,
    compute_rects_unzoomed,
    compute_rects_zoomed,
    render_root
);
criterion_main!(benches);
