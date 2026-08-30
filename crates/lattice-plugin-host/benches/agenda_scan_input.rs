#![allow(clippy::unwrap_used, clippy::panic)]
//! OT.3 criterion bench for the agenda `scan-input` trade.
//!
//! `scanned-excerpt-source.wit` used to justify handing the guest a whole file's text:
//! "a scan reads EVERY line, so a handle would cost one boundary crossing per
//! line where one copy costs one." OT.3 retired that as circular — the guest
//! reads every line only *because it hand-parses* — and switched `scan` to lend
//! a borrowed `tree-snapshot` instead, parsed host-side from text the host had
//! already read.
//!
//! That swap is **not free in one direction**. It removes a per-file string copy
//! across the WASM boundary and adds a per-file tree-sitter parse. The slice
//! plan (D3) is explicit that raw parsing is SLOWER than a line-prefix scan and
//! that the net must be measured rather than assumed, because the agenda scan is
//! on a producer's critical path. This bench is that measurement.
//!
//! Two numbers, both per file:
//!
//!   * `parse` — what the host now pays. `Syntax::parse` over the file.
//!   * `text_copy` — what crossing the boundary cost before: the `String`
//!     allocation + copy `call_scan(store, path, text)` made per file.
//!
//! **What this deliberately does not claim.** The guest-side saving is real and
//! larger than either number — a query answered host-side against ranges replaces
//! reading every line inside the guest — but it is guest-specific, so measuring
//! it here would be measuring one plugin's parser rather than the seam. Read
//! these two as the HOST-side cost of the trade; the guest side only improves it.
//!
//! Markdown, not org: org's grammar is the org plugin's build artefact and is
//! not linked here. Markdown is the closest bundled analogue — headings plus
//! prose, the same shape the agenda walks.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use lattice_syntax::{Lang, Syntax};

/// A file shaped like the org files an agenda walk actually meets: mostly prose,
/// every fifth line a heading, a few hundred lines long.
fn corpus(lines: usize) -> String {
    let mut s = String::with_capacity(lines * 48);
    for i in 0..lines {
        if i % 5 == 0 {
            s.push_str(&format!("# Heading {i}\n"));
        } else {
            s.push_str(&format!("some ordinary body text on line {i}\n"));
        }
    }
    s
}

fn bench_scan_input(c: &mut Criterion) {
    // 400 lines — a large-ish real org file. The per-file cost is what matters
    // (a scan pays it once per file), not the per-byte rate.
    let src = corpus(400);

    // Faithful to `parse_for_scan` as first written: a fresh `Syntax` per file.
    c.bench_function("agenda_scan_input/parse_fresh_parser", |b| {
        b.iter(|| {
            let mut syntax = Syntax::for_language(Lang::Markdown).unwrap().unwrap();
            syntax.parse(black_box(&src));
            black_box(syntax.snapshot_owned())
        })
    });

    // The same parse with the parser already built — i.e. what a scan pays per
    // file if construction is hoisted out of the per-file loop. The gap between
    // this and `parse_fresh_parser` IS the cost of rebuilding the parser 200
    // times for a 200-file project, and is the reason the scan caches it.
    c.bench_function("agenda_scan_input/parse_reused_parser", |b| {
        let mut syntax = Syntax::for_language(Lang::Markdown).unwrap().unwrap();
        b.iter(|| {
            syntax.parse(black_box(&src));
            black_box(syntax.snapshot_owned())
        })
    });

    // Sanity arm: markdown's grammar is two-pass with an external scanner and
    // is among the slowest bundled ones. Rust is a conventional single-pass
    // grammar, and org's is simpler still — so this brackets how much of the
    // number above is "parsing" versus "markdown".
    c.bench_function("agenda_scan_input/parse_rust_for_scale", |b| {
        let rust_src: String = (0..400)
            .map(|i| format!("fn f{i}() {{ let x = {i}; }}\n"))
            .collect();
        let mut syntax = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        b.iter(|| {
            syntax.parse(black_box(&rust_src));
            black_box(syntax.snapshot_owned())
        })
    });

    c.bench_function("agenda_scan_input/text_copy", |b| {
        b.iter(|| black_box(black_box(&src).to_string()))
    });
}

criterion_group!(agenda_scan_input, bench_scan_input);
criterion_main!(agenda_scan_input);
