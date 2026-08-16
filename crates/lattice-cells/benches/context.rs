//! TC.1 — `resolve_context` cost.
//!
//! This is the one part of tree-sitter context that runs on the keystroke
//! path: the host resolves it per pane every time it publishes pane
//! inputs, which a cursor move does. Everything else in the feature is
//! off-thread (the plugin's query, once per parse) or version-gated (the
//! cells worker's row build, skipped when the resolved list is unchanged).
//!
//! So this number is the one worth ratcheting. The shape being measured
//! is a linear scan over the scope list plus a sort of the (small)
//! enclosing subset — a file with 50k scopes is the pathological end, not
//! the normal one, and it is there to catch a change that turns the
//! per-call cost superlinear.

use criterion::{Criterion, criterion_group, criterion_main};
use lattice_cells::context::{ContextOptions, ContextScope, resolve_context};
use std::hint::black_box;

/// `depth` scopes nested around the anchor, padded out to `total` scopes
/// with siblings the anchor is not inside — the realistic shape, since
/// most of a file's scopes are somewhere else entirely.
fn corpus(total: usize, depth: usize) -> (Vec<ContextScope>, u32) {
    let anchor = (total as u32) + 1_000;
    let mut scopes: Vec<ContextScope> = Vec::with_capacity(total);

    // Nested scopes tightening around the anchor.
    for i in 0..depth {
        let start = anchor - (depth - i) as u32 * 10;
        let end = anchor + (depth - i) as u32 * 10;
        scopes.push(ContextScope {
            scope_start: start,
            scope_end: end,
            header_start: start,
            header_end: start,
        });
    }
    // Non-enclosing siblings scattered below the anchor.
    for i in 0..total.saturating_sub(depth) {
        let start = (i as u32) * 2;
        scopes.push(ContextScope {
            scope_start: start,
            scope_end: start + 1,
            header_start: start,
            header_end: start,
        });
    }
    (scopes, anchor)
}

fn bench_resolve(c: &mut Criterion) {
    let mut group = c.benchmark_group("context_resolve");

    for (total, depth) in [(100usize, 5usize), (5_000, 20), (50_000, 20)] {
        let (scopes, anchor) = corpus(total, depth);
        let opts = ContextOptions {
            viewport_top: anchor,
            viewport_height: 60,
            ..ContextOptions::default()
        };
        group.bench_function(format!("{total}_scopes_depth_{depth}"), |b| {
            b.iter(|| {
                black_box(resolve_context(
                    black_box(&scopes),
                    black_box(anchor),
                    black_box(&opts),
                ))
            })
        });
    }

    group.finish();
}

criterion_group!(benches, bench_resolve);
criterion_main!(benches);
