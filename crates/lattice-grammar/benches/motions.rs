#![allow(clippy::unwrap_used, clippy::panic)]
//! Criterion benchmarks for motion evaluators.
//!
//! These benches back the §8.2 / §5.2.5 latency commitments:
//! Reflex commands (motions are Reflex) must commit a sync `Effect`
//! within the keystroke budget (<2ms p99) on representative buffers.
//!
//! The benchmark harness measures `execute(..)` latency for each motion
//! against three buffer sizes (small/medium/large) so regressions on
//! either small-file ergonomics or large-file scaling surface in CI.

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

use lattice_core::Document;
use lattice_grammar::{
    CancellationToken, CommandRegistry,
    builtins::{Builtins, populate},
    command::{CommandInvocation, Count},
    dispatcher::execute,
};
use lattice_protocol::position::Position;

/// Build a buffer of `n` repeated lines so we have realistic line breaks
/// and word boundaries for the motion engine to walk through.
fn build_buffer(n_lines: usize) -> String {
    let mut s = String::with_capacity(n_lines * 64);
    for i in 0..n_lines {
        s.push_str(&format!(
            "fn handler_{i}(input: &str) -> Result<Output, Error> {{\n"
        ));
    }
    s
}

fn motion_word_forward(c: &mut Criterion) {
    let mut g = c.benchmark_group("motion::word_forward");
    let mut registry = CommandRegistry::new();
    let b: Builtins = populate(&mut registry);

    for size in [10usize, 1_000, 50_000] {
        let text = build_buffer(size);
        let bytes = text.len();
        g.throughput(Throughput::Bytes(bytes as u64));
        g.bench_with_input(BenchmarkId::from_parameter(size), &text, |bencher, t| {
            let mut doc = Document::from_text(t);
            let inv = CommandInvocation::of(b.word_forward.0);
            bencher.iter(|| {
                let _ = execute(
                    &registry,
                    &mut doc,
                    lattice_core::BufferId(0), black_box(Position::ZERO),
                    inv.clone(),
                    &CancellationToken::never(),
                )
                .unwrap();
            });
        });
    }
    g.finish();
}

fn motion_word_backward(c: &mut Criterion) {
    let mut g = c.benchmark_group("motion::word_backward");
    let mut registry = CommandRegistry::new();
    let b = populate(&mut registry);

    for size in [10usize, 1_000, 50_000] {
        let text = build_buffer(size);
        let last_line = (size as u32).saturating_sub(1);
        let line_len = "fn handler_X(input: &str) -> Result<Output, Error> {".len() as u32;
        g.bench_with_input(BenchmarkId::from_parameter(size), &text, |bencher, t| {
            let mut doc = Document::from_text(t);
            let inv = CommandInvocation::of(b.word_backward.0);
            bencher.iter(|| {
                let _ = execute(
                    &registry,
                    &mut doc,
                    lattice_core::BufferId(0), black_box(Position::new(last_line, line_len)),
                    inv.clone(),
                    &CancellationToken::never(),
                )
                .unwrap();
            });
        });
    }
    g.finish();
}

fn motion_word_end(c: &mut Criterion) {
    let mut g = c.benchmark_group("motion::word_end");
    let mut registry = CommandRegistry::new();
    let b = populate(&mut registry);

    for size in [10usize, 1_000, 50_000] {
        let text = build_buffer(size);
        g.bench_with_input(BenchmarkId::from_parameter(size), &text, |bencher, t| {
            let mut doc = Document::from_text(t);
            let inv = CommandInvocation::of(b.word_end.0);
            bencher.iter(|| {
                let _ = execute(
                    &registry,
                    &mut doc,
                    lattice_core::BufferId(0), black_box(Position::ZERO),
                    inv.clone(),
                    &CancellationToken::never(),
                )
                .unwrap();
            });
        });
    }
    g.finish();
}

fn motion_first_non_blank(c: &mut Criterion) {
    let mut g = c.benchmark_group("motion::first_non_blank");
    let mut registry = CommandRegistry::new();
    let b = populate(&mut registry);

    let text = "    indented hello world\n".repeat(50_000);
    g.bench_function("indented-50k", |bencher| {
        let mut doc = Document::from_text(&text);
        let inv = CommandInvocation::of(b.first_non_blank.0);
        bencher.iter(|| {
            let _ = execute(
                &registry,
                &mut doc,
                lattice_core::BufferId(0), black_box(Position::new(25_000, 0)),
                inv.clone(),
                &CancellationToken::never(),
            )
            .unwrap();
        });
    });
    g.finish();
}

fn motion_word_forward_with_count(c: &mut Criterion) {
    let mut g = c.benchmark_group("motion::word_forward_count");
    let mut registry = CommandRegistry::new();
    let b = populate(&mut registry);

    let text = "alpha beta gamma delta epsilon zeta eta theta ".repeat(100);
    g.bench_function("count_50_in_100x_buffer", |bencher| {
        let mut doc = Document::from_text(&text);
        let inv = CommandInvocation::of(b.word_forward.0).with_count(Count(50));
        bencher.iter(|| {
            let _ = execute(
                &registry,
                &mut doc,
                lattice_core::BufferId(0), black_box(Position::ZERO),
                inv.clone(),
                &CancellationToken::never(),
            )
            .unwrap();
        });
    });
    g.finish();
}

fn motion_find_char_forward(c: &mut Criterion) {
    let mut g = c.benchmark_group("motion::find_char_forward");
    let mut registry = CommandRegistry::new();
    let b = populate(&mut registry);

    // f<char> only scans the current line, so a "wide" line is the worst case.
    let line = "the quick brown fox jumps over the lazy dog: ".repeat(20);
    let text = format!("{line}\n");
    g.bench_function("wide_line_900_chars", |bencher| {
        let mut doc = Document::from_text(&text);
        let inv = CommandInvocation::of(b.find_char_forward.0)
            .with_args(lattice_grammar::Args::Char('z'));
        bencher.iter(|| {
            let _ = execute(
                &registry,
                &mut doc,
                lattice_core::BufferId(0), black_box(Position::ZERO),
                inv.clone(),
                &CancellationToken::never(),
            )
            .unwrap();
        });
    });
    g.finish();
}

criterion_group!(
    benches,
    motion_word_forward,
    motion_word_backward,
    motion_word_end,
    motion_first_non_blank,
    motion_word_forward_with_count,
    motion_find_char_forward,
);
criterion_main!(benches);
