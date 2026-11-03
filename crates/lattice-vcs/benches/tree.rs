//! A directory listing at a revision — what `-` from a file at a revision
//! costs, and every `<CR>` into a directory after it.
//!
//! Run against this repository (`.`), so the tree is a real one of a few
//! hundred paths rather than a fixture. Two numbers, because the listing
//! source chooses between them:
//!
//! - `git_tree_list_root_us` — the repository already open: the cost of
//!   the lookup and the walk alone.
//! - `git_tree_list_root_with_open_us` — `Repository::discover` per call:
//!   what a source that holds no handle pays per listing.
//!
//! `git_tree_list_index_us` is the `staged` pseudo-revision, which walks
//! the whole index (a flat path list) rather than one tree.

use std::path::Path;

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use lattice_vcs::{Repository, Tree};

fn bench_tree(c: &mut Criterion) {
    let repo = Repository::discover(".").unwrap();
    c.bench_function("git_tree_list_root_us", |b| {
        b.iter(|| black_box(Tree::list(&repo, "HEAD", Path::new("")).unwrap()));
    });
    c.bench_function("git_tree_list_subdir_us", |b| {
        b.iter(|| black_box(Tree::list(&repo, "HEAD", Path::new("crates")).unwrap()));
    });
    c.bench_function("git_tree_list_root_with_open_us", |b| {
        b.iter(|| {
            let repo = Repository::discover(".").unwrap();
            black_box(Tree::list(&repo, "HEAD", Path::new("")).unwrap())
        });
    });
    c.bench_function("git_tree_list_index_us", |b| {
        b.iter(|| black_box(Tree::list_index(&repo, Path::new("")).unwrap()));
    });
}

criterion_group!(benches, bench_tree);
criterion_main!(benches);
