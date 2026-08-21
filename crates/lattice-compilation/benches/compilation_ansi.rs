#![allow(clippy::unwrap_used)]
//! CM.5 — ANSI strip + span throughput.
//!
//! Every captured line passes through `clean_line` before the parsers
//! or the buffer see it, in the pipe reader's own thread — off the UI /
//! actor thread (paramount goal #1). It is therefore not on the
//! keystroke path, but it *is* on the critical path of a fast producer:
//! a build streaming thousands of lines a second must not back up
//! behind escape scanning.
//!
//! Three shapes, because the cost profile differs sharply between
//! them and the common one deserves to be measured on its own:
//!
//! - **plain** — no escapes at all. This is the default reality: a
//!   pipe makes cargo and rustc disable colour, so this path runs on
//!   nearly every real build and must be close to free.
//! - **coloured** — `--color=always` output, roughly the SGR density
//!   cargo emits (a bold+colour prefix per diagnostic line).
//! - **escape-heavy** — a progress renderer's cursor moves and `\r`
//!   redraws, the worst realistic case for scan-and-discard work.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use lattice_compilation::{AnsiPalette, SgrState, clean_line};

/// The bench does not stand up a theme registry, so the palette is
/// built directly from interned-looking ids. `clean_line` only ever
/// copies them into spans, so the values are irrelevant to the cost —
/// what matters is that colouring is switched ON, which is the more
/// expensive path.
fn palette() -> AnsiPalette {
    let mut colors = [lattice_theme::ElementId(0); 16];
    for (i, c) in colors.iter_mut().enumerate() {
        *c = lattice_theme::ElementId(i as u32);
    }
    AnsiPalette {
        colors,
        bold: lattice_theme::ElementId(100),
    }
}

fn plain(lines: usize) -> Vec<String> {
    (0..lines)
        .map(|i| match i % 4 {
            0 => format!("   Compiling lattice-core v0.1.{i}"),
            1 => format!("src/main.rs:{i}:5: error: mismatched types"),
            2 => "    |".to_string(),
            _ => format!("warning: unused variable `x{i}`"),
        })
        .collect()
}

fn coloured(lines: usize) -> Vec<String> {
    (0..lines)
        .map(|i| match i % 4 {
            0 => format!("\u{1b}[1m\u{1b}[32m   Compiling\u{1b}[0m lattice-core v0.1.{i}"),
            1 => format!(
                "\u{1b}[1m\u{1b}[31merror\u{1b}[0m\u{1b}[1m: mismatched types\u{1b}[0m\n\
                 \u{1b}[1m\u{1b}[34m  -->\u{1b}[0m src/main.rs:{i}:5"
            ),
            2 => "\u{1b}[1m\u{1b}[34m    |\u{1b}[0m".to_string(),
            _ => format!("\u{1b}[1m\u{1b}[33mwarning\u{1b}[0m: unused variable `x{i}`"),
        })
        .collect()
}

fn escape_heavy(lines: usize) -> Vec<String> {
    (0..lines)
        .map(|i| {
            format!(
                "\u{1b}[2K\u{1b}[1G   Building [{}] {i}/1000\r\
                 \u{1b}[2K\u{1b}[1G   Building [{}] {}/1000",
                "=".repeat(i % 30),
                "=".repeat((i + 1) % 30),
                i + 1
            )
        })
        .collect()
}

fn bench_ansi(c: &mut Criterion) {
    const LINES: usize = 2000;
    let p = palette();

    for (name, fixture) in [
        ("plain", plain(LINES)),
        ("coloured", coloured(LINES)),
        ("escape_heavy", escape_heavy(LINES)),
    ] {
        let bytes: usize = fixture.iter().map(|l| l.len()).sum();
        let mut group = c.benchmark_group("compilation_ansi");
        group.throughput(criterion::Throughput::Bytes(bytes as u64));
        group.bench_function(name, |b| {
            b.iter(|| {
                let mut state = SgrState::default();
                for line in &fixture {
                    black_box(clean_line(black_box(line), &mut state, Some(&p)));
                }
            })
        });
        group.finish();
    }
}

criterion_group!(benches, bench_ansi);
criterion_main!(benches);
