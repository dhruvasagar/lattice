#![allow(clippy::unwrap_used, clippy::panic)]
//! AR.6 (`docs/dev/architecture/autoread.md`): autoread watch-set cost.
//!
//! `refresh_autoread_watcher` runs on buffer open/close — NOT per frame and
//! NOT proportional to project size. Its only non-trivial compute is over the
//! desired watch set, whose size tracks the number of **open** file-backed
//! buffers (deduped by dir), never the file count of the project. This bench
//! guards that the two pure steps that dominate a refresh —
//! [`autoread_watch_fingerprint`] (the "did the set change?" gate) and
//! [`bound_watch_set`] (the LRU cap) — stay cheap as the open-buffer count
//! grows across 10 / 100 / 1000 distinct directories.
//!
//! The numbers should scale ~linearly (a sort + hash of N small entries) and
//! land in low microseconds even at 1000 — far under any per-frame budget, and
//! it only runs on a discrete open/close, not on the keystroke path.
//!
//! Run:
//!
//!   cargo bench -p lattice-host --bench autoread_watch_set

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use lattice_host::dispatch::{autoread_watch_fingerprint, bound_watch_set};

/// Build a desired watch set of `n` distinct directories, each with one file —
/// the shape `desired_autoread_watches` produces from `n` open buffers spread
/// across `n` dirs.
fn watch_set(n: usize) -> HashMap<PathBuf, HashSet<String>> {
    (0..n)
        .map(|i| {
            (
                PathBuf::from(format!("/project/module_{i}/src")),
                HashSet::from([format!("file_{i}.rs")]),
            )
        })
        .collect()
}

fn bench_watch_set(c: &mut Criterion) {
    let mut group = c.benchmark_group("autoread_watch_set");
    for n in [10usize, 100, 1000] {
        let set = watch_set(n);
        let active = PathBuf::from("/project/module_0/src");

        group.bench_with_input(BenchmarkId::new("fingerprint", n), &set, |b, set| {
            b.iter(|| black_box(autoread_watch_fingerprint(black_box(set))));
        });

        group.bench_with_input(BenchmarkId::new("bound_uncapped", n), &n, |b, _| {
            // Under the 128 cap for n=10/100; at n=1000 the cap path runs.
            b.iter(|| black_box(bound_watch_set(watch_set(n), Some(active.as_path()), 128)));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_watch_set);
criterion_main!(benches);
