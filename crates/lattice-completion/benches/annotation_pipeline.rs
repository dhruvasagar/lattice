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
    Annotation, AnnotationColumns, AnnotationSegment, CandidateData, CandidateKind,
    DocSnippetAnnotator, KeybindingAnnotator, KeymapReverseLookup, KindLabelAnnotator, MatchScore,
    RawCandidate, RenderedCandidate,
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
                display_spans: Vec::new(),
            },
            score: MatchScore::PERFECT,
            match_ranges: Vec::new(),
            annotations: Vec::new(),
        })
        .collect()
}

/// MARG §8: file-picker-shaped candidates — each carries a 10-segment
/// `Styled` permission cell plus single-segment size + mtime cells, the
/// exact set `metadata_annotations` produces. Used to bench the
/// per-frame `AnnotationColumns` layout + `display_text` concat cost the
/// renderer pays when a styled marginalia column is visible.
fn seg(ch: char, slot: &'static str) -> AnnotationSegment {
    AnnotationSegment {
        text: Arc::from(ch.to_string()),
        slot: Arc::from(slot),
    }
}

fn make_styled_file_candidates(n: usize) -> Vec<RenderedCandidate> {
    let perm = || Annotation::Styled {
        category: Arc::from("perm"),
        segments: vec![
            seg('d', "completion.annotation.perm.type"),
            seg('r', "completion.annotation.perm.read"),
            seg('w', "completion.annotation.perm.write"),
            seg('x', "completion.annotation.perm.exec"),
            seg('r', "completion.annotation.perm.read"),
            seg('-', "completion.annotation.perm.none"),
            seg('x', "completion.annotation.perm.exec"),
            seg('r', "completion.annotation.perm.read"),
            seg('-', "completion.annotation.perm.none"),
            seg('x', "completion.annotation.perm.exec"),
        ],
    };
    (0..n)
        .map(|i| {
            let mut raw = RawCandidate::plain(format!("src/module-{i:04}.rs"), CandidateKind::File);
            raw.annotations = vec![
                perm(),
                Annotation::Styled {
                    category: Arc::from("size"),
                    segments: vec![AnnotationSegment {
                        text: Arc::from("1.2K"),
                        slot: Arc::from("completion.annotation.size"),
                    }],
                },
                Annotation::Styled {
                    category: Arc::from("mtime"),
                    segments: vec![AnnotationSegment {
                        text: Arc::from("3 days ago"),
                        slot: Arc::from("completion.annotation.mtime"),
                    }],
                },
            ];
            let annotations = raw.annotations.clone();
            RenderedCandidate {
                raw,
                score: MatchScore::PERFECT,
                match_ranges: Vec::new(),
                annotations,
            }
        })
        .collect()
}

/// MARG §9: picker-rollout-shaped candidates — each carries a 5-segment
/// `location` cell (path:line:col) plus a 2-segment `status` cell and a
/// single-segment `latency` cell, the families MP.2–MP.4 emit across the
/// non-file pickers. Used to lock the §9.5 "same O(visible × segments),
/// no measurable cost" claim for the rollout families.
fn tseg(text: &str, slot: &'static str) -> AnnotationSegment {
    AnnotationSegment {
        text: Arc::from(text),
        slot: Arc::from(slot),
    }
}

fn make_styled_picker_candidates(n: usize) -> Vec<RenderedCandidate> {
    (0..n)
        .map(|i| {
            let mut raw = RawCandidate::plain(format!("let x = {i};"), CandidateKind::Plain);
            raw.annotations = vec![
                Annotation::Styled {
                    category: Arc::from("location"),
                    segments: vec![
                        tseg("src/module.rs", "completion.annotation.location.path"),
                        tseg(":", "completion.annotation.location.path"),
                        tseg("42", "completion.annotation.location.line"),
                        tseg(":", "completion.annotation.location.col"),
                        tseg("7", "completion.annotation.location.col"),
                    ],
                },
                Annotation::Styled {
                    category: Arc::from("status"),
                    segments: vec![
                        tseg("•", "completion.annotation.status.active"),
                        tseg("+", "completion.annotation.status.dirty"),
                    ],
                },
                Annotation::Styled {
                    category: Arc::from("latency"),
                    segments: vec![tseg("[display]", "completion.annotation.latency.display")],
                },
            ];
            let annotations = raw.annotations.clone();
            RenderedCandidate {
                raw,
                score: MatchScore::PERFECT,
                match_ranges: Vec::new(),
                annotations,
            }
        })
        .collect()
}

