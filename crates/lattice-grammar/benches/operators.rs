#![allow(clippy::unwrap_used, clippy::panic)]
//! Criterion benchmarks for operator dispatch.
//!
//! Operators are Reflex (per §5.2.5) -- the keystroke budget applies.
//! These benches stress the `dw` / `db` / `dd` paths.

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};

use lattice_core::Document;
use lattice_grammar::{
    Args, CancellationToken, Range, Target,
    builtins::populate,
    command::CommandInvocation,
    dispatcher::execute,
    registry::CommandRegistry,
};
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

fn operator_dw(c: &mut Criterion) {
    let mut g = c.benchmark_group("operator::dw");
    let mut registry = CommandRegistry::new();
    let b = populate(&mut registry);

    for size in [10usize, 1_000, 50_000] {
        let text = build_buffer(size);
        g.bench_with_input(BenchmarkId::from_parameter(size), &text, |bencher, t| {
            bencher.iter_with_setup(
                || Document::from_text(t),
                |mut doc| {
                    let inv = CommandInvocation::of(b.delete.0)
                        .with_target(Target::Motion(b.word_forward, Args::None));
                    let _ = execute(
                        &registry,
                        &mut doc,
                        black_box(Position::ZERO),
                        inv,
                        &CancellationToken::never(),
                    )
                    .unwrap();
                },
            );
        });
    }
    g.finish();
}

fn operator_dd(c: &mut Criterion) {
    let mut g = c.benchmark_group("operator::dd");
    let mut registry = CommandRegistry::new();
    let b = populate(&mut registry);

    for size in [10usize, 1_000, 50_000] {
        let text = build_buffer(size);
        g.bench_with_input(BenchmarkId::from_parameter(size), &text, |bencher, t| {
            bencher.iter_with_setup(
                || Document::from_text(t),
                |mut doc| {
                    let inv =
                        CommandInvocation::of(b.delete.0).with_range(Range::CurrentLine);
                    let _ = execute(
                        &registry,
                        &mut doc,
                        black_box(Position::ZERO),
                        inv,
                        &CancellationToken::never(),
                    )
                    .unwrap();
                },
            );
        });
    }
    g.finish();
}

fn operator_d_whole(c: &mut Criterion) {
    let mut g = c.benchmark_group("operator::d_whole");
    let mut registry = CommandRegistry::new();
    let b = populate(&mut registry);

    // d% (or :%d) -- delete the whole buffer. Stress on a large file.
    for size in [10usize, 1_000, 50_000] {
        let text = build_buffer(size);
        g.bench_with_input(BenchmarkId::from_parameter(size), &text, |bencher, t| {
            bencher.iter_with_setup(
                || Document::from_text(t),
                |mut doc| {
                    let inv = CommandInvocation::of(b.delete.0).with_range(Range::Whole);
                    let _ = execute(
                        &registry,
                        &mut doc,
                        black_box(Position::ZERO),
                        inv,
                        &CancellationToken::never(),
                    )
                    .unwrap();
                },
            );
        });
    }
    g.finish();
}

fn operator_yw(c: &mut Criterion) {
    let mut g = c.benchmark_group("operator::yw");
    let mut registry = CommandRegistry::new();
    let b = populate(&mut registry);

    for size in [10usize, 1_000, 50_000] {
        let text = build_buffer(size);
        g.bench_with_input(BenchmarkId::from_parameter(size), &text, |bencher, t| {
            bencher.iter_with_setup(
                || Document::from_text(t),
                |mut doc| {
                    let inv = CommandInvocation::of(b.yank.0)
                        .with_target(Target::Motion(b.word_forward, Args::None));
                    let _ = execute(&registry, &mut doc, black_box(Position::ZERO), inv, &CancellationToken::never()).unwrap();
                },
            );
        });
    }
    g.finish();
}

fn operator_cw(c: &mut Criterion) {
    let mut g = c.benchmark_group("operator::cw");
    let mut registry = CommandRegistry::new();
    let b = populate(&mut registry);

    for size in [10usize, 1_000, 50_000] {
        let text = build_buffer(size);
        g.bench_with_input(BenchmarkId::from_parameter(size), &text, |bencher, t| {
            bencher.iter_with_setup(
                || Document::from_text(t),
                |mut doc| {
                    let inv = CommandInvocation::of(b.change.0)
                        .with_target(Target::Motion(b.word_forward, Args::None));
                    let _ = execute(&registry, &mut doc, black_box(Position::ZERO), inv, &CancellationToken::never()).unwrap();
                },
            );
        });
    }
    g.finish();
}

fn operator_diw(c: &mut Criterion) {
    let mut g = c.benchmark_group("operator::diw");
    let mut registry = CommandRegistry::new();
    let b = populate(&mut registry);

    for size in [10usize, 1_000, 50_000] {
        let text = build_buffer(size);
        g.bench_with_input(BenchmarkId::from_parameter(size), &text, |bencher, t| {
            bencher.iter_with_setup(
                || Document::from_text(t),
                |mut doc| {
                    let inv = CommandInvocation::of(b.delete.0)
                        .with_target(Target::TextObject(b.inner_word, Args::None));
                    let _ = execute(
                        &registry,
                        &mut doc,
                        black_box(Position::new(0, 5)),
                        inv,
                        &CancellationToken::never(),
                    )
                    .unwrap();
                },
            );
        });
    }
    g.finish();
}

fn operator_di_paren(c: &mut Criterion) {
    let mut g = c.benchmark_group("operator::di_paren");
    let mut registry = CommandRegistry::new();
    let b = populate(&mut registry);

    // Bracket scanning is O(N) on the buffer at worst; bench the worst
    // case (paren near start, cursor near end).
    let mut text = String::from("call(");
    for i in 0..2_000 {
        text.push_str(&format!("arg{i}, "));
    }
    text.push(')');
    g.bench_function("deep_arg_list", |bencher| {
        bencher.iter_with_setup(
            || Document::from_text(&text),
            |mut doc| {
                let inv = CommandInvocation::of(b.delete.0)
                    .with_target(Target::TextObject(b.inner_paren, Args::None));
                // Cursor in the middle of the arg list.
                let _ = execute(
                    &registry,
                    &mut doc,
                    black_box(Position::new(0, 5_000)),
                    inv,
                    &CancellationToken::never(),
                )
                .unwrap();
            },
        );
    });
    g.finish();
}

criterion_group!(
    benches,
    operator_dw,
    operator_dd,
    operator_d_whole,
    operator_yw,
    operator_cw,
    operator_diw,
    operator_di_paren,
);
criterion_main!(benches);
