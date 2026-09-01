#![allow(clippy::unwrap_used, clippy::panic)]
//! OA.0a criterion bench for a full guest-shaped tree WALK — the counterpart to
//! `tree_enclosing`, which times the single sync ancestor walk.
//!
//! `enclosing` touches one path from the cursor to the root. A guest that scans
//! a file for structure does the opposite: it visits *every* child of a node,
//! reads each one's kind and range, and resolves a field on some of them. The
//! org agenda's `walk_sections` is exactly that shape, and it is the thing this
//! bench exists to hold linear.
//!
//! **Why it exists.** A `NodeResource` is a path from the root, and the last
//! step of resolving one — `Node::child(i)` — walks the sibling list, so it is
//! O(i). Re-resolving on every accessor made a pass over a node's k children
//! O(k²), which made the whole walk quadratic in file size. Measured before the
//! fix: one guest `scan` of a 34 KB org file took **28.9 s**; after, 0.28 s. The
//! agenda that fed on it did not look slow, it looked broken — the view is
//! cleared before the scan is spawned, so a refresh showed nothing at all.
//!
//! The bench sweeps fan-out deliberately. A single size cannot tell linear from
//! quadratic, and quadratic is the failure mode: at 800 children the cost per
//! doubling must stay near 2×, not 4×. Read the ratio between the sizes, not
//! any one number.

use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use lattice_plugin_host::tree_resource::TreeSnapshotResource;
use lattice_syntax::{Lang, Syntax};

/// `n` top-level functions — the root's fan-out is what the walk pays for.
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

/// One pass in the shape a guest actually walks: every named child, its kind,
/// its range, and one field lookup.
fn bench_walk(c: &mut Criterion) {
    let mut group = c.benchmark_group("tree_walk");
    for n in [100usize, 200, 400, 800] {
        let src = rust_corpus(n);
        let ts = TreeSnapshotResource::new(Arc::new(snapshot_rust(&src)));
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                // A fresh root each iteration: the caches are per-resource, and
                // measuring a warmed one would measure the cache, not the walk.
                let root = ts.root();
                let count = root.named_child_count();
                for i in 0..count {
                    let Some(child) = root.named_child(i) else {
                        continue;
                    };
                    black_box(child.kind());
                    black_box(child.byte_range());
                    black_box(child.child_by_field("name").is_some());
                }
                black_box(count)
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_walk);
criterion_main!(benches);
