//! TC.7 — the CI gate on sticky-context resolution.
//!
//! Companion to `benches/context.rs`, which records the descriptive numbers.
//! This is the GATE, in the shape the repo already uses for its other perf
//! ratchets (`lattice-plugin-host/tests/perf_ratchet.rs`,
//! `lattice-host/tests/keystroke_publish_ratchet.rs`): measure a warm
//! operation inline and assert a **generous absolute ceiling**, orders of
//! magnitude above the real release cost, so it catches a gross regression —
//! a superlinear term, an accidental clone of the scope list — without
//! tripping on runner variance or debug-build inflation.
//!
//! TC.7's checklist called for this and it was never written. The bench alone
//! cannot fail a build: `bench-compile` only checks that benches compile, and
//! `bench-baseline` runs on `main` and is explicitly record-only. So the
//! "ratchet" the slice claimed did not exist in any form that could catch
//! anything.
//!
//! `resolve_context` is the one part of this feature on the **keystroke path**
//! — the host calls it per pane on every pane-inputs publish, and a cursor
//! move is one.

#![allow(clippy::unwrap_used)]

use std::time::{Duration, Instant};

use lattice_cells::context::{ContextOptions, ContextScope, resolve_context};

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

/// `n` scopes nested `depth` deep around the middle of the file, the same
/// corpus shape the bench uses.
fn corpus(n: usize, depth: usize) -> Vec<ContextScope> {
    let mut scopes = Vec::with_capacity(n);
    for i in 0..n {
        let start = (i * 4) as u32;
        scopes.push(ContextScope {
            scope_start: start,
            scope_end: start + 3,
            header_start: start,
            header_end: start,
        });
    }
    // A genuinely nested spine through the middle, so the depth-bounded walk
    // and the sort of the enclosing subset are both exercised.
    let mid = (n / 2 * 4) as u32;
    for d in 0..depth {
        scopes.push(ContextScope {
            scope_start: mid.saturating_sub((depth - d) as u32 * 8),
            scope_end: mid + (depth - d) as u32 * 8,
            header_start: mid.saturating_sub((depth - d) as u32 * 8),
            header_end: mid.saturating_sub((depth - d) as u32 * 8),
        });
    }
    scopes
}

/// The pathological end of the recorded range: 50 000 scopes, depth 20.
///
/// Release median is ~21.8 µs (`benchmarks.md`, TC.1). The ceiling is 5 ms —
/// over 200x that — because this runs in a debug build on shared CI hardware,
/// where the honest signal is "did the shape change", not "did it get 20%
/// slower". A superlinear regression at this corpus size blows through 5 ms by
/// orders of magnitude; a legitimate 2x does not.
#[test]
fn resolve_context_stays_within_ceiling() {
    let scopes = corpus(50_000, 20);
    let anchor = (50_000 / 2 * 4) as u32;
    let opts = ContextOptions {
        viewport_height: 40,
        viewport_top: anchor.saturating_sub(20),
        ..Default::default()
    };

    // Warm, then measure: the first call touches cold cache lines for a 50k
    // vector and would dominate an unwarmed median.
    for _ in 0..8 {
        let _ = resolve_context(&scopes, anchor, &opts);
    }
    let samples: Vec<Duration> = (0..32)
        .map(|_| {
            let t = Instant::now();
            let out = resolve_context(&scopes, anchor, &opts);
            let elapsed = t.elapsed();
            assert!(!out.is_empty(), "the corpus must actually resolve context");
            elapsed
        })
        .collect();

    let p50 = median(samples);
    assert!(
        p50 < Duration::from_millis(5),
        "resolve_context over 50k scopes took {p50:?} (ceiling 5ms, release \
         median ~21.8us). This gate exists to catch a SHAPE change — a scan \
         that became quadratic, or a clone of the scope list per call — not a \
         modest slowdown."
    );
}

/// The same call on a realistic corpus must be far cheaper still, so a
/// regression that only shows at the pathological end is not the only one this
/// can catch.
#[test]
fn a_realistic_corpus_resolves_in_well_under_a_frame() {
    let scopes = corpus(2_000, 8);
    let anchor = (2_000 / 2 * 4) as u32;
    let opts = ContextOptions {
        viewport_height: 40,
        viewport_top: anchor.saturating_sub(20),
        ..Default::default()
    };
    for _ in 0..8 {
        let _ = resolve_context(&scopes, anchor, &opts);
    }
    let p50 = median(
        (0..32)
            .map(|_| {
                let t = Instant::now();
                let _ = resolve_context(&scopes, anchor, &opts);
                t.elapsed()
            })
            .collect(),
    );
    assert!(
        p50 < Duration::from_micros(500),
        "a 2 000-scope file (a large real source file) resolved in {p50:?}; \
         this runs per pane on every cursor move, so it must stay far inside \
         one 120Hz frame (8.3ms) with room for everything else in the publish"
    );
}