fn bench_styled_picker_columns_1000(c: &mut Criterion) {
    // §9.5: per-frame `AnnotationColumns` layout cost for the rollout
    // families (location + status + latency) over 1000 rows (~30× a full
    // picker page). Sibling to `styled_marginalia_columns_1000` (file
    // metadata); locks the rollout's no-measurable-cost claim.
    let cands = make_styled_picker_candidates(BENCH_CANDIDATE_COUNT);
    c.bench_function("styled_picker_columns_1000", |b| {
        b.iter(|| black_box(AnnotationColumns::from_visible(black_box(cands.iter()))))
    });
}

fn bench_styled_marginalia_columns_1000(c: &mut Criterion) {
    // Per-frame cost of laying out a styled marginalia column over the
    // visible candidate set: `AnnotationColumns::from_visible` walks
    // every candidate's annotations and `display_text()`s each cell to
    // size the columns. Locks the §8.7 "O(visible × segments), no
    // measurable cost" claim for the file/dir picker's perm/size/mtime
    // marginalia. 1000 rows is the worst case (full picker page is
    // ~30 visible; this is 30×).
    let cands = make_styled_file_candidates(BENCH_CANDIDATE_COUNT);
    c.bench_function("styled_marginalia_columns_1000", |b| {
        b.iter(|| black_box(AnnotationColumns::from_visible(black_box(cands.iter()))))
    });
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
    let kind = Annotation::Kind(Arc::from("→"));
    let doc = Annotation::DocSnippet(Arc::from("Write the buffer."));
    let keyb = Annotation::Keybinding(vec![KeyChord::ctrl('w'), KeyChord::char('v')]);
    let source = Annotation::Source(Arc::from("builtin"));
    let custom = Annotation::Custom {
        text: Arc::from("[lsp]"),
        slot: Arc::from("annotation_lsp"),
    };
    // MARG §8: a 10-segment permission cell — the multi-segment
    // `display_text()` concat (the only owning case besides Keybinding).
    let styled_perm = Annotation::Styled {
        category: Arc::from("perm"),
        segments: vec![
            seg('d', "completion.annotation.perm.type"),
            seg('r', "completion.annotation.perm.read"),
            seg('w', "completion.annotation.perm.write"),
            seg('x', "completion.annotation.perm.exec"),
            seg('r', "completion.annotation.perm.read"),
            seg('-', "completion.annotation.perm.none"),
            seg('x', "completion.annotation.perm.exec"),
            seg('r', "completion.annotation.perm.read"),
            seg('-', "completion.annotation.perm.none"),
            seg('x', "completion.annotation.perm.exec"),
        ],
    };

    let mut group = c.benchmark_group("annotation_display_text");
    group.bench_function("kind", |b| b.iter(|| black_box(kind.display_text())));
    group.bench_function("doc", |b| b.iter(|| black_box(doc.display_text())));
    group.bench_function("keybinding_2_chords", |b| {
        b.iter(|| black_box(keyb.display_text()))
    });
    group.bench_function("source", |b| b.iter(|| black_box(source.display_text())));
    group.bench_function("custom", |b| b.iter(|| black_box(custom.display_text())));
    group.bench_function("styled_perm_10seg", |b| {
        b.iter(|| black_box(styled_perm.display_text()))
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_annotate_pipeline_1000_3stage,
    bench_keybinding_annotator_1000,
    bench_styled_marginalia_columns_1000,
    bench_styled_picker_columns_1000,
    bench_annotation_display_text_per_variant,
);
criterion_main!(benches);
