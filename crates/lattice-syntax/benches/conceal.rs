//! H.3 (2026-08-29): what conceal costs a display-matrix rebuild.
//!
//! `conceal_spans` is the one new per-line cost the conceal design
//! adds, and `conceal.md` promised the number would land in
//! `benchmarks.md` rather than being asserted in prose. This measures
//! it directly rather than through `recompute`, because reaching the
//! rules through the full rebuild path requires a registered wasm
//! grammar — which would measure Cranelift, not conceal.
//!
//! Three workloads, chosen so the comparison answers the two questions
//! the design makes claims about:
//!
//! - `no_rules` — the empty-rule-set early return. This is the path
//!   every buffer in the editor but org takes, and the claim is that it
//!   costs a branch. It is benched so "costs nothing" is a measurement.
//! - `org_rules_link_dense` — org's real two rules over a line with
//!   three links, the worst realistic line.
//! - `org_rules_no_match` — org's rules over ordinary prose. The common
//!   case *within* an org buffer: most lines hold no link at all, and
//!   if the miss were expensive the per-viewport cost would be set by
//!   line count rather than by link count.
//!
//! Multiply the per-line figure by ~50 (a viewport) for the per-rebuild
//! cost, and note the rebuild is off the UI thread.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use lattice_syntax::conceal::{ConcealRule, compile_rules, conceal_spans};

/// Org's two rules, exactly as the plugin declares them — slots included.
///
/// OL.1: the slots matter to what this measures. `conceal_style_spans` runs
/// beside `conceal_spans` on every rebuilt line, so a fixture that omitted
/// them would benchmark a shape org no longer ships and quietly stop covering
/// the styling walk.
fn org_rules() -> Vec<ConcealRule> {
    let (ok, errs) = compile_rules(
        &[
            (
                r"(\[\[[^]]+\]\[)[^]]+(\]\])".to_string(),
                vec![1, 2],
                Some("text.reference".to_string()),
            ),
            (
                r"(\[\[)([^]]+)(\]\])".to_string(),
                vec![1, 3],
                Some("text.uri".to_string()),
            ),
        ],
        None,
    );
    assert!(errs.is_empty(), "{errs:?}");
    ok
}

const LINK_DENSE: &str = "- See [[id:6F398E54-7E63-4492-9EB6-89C8A90E7DD3][Project Kickoff \
                          Checklist]], [[id:3C6D10DB-3272-4E96-A3F3-D2E6BC835708][ComfyUI \
                          Workflow]] and [[https://example.com/some/long/path]] before Friday.";

const PROSE: &str = "- The purpose of this document is to ensure that every project begins \
                     with a shared understanding of scope, risk and readiness.";

fn bench_conceal_spans(c: &mut Criterion) {
    let rules = org_rules();
    let mut g = c.benchmark_group("conceal_spans");

    // The zero-cost claim, measured. `conceal_spans` returns before
    // touching the line when the rule list is empty.
    g.bench_function("no_rules", |b| {
        b.iter(|| conceal_spans(black_box(&[]), black_box(LINK_DENSE)))
    });

    g.bench_function("org_rules_link_dense", |b| {
        b.iter(|| conceal_spans(black_box(&rules), black_box(LINK_DENSE)))
    });

    g.bench_function("org_rules_no_match", |b| {
        b.iter(|| conceal_spans(black_box(&rules), black_box(PROSE)))
    });

    g.finish();
}

criterion_group!(benches, bench_conceal_spans);
criterion_main!(benches);
