#![allow(clippy::unwrap_used, clippy::panic)]
//! Criterion benchmarks for `lattice-picker`. Per
//! `docs/dev/architecture/picker.md` § 9.2:
//!
//! - `open_inline` × {100, 500, 5000} -- cost of seeding a
//!   picker with N inline candidates including MRU bonus
//!   snapshot.
//! - `refilter` × {empty, 1-char, 5-char} × {500, 5000} --
//!   per-keystroke filter+rank pass. The hot path; design
//!   goal §8.2 keeps this sub-frame.
//! - `mru_snapshot` -- O(N) bonus-cache pass at picker-open.
//! - `mru_record` -- single-accept cost. Sub-microsecond
//!   target since accept is user-driven.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use lattice_completion::{CandidateKind, RawCandidate};
use lattice_picker::{
    DEFAULT_HALF_LIFE, Picker, PickerAction, PickerMruIndex, PickerSource, RoutingPayload,
    bonus_of, routing_identity,
};

/// Build N `(RawCandidate, RoutingPayload)` pairs that
/// mimic the `:picker files` shape: each candidate's
/// display is a unique file path so the matcher's substring
/// path runs on realistic inputs.
fn build_pairs(n: usize) -> Vec<(RawCandidate, RoutingPayload)> {
    (0..n)
        .map(|i| {
            let path = format!("src/module/{i:05}/file_{i}.rs");
            let mut cand = RawCandidate::plain(path.clone(), CandidateKind::Plain);
            cand.display = path.clone();
            (
                cand,
                RoutingPayload::OpenFile { path: PathBuf::from(path) },
            )
        })
        .collect()
}

/// Build an MRU index pre-populated with `n` entries
/// matching the candidates produced by `build_pairs`. Used
/// to make the bonus snapshot pass non-trivial (every
/// candidate hits an entry).
fn build_mru(n: usize, now: SystemTime) -> PickerMruIndex {
    let mut mru = PickerMruIndex::new();
    for i in 0..n {
        let path = format!("src/module/{i:05}/file_{i}.rs");
        let identity = format!("file:{path}");
        // Vary `last_used` so frecency math actually computes
        // distinct bonuses rather than every entry being
        // "just now."
        let age = Duration::from_secs((i % 86400) as u64);
        mru.record_at("files", &identity, now - age);
    }
    mru
}

/// Pre-compute the bonus vec for `pairs` against `mru` --
/// the same shape the host calls
/// `Picker::set_mru_bonuses` with.
fn compute_bonuses(
    mru: &PickerMruIndex,
    pairs: &[(RawCandidate, RoutingPayload)],
    now: SystemTime,
) -> Vec<f64> {
    pairs
        .iter()
        .map(|(_, routing)| match routing_identity(routing) {
            Some(id) => mru.frecency_bonus("files", &id, now, DEFAULT_HALF_LIFE),
            None => 0.0,
        })
        .collect()
}

/// Build a fully-seated picker (raw + routing + bonuses) so
/// refilter benches don't re-allocate the candidate set on
/// every iteration.
fn seated_picker(n: usize) -> Picker {
    let pairs = build_pairs(n);
    let mru = build_mru(n, SystemTime::now());
    let bonuses = compute_bonuses(&mru, &pairs, SystemTime::now());
    let mut picker = Picker::new("files", PickerSource::Files, PickerAction::OpenFile);
    picker.set_raw_candidates_with_routing_and_bonuses(pairs, bonuses);
    picker
}

/// Bench: open_inline. Measures the host-side seat cost --
/// builds candidates, stamps MRU bonuses, refilters with an
/// empty query. The whole inline-open hot path end-to-end.
fn open_inline(c: &mut Criterion) {
    let mut g = c.benchmark_group("picker::open_inline");
    for size in [100usize, 500, 5_000] {
        g.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &n| {
            b.iter_with_setup(
                || {
                    let pairs = build_pairs(n);
                    let mru = build_mru(n, SystemTime::now());
                    (pairs, mru)
                },
                |(pairs, mru)| {
                    let now = SystemTime::now();
                    let bonuses = compute_bonuses(&mru, &pairs, now);
                    let mut picker = Picker::new(
                        "files",
                        PickerSource::Files,
                        PickerAction::OpenFile,
                    );
                    // Single-pass seat (one refilter); matches
                    // the host's trait-driven open path.
                    picker.set_raw_candidates_with_routing_and_bonuses(pairs, bonuses);
                    black_box(picker);
                },
            );
        });
    }
    g.finish();
}

/// Bench: refilter. Measures one keystroke's worth of
/// rank-and-sort against a pre-seated picker. Three query
/// shapes (empty, 1-char, 5-char) crossed with two candidate
/// counts -- mirrors the design fragment's matrix.
fn refilter(c: &mut Criterion) {
    let mut g = c.benchmark_group("picker::refilter");
    for size in [500usize, 5_000] {
        let picker = seated_picker(size);
        for query in ["", "f", "file_"] {
            let id = format!("n={size},query={:?}", query);
            g.bench_with_input(BenchmarkId::from_parameter(id), &query, |b, &q| {
                b.iter_with_setup(
                    || {
                        let mut p = picker.clone();
                        p.query.clear();
                        for c in q.chars() {
                            p.query.push(c);
                        }
                        p.query_cursor = p.query.len();
                        p
                    },
                    |mut p| {
                        p.refilter();
                        black_box(p);
                    },
                );
            });
        }
    }
    g.finish();
}

/// Bench: mru_snapshot. Measures the O(N) bonus-cache pass
/// the host runs once at picker-open. Pure host-side work;
/// no picker mutation.
fn mru_snapshot(c: &mut Criterion) {
    let mut g = c.benchmark_group("picker::mru_snapshot");
    for size in [100usize, 500, 5_000] {
        g.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &n| {
            let pairs = build_pairs(n);
            let mru = build_mru(n, SystemTime::now());
            b.iter(|| {
                let now = SystemTime::now();
                let bonuses = compute_bonuses(black_box(&mru), black_box(&pairs), now);
                black_box(bonuses);
            });
        });
    }
    g.finish();
}

/// Bench: mru_record. Single-accept cost -- record a fresh
/// identity into a populated index. Sub-microsecond budget
/// (the path is one HashMap insert + an Arc-swap-write).
fn mru_record(c: &mut Criterion) {
    let mut g = c.benchmark_group("picker::mru_record");
    for size in [100usize, 1_000] {
        g.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &n| {
            b.iter_with_setup(
                || build_mru(n, SystemTime::now()),
                |mut mru| {
                    // Record a brand-new identity (not in the
                    // pre-populated set) so we measure the
                    // insert + cap-check path, not the
                    // touch-existing fast path.
                    mru.record_at(
                        "files",
                        "file:src/module/99999/new.rs",
                        SystemTime::now(),
                    );
                    black_box(mru);
                },
            );
        });
    }
    g.finish();
}

/// Bench: bonus_of. Smallest unit -- the frecency math
/// itself. Should be a handful of nanoseconds; if this
/// regresses something has gone wrong with the formula.
fn bonus_math(c: &mut Criterion) {
    let now = SystemTime::now();
    let entry = lattice_picker::MruEntry {
        last_used: now - Duration::from_secs(86400),
        use_count: 5,
    };
    c.bench_function("picker::bonus_of", |b| {
        b.iter(|| {
            let bonus = bonus_of(black_box(&entry), now, DEFAULT_HALF_LIFE);
            black_box(bonus);
        });
    });
}

criterion_group!(
    benches,
    open_inline,
    refilter,
    mru_snapshot,
    mru_record,
    bonus_math
);
criterion_main!(benches);
