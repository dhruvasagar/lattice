//! HD.5 bench: the cost of embedding user docs compressed.
//!
//! Three numbers, matching the three things that actually happen:
//!
//! - `help_registry_boot_ns` — `builtin_topics()`. Runs once at editor
//!   boot, on the startup path, for every session whether or not the
//!   user ever opens `:help`. It must NOT decompress anything; if this
//!   grows with the doc set, laziness has been lost.
//! - `help_topic_first_open_ns` — the inflate + cache fill on the first
//!   `:help <topic>`. The one-time cost a user pays, and the number the
//!   embedded-docs budget doc asked to see measured.
//! - `help_topic_cached_open_ns` — every subsequent open of the same
//!   topic: a clone of the cached `String`, no inflate.
//!
//! CR.1 adds a fourth:
//!
//! - `help_registry_handle_read_ns` — one `.load()` snapshot plus a
//!   lookup through the RCU handle the registry now lives behind. The
//!   number exists to show the wrapper is free at the READ site: making
//!   the registry runtime-writable so plugins can register `:help`
//!   pages must not tax the path every `:help` already took.
//!
//! All four run on the dispatch path of an explicit user action, not
//! per-keystroke or per-frame, so the bar is "imperceptible within a
//! command", not the frame budget.
//!
//! The shared-dictionary scheme (2026-11-05) adds two:
//!
//! - `help_dictionary_prepare_ns` — preparing the shared zstd
//!   dictionary for decoding. Paid once per process, on the first
//!   `:help` of the session, never at boot.
//! - `help_topic_first_open_median_ns` — first open of the median-sized
//!   topic. The dictionary pays off most on small topics, and most
//!   topics are small, so the largest-topic number alone undersells it.
//!
//! The first-open benches pick their topics by size at run time rather
//! than by name: the largest topic changed from `modal-editing` to
//! `magit-transient` without anyone noticing, and a name pinned here
//! would have kept measuring the wrong case.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use lattice_help::topics::{HelpTopicBody, HelpTopicRegistry, builtin_topics, embedded_dictionary};

/// Builtin topic names ordered by decompressed size, smallest first.
fn by_size(r: &HelpTopicRegistry) -> Vec<String> {
    let mut sized: Vec<(usize, String)> = r
        .names()
        .filter_map(|n| match &r.lookup(n)?.body {
            HelpTopicBody::Compressed { raw_len, .. } => Some((*raw_len, n.to_string())),
            _ => None,
        })
        .collect();
    sized.sort();
    sized.into_iter().map(|(_, n)| n).collect()
}

fn bench_topics(c: &mut Criterion) {
    c.bench_function("help_registry_boot_ns", |b| {
        b.iter(|| black_box(builtin_topics().len()));
    });

    c.bench_function("help_dictionary_prepare_ns", |b| {
        b.iter(|| black_box(zstd::dict::DecoderDictionary::copy(embedded_dictionary())));
    });

    let sizes = by_size(&builtin_topics());
    let largest = sizes.last().expect("at least one builtin topic").clone();
    let median = sizes[sizes.len() / 2].clone();

    // The worst first-open case.
    c.bench_function("help_topic_first_open_ns", |b| {
        b.iter_batched(
            builtin_topics,
            |r| {
                let t = r.lookup(&largest).expect("largest topic");
                black_box(t.body.render().len())
            },
            criterion::BatchSize::SmallInput,
        );
    });

    // The typical first-open case.
    c.bench_function("help_topic_first_open_median_ns", |b| {
        b.iter_batched(
            builtin_topics,
            |r| {
                let t = r.lookup(&median).expect("median topic");
                black_box(t.body.render().len())
            },
            criterion::BatchSize::SmallInput,
        );
    });

    let warm = builtin_topics();
    let _ = warm.lookup(&largest).expect("largest topic").body.render();
    c.bench_function("help_topic_cached_open_ns", |b| {
        b.iter(|| {
            let t = warm.lookup(&largest).expect("largest topic");
            black_box(t.body.render().len())
        });
    });

    // CR.1: the RCU handle's read cost. Compare against
    // `help_topic_cached_open_ns` above — the delta IS the wrapper, and
    // it should be indistinguishable from noise.
    let handle = builtin_topics().into_handle();
    let _ = handle
        .load()
        .lookup(&largest)
        .expect("largest topic")
        .body
        .render();
    c.bench_function("help_registry_handle_read_ns", |b| {
        b.iter(|| {
            let snapshot = handle.load();
            let t = snapshot.lookup(&largest).expect("largest topic");
            black_box(t.body.render().len())
        });
    });
}

criterion_group!(benches, bench_topics);
criterion_main!(benches);
