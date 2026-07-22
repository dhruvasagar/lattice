#![allow(clippy::unwrap_used)]
//! CM.3a — combined parser throughput bench.
//!
//! The compilation stderr reader feeds every captured line to a
//! [`ParserRegistry`] (cargo/rustc + gnu-style) in arrival order. That
//! parse runs in the reader's `spawn_blocking` thread, off the UI /
//! actor thread (paramount goal #1), but it must still keep up with a
//! noisy build — a fast producer streaming thousands of stderr lines
//! must not back up behind the parser.
//!
//! This bench measures the per-line parse cost over a large synthetic
//! fixture that mixes cargo multi-line error blocks, gnu single-line
//! diagnostics, and non-matching noise (progress / prose) in realistic
//! proportions. Reported as time per full fixture pass; divide by the
//! line count for ns/line.

use criterion::{Criterion, black_box, criterion_group, criterion_main};

use lattice_compilation::ParserRegistry;

/// Build a ~5000-line fixture mixing cargo blocks, gnu lines, and noise.
fn fixture(lines_target: usize) -> Vec<String> {
    let mut lines = Vec::with_capacity(lines_target);
    let mut i = 0usize;
    while lines.len() < lines_target {
        // Noise: progress / prose (matches nothing).
        lines.push(format!("   Compiling crate_{i} v0.{i}.0"));
        lines.push("    Finished dev [unoptimized] target(s)".to_string());
        // A cargo multi-line error block (header + location + context).
        lines.push(format!("error[E0308]: mismatched types in item {i}"));
        lines.push(format!(
            "  --> src/module_{i}.rs:{}:{}",
            i % 400 + 1,
            i % 80 + 1
        ));
        lines.push("   |".to_string());
        lines.push(format!("{i:>3} |     let x: u32 = \"s\";"));
        lines.push("   |            ---   ^^^ expected `u32`".to_string());
        // A cargo warning block.
        lines.push(format!("warning: unused variable: `v{i}`"));
        lines.push(format!("  --> src/lib_{i}.rs:{}:5", i % 300 + 1));
        // Gnu single-line diagnostics (full + short forms).
        lines.push(format!(
            "gen_{i}.c:{}:{}: error: undeclared '{i}'",
            i % 200 + 1,
            i % 40 + 1
        ));
        lines.push(format!("Makefile.{i}:{}: missing separator", i % 100 + 1));
        i += 1;
    }
    lines.truncate(lines_target);
    lines
}

fn bench_parse(c: &mut Criterion) {
    let lines = fixture(5_000);
    let mut group = c.benchmark_group("compilation_parse");
    group.throughput(criterion::Throughput::Elements(lines.len() as u64));
    group.bench_function("feed_5000_mixed_lines", |b| {
        b.iter(|| {
            // Fresh registry per pass so pending multi-line state starts
            // clean, exactly as a fresh run's reader does.
            let mut registry = ParserRegistry::with_builtins();
            let mut total = 0usize;
            for line in &lines {
                total += registry.feed(black_box(line)).len();
            }
            black_box(total)
        });
    });
    group.finish();
}

criterion_group!(benches, bench_parse);
criterion_main!(benches);
