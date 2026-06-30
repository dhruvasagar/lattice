#![allow(clippy::unwrap_used)]
//! MARG.4 (2026-06-03): benches for the typed-annotation
//! pipeline (DESIGN.md §5.11.3, marginalia.md).
//!
//! Measures the cost of the annotator stage that the cmdline
//! popup runs on every keystroke that re-filters the
//! candidate list. The hot path is:
//!
//! 1. Walk the `Vec<ScoredCandidate>` produced by the matcher.
//! 2. For each, invoke every registered annotator's
//!    `annotate(&mut RenderedCandidate)`.
//! 3. Renderer consumes the typed `Vec<Annotation>` and paints.
//!
//! Per CLAUDE.md heuristic #5 (four-artefact discipline): every
//! non-trivial design change ships the bench alongside the
//! code, doc, and tests. The keybinding annotator's lookup
//! cost is the budget-sensitive one — it runs N times per
//! popup-open, where N is the visible candidate count
//! (~20-50 typical, up to ~1000 for full `:` enumeration).
//!
//! Targets (dev box; see `docs/dev/operations/benchmarks.md`
//! hardware caveat for headroom against user hardware):
//!
//! - `annotate_pipeline_1000_3stage`: full pipeline (kind +
//!   doc + keybinding) on 1000 candidates. Target < 200 µs
//!   on the dev box so 5× slower user hardware lands under
//!   the one-frame ceiling (8.3 ms at 120 Hz) with plenty of headroom.
//! - `keybinding_annotator_1000`: keybinding annotator only,
//!   1000 candidates. Isolates the reverse-cache lookup
//!   cost so reverse-cache regressions surface here without
//!   noise from kind / doc stages.
//! - `annotation_display_text_per_variant`: per-variant
//!   `display_text()` cost. Renderer calls this once per
//!   visible row × annotation; cheap by design (`Cow::Borrowed`
//!   for string variants), the bench locks the assumption.

use std::collections::HashMap;
use std::sync::Arc;

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use lattice_completion::{
    Annotation, CandidateData, CandidateKind, DocSnippetAnnotator, KeybindingAnnotator,
    KeymapReverseLookup, KindLabelAnnotator, MatchScore, RawCandidate, RenderedCandidate,
};
use lattice_grammar::source::SourceLocation;
use lattice_protocol::KeyChord;

const BENCH_CANDIDATE_COUNT: usize = 1000;

/// In-memory reverse-lookup stub. Roughly half the candidates
/// resolve to a bound chord; the rest hit the
/// `unwrap_or_default()` empty-vec branch in the annotator.
/// The mix is closer to a real keymap (most commands don't
/// have a Normal-mode chord) than "every command bound."
struct StubLookup(HashMap<String, Vec<KeyChord>>);

impl KeymapReverseLookup for StubLookup {
    fn chords_for(&self, name: &str) -> Vec<KeyChord> {
        self.0.get(name).cloned().unwrap_or_default()
    }
}

fn make_stub_lookup() -> Arc<StubLookup> {
    let mut m = HashMap::new();
    // Roughly half the candidates resolve to a binding.
    // Single-chord (`<C-w>v`-style) is the dominant shape.
    for i in 0..BENCH_CANDIDATE_COUNT / 2 {
        m.insert(
            format!("ex:cmd-{i:04}"),
            vec![KeyChord::ctrl('w'), KeyChord::char('v')],
        );
    }
    Arc::new(StubLookup(m))
}

fn make_candidates(n: usize) -> Vec<RenderedCandidate> {
    (0..n)
        .map(|i| RenderedCandidate {
            raw: RawCandidate {
                text: format!("ex:cmd-{i:04}"),
                display: format!("ex:cmd-{i:04}"),
                kind: CandidateKind::Command,
                data: CandidateData::Command {
                    name: format!("ex:cmd-{i:04}"),
                    doc: format!("Doc line for command {i}.\nSecond line ignored."),
                    kind_label: "ex-command".into(),
                    source: SourceLocation::synthetic("bench"),
                },
                source: None,
                accept_action: None,
                annotations: Vec::new(),
            },
            score: MatchScore::PERFECT,
            match_ranges: Vec::new(),
            annotations: Vec::new(),
        })
        .collect()
}

fn bench_annotate_pipeline_1000_3stage(c: &mut Criterion) {
    // Full pipeline: kind + doc + keybinding annotators run
    // in order on every candidate. Matches what
    // `lattice_completion::populate` + the MARG.2 wiring in
    // editor_boot register as the default annotator stack.
    let kind = KindLabelAnnotator;
    let doc = DocSnippetAnnotator;
    let keyb = KeybindingAnnotator::new(make_stub_lookup());
    use lattice_completion::traits::CandidateAnnotator;
    c.bench_function("annotate_pipeline_1000_3stage", |b| {
        b.iter_batched(
            || make_candidates(BENCH_CANDIDATE_COUNT),
            |mut cands| {
                for c in cands.iter_mut() {
                    kind.annotate(c);
                    doc.annotate(c);
                    keyb.annotate(c);
                }
                black_box(cands)
            },
            criterion::BatchSize::SmallInput,
        )
    });
}

fn bench_keybinding_annotator_1000(c: &mut Criterion) {
    // Isolates the keybinding annotator's reverse-cache
    // lookup cost. Regression here points at the reverse-
    // cache shape (HashMap probe + Vec clone) rather than
    // pipeline overhead.
    let keyb = KeybindingAnnotator::new(make_stub_lookup());
    use lattice_completion::traits::CandidateAnnotator;
    c.bench_function("keybinding_annotator_1000", |b| {
        b.iter_batched(
            || make_candidates(BENCH_CANDIDATE_COUNT),
            |mut cands| {
                for c in cands.iter_mut() {
                    keyb.annotate(c);
                }
                black_box(cands)
            },
            criterion::BatchSize::SmallInput,
        )
    });
}

fn bench_annotation_display_text_per_variant(c: &mut Criterion) {
    // `display_text()` runs once per visible annotation on
    // every paint. Cheap by design — `Cow::Borrowed` for
    // string variants, structured format only for
    // `Keybinding`. Bench locks in the assumption so a
    // future variant that owns a String allocation surfaces
    // immediately.
    let kind = Annotation::Kind(Arc::from("(motion)"));
    let doc = Annotation::DocSnippet(Arc::from("Write the buffer."));
    let keyb = Annotation::Keybinding(vec![KeyChord::ctrl('w'), KeyChord::char('v')]);
    let source = Annotation::Source(Arc::from("builtin"));
    let custom = Annotation::Custom {
        text: Arc::from("[lsp]"),
        slot: Arc::from("annotation_lsp"),
    };

    let mut group = c.benchmark_group("annotation_display_text");
    group.bench_function("kind", |b| b.iter(|| black_box(kind.display_text())));
    group.bench_function("doc", |b| b.iter(|| black_box(doc.display_text())));
    group.bench_function("keybinding_2_chords", |b| {
        b.iter(|| black_box(keyb.display_text()))
    });
    group.bench_function("source", |b| b.iter(|| black_box(source.display_text())));
    group.bench_function("custom", |b| b.iter(|| black_box(custom.display_text())));
    group.finish();
}

criterion_group!(
    benches,
    bench_annotate_pipeline_1000_3stage,
    bench_keybinding_annotator_1000,
    bench_annotation_display_text_per_variant,
);
criterion_main!(benches);
