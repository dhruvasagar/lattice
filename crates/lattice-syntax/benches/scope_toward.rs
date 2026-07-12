#![allow(clippy::unwrap_used, clippy::panic)]
//! Criterion bench for `SyntaxSnapshot::scope_toward` (TSM.2's tree
//! walk, backing the 16 structural motions registered in TSM.3/4).
//!
//! `scope_toward` runs on the core/actor thread on a deliberate
//! keypress (`]f` / `[c` / …) -- never in `Render::render`, never
//! per-frame (paramount #1, treesitter-motions.md slice-plan
//! "Global Constraints"). This bench is the artefact that verifies
//! the perf claim rather than asserting it in prose: it isolates
//! the tree-query cost on a large file, from the file midpoint, in
//! both directions.
//!
//! The design-critical detail under test is the
//! `QueryCursor::set_byte_range` restriction in
//! `SyntaxSnapshot::scope_toward` -- `Forward` scans only
//! `[cursor, EOF)` and `Backward` only `[0, cursor)`, so a query
//! from the midpoint of a 2000-fn file only ever walks matches in
//! HALF the tree, not the whole file. That's the bound this bench
//! makes visible.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use lattice_grammar::{NavBoundary, NavDir};
use lattice_syntax::{Lang, Syntax};

/// 2000 top-level functions, one per couple of lines -- large enough
/// that an unbounded (whole-file) scan would show up clearly against
/// the byte-range-restricted half-file scan this bench exercises.
fn rust_corpus(n_fns: usize) -> String {
    let mut s = String::with_capacity(n_fns * 32);
    for i in 0..n_fns {
        s.push_str(&format!("fn f{i}() {{ let x = {i}; }}\n"));
    }
    s
}

fn snapshot_rust(src: &str) -> lattice_syntax::SyntaxSnapshot {
    let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
    s.parse(src);
    s.snapshot_owned()
}

fn bench_scope_toward(c: &mut Criterion) {
    let src = rust_corpus(2000);
    let snap = snapshot_rust(&src);
    let mid = (src.lines().count() / 2) as u32;

    c.bench_function("scope_toward/fwd_start", |b| {
        b.iter(|| {
            snap.scope_toward(
                black_box(mid),
                0,
                "function.outer",
                NavDir::Forward,
                NavBoundary::Start,
                1,
            )
        })
    });

    c.bench_function("scope_toward/back_start", |b| {
        b.iter(|| {
            snap.scope_toward(
                black_box(mid),
                0,
                "function.outer",
                NavDir::Backward,
                NavBoundary::Start,
                1,
            )
        })
    });
}

criterion_group!(scope_toward, bench_scope_toward);
criterion_main!(scope_toward);
