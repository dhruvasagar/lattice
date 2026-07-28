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
//! All three run on the dispatch path of an explicit user action, not
//! per-keystroke or per-frame, so the bar is "imperceptible within a
//! command", not the frame budget.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use lattice_help::topics::builtin_topics;

fn bench_topics(c: &mut Criterion) {
    c.bench_function("help_registry_boot_ns", |b| {
        b.iter(|| black_box(builtin_topics().len()));
    });

    // `modal-editing` is the largest topic — the worst first-open case.
    c.bench_function("help_topic_first_open_ns", |b| {
        b.iter_batched(
            builtin_topics,
            |r| {
                let t = r.lookup("modal-editing").expect("modal-editing topic");
                black_box(t.body.render().len())
            },
            criterion::BatchSize::SmallInput,
        );
    });

    let warm = builtin_topics();
    let _ = warm
        .lookup("modal-editing")
        .expect("modal-editing topic")
        .body
        .render();
    c.bench_function("help_topic_cached_open_ns", |b| {
        b.iter(|| {
            let t = warm.lookup("modal-editing").expect("modal-editing topic");
            black_box(t.body.render().len())
        });
    });
}

criterion_group!(benches, bench_topics);
criterion_main!(benches);
