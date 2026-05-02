#![allow(clippy::unwrap_used, clippy::panic)]
//! Criterion benchmarks for `lattice_core::search`.
//!
//! Search is a Reflex-class command (vim's `/`, `n`, `?`); it must
//! complete inside the keystroke budget on representative buffers.
//!
//! After the fancy-regex switch, every bench compiles a
//! `fancy_regex::Regex` once and reuses it across iterations. The
//! pattern is a literal so `regex` crate's literal-extraction
//! optimisation routes through the same memmem prefilter we used
//! pre-switch -- numbers should match the B-β baseline closely.
//!
//! A separate group, `search::regex_*`, exercises patterns that
//! actually need the regex engine (alternation, character class,
//! pattern backref) to characterise the cost of the
//! "non-literal" path.

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

use fancy_regex::Regex;
use lattice_core::Buffer;
use lattice_core::search::{Direction, find};
use lattice_protocol::position::Position;

fn build_buffer(n_lines: usize) -> String {
    let mut s = String::with_capacity(n_lines * 64);
    for i in 0..n_lines {
        s.push_str(&format!(
            "fn handler_{i}(input: &str) -> Result<Output, Error> {{ Ok(()) }}\n"
        ));
    }
    s
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("test pattern compiles")
}

fn search_forward_first_match(c: &mut Criterion) {
    let mut g = c.benchmark_group("search::forward_first_match");
    let regex = re("Result");
    for size in [10usize, 1_000, 50_000, 200_000] {
        let text = build_buffer(size);
        let buffer = Buffer::from_text(&text);
        g.throughput(Throughput::Bytes(text.len() as u64));
        g.bench_with_input(
            BenchmarkId::from_parameter(size),
            &buffer,
            |bencher, buf| {
                bencher.iter(|| {
                    let _ = find(buf, black_box(&regex), Position::ZERO, Direction::Forward)
                        .unwrap();
                });
            },
        );
    }
    g.finish();
}

fn search_forward_last_match(c: &mut Criterion) {
    // Worst-case forward scan: pattern is at the very end of the buffer.
    let mut g = c.benchmark_group("search::forward_last_match");
    let regex = re("MARKER");
    for size in [10usize, 1_000, 50_000, 200_000] {
        let mut text = build_buffer(size);
        text.push_str("// MARKER\n");
        let buffer = Buffer::from_text(&text);
        g.throughput(Throughput::Bytes(text.len() as u64));
        g.bench_with_input(
            BenchmarkId::from_parameter(size),
            &buffer,
            |bencher, buf| {
                bencher.iter(|| {
                    let _ = find(buf, black_box(&regex), Position::ZERO, Direction::Forward)
                        .unwrap();
                });
            },
        );
    }
    g.finish();
}

fn search_no_match_with_wrap(c: &mut Criterion) {
    // Worst case: pattern not present anywhere -- forces a full forward scan
    // and a full wrap pass before returning None.
    let mut g = c.benchmark_group("search::no_match_with_wrap");
    let regex = re("ZZZ_MISSING");
    for size in [10usize, 1_000, 50_000, 200_000] {
        let text = build_buffer(size);
        let buffer = Buffer::from_text(&text);
        g.throughput(Throughput::Bytes(text.len() as u64));
        g.bench_with_input(
            BenchmarkId::from_parameter(size),
            &buffer,
            |bencher, buf| {
                bencher.iter(|| {
                    let _ = find(
                        buf,
                        black_box(&regex),
                        Position::new((size as u32) / 2, 0),
                        Direction::Forward,
                    )
                    .unwrap();
                });
            },
        );
    }
    g.finish();
}

fn search_backward(c: &mut Criterion) {
    let mut g = c.benchmark_group("search::backward");
    let regex = re("Result");
    for size in [10usize, 1_000, 50_000] {
        let text = build_buffer(size);
        let buffer = Buffer::from_text(&text);
        g.throughput(Throughput::Bytes(text.len() as u64));
        g.bench_with_input(
            BenchmarkId::from_parameter(size),
            &buffer,
            |bencher, buf| {
                bencher.iter(|| {
                    let _ = find(
                        buf,
                        black_box(&regex),
                        Position::new((size as u32).saturating_sub(1), 0),
                        Direction::Backward,
                    )
                    .unwrap();
                });
            },
        );
    }
    g.finish();
}

/// Patterns that use regex features beyond a single literal.
/// Quantifies the cost of the "actual regex engine" path vs. the
/// SIMD-prefiltered literal path measured above.
fn search_regex_features(c: &mut Criterion) {
    let mut g = c.benchmark_group("search::regex");
    let text = build_buffer(50_000);
    let buffer = Buffer::from_text(&text);

    // Alternation -- regex crate literal-set extraction kicks in.
    let alt = re(r"(handler_42|handler_4242|handler_42420)");
    g.bench_function("alternation_50k", |bencher| {
        bencher.iter(|| {
            let _ = find(&buffer, black_box(&alt), Position::ZERO, Direction::Forward).unwrap();
        });
    });

    // Character class with quantifier -- general regex engine.
    let class = re(r"\bhandler_\d{4,6}\b");
    g.bench_function("class_quantifier_50k", |bencher| {
        bencher.iter(|| {
            let _ = find(&buffer, black_box(&class), Position::ZERO, Direction::Forward).unwrap();
        });
    });

    // Pattern with backref -- forces fancy-regex's NFA path.
    let backref = re(r"(handler_\d+)\b.*\b\1");
    g.bench_function("backref_50k", |bencher| {
        bencher.iter(|| {
            let _ = find(&buffer, black_box(&backref), Position::ZERO, Direction::Forward).unwrap();
        });
    });

    g.finish();
}

criterion_group!(
    benches,
    search_forward_first_match,
    search_forward_last_match,
    search_no_match_with_wrap,
    search_backward,
    search_regex_features,
);
criterion_main!(benches);
