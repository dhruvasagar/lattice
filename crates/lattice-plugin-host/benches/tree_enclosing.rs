#![allow(clippy::unwrap_used, clippy::panic)]
//! TS.1 criterion bench for the sync on-keystroke tree query —
//! `TreeSnapshotResource::enclosing` (plugin-treesitter-seam.md §6).
//!
//! `enclosing` is the ONE tree-sitter-seam operation on the synchronous grammar
//! path (auto-pair's manual close key / backspace, AP.3): it runs on the
//! dispatch thread, inside the grammar Reflex budget. The design's perf claim is
//! that it does **no parsing** — the tree is already there — and is a single
//! bounded walk: descend to the cursor's smallest node, then up the ancestor
//! chain. This bench is the artefact that makes that visible rather than
//! asserting it in prose: it times `enclosing` on a large pre-parsed file from
//! the file midpoint. (The WASM round-trip cost is bounded separately by
//! `grammar_roundtrip`; this isolates the native host-side walk the trampoline
//! runs before any result crosses.)

use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use lattice_plugin_host::tree_resource::TreeSnapshotResource;
use lattice_protocol::position::Position;
use lattice_syntax::{Lang, Syntax};

/// 2000 top-level functions — deep enough that a whole-tree scan (which
/// `enclosing` must NOT do) would dominate the ancestor walk it does do.
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

fn bench_enclosing(c: &mut Criterion) {
    let src = rust_corpus(2000);
    let ts = TreeSnapshotResource::new(Arc::new(snapshot_rust(&src)));
    // A cursor deep in the file (on the `x` of the midpoint function's body).
    let mid_line = (src.lines().count() / 2) as u32;
    let pos = Position {
        line: mid_line,
        byte: 18, // inside `{ let x = … }`
    };
    let block = [String::from("block")];

    c.bench_function("tree_enclosing/block_from_midpoint", |b| {
        b.iter(|| ts.enclosing(black_box(pos), black_box(&block)))
    });
    c.bench_function("tree_enclosing/node_at_from_midpoint", |b| {
        b.iter(|| ts.node_at(black_box(pos)))
    });
}

criterion_group!(tree_enclosing, bench_enclosing);
criterion_main!(tree_enclosing);
