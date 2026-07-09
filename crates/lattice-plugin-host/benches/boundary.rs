//! PH7.3a boundary-conversion microbench.
//!
//! Measures the per-value marshalling cost of the `WitBoundary` adapter —
//! `to_wit` then `from_wit` — for the representative boundary types (`Args`,
//! `RawCandidate`, `PickerAcceptOutcome`, `Effect`). This is the *marshalling*
//! component
//! of the §7 "typed host function call" budget (< 100ns p50 / < 500ns p99);
//! the **end-to-end guest↔host typed-call** gate — which also includes the
//! wasmtime canonical-ABI lift/lower + the async suspend — lands with the
//! call machinery at PH7.3d, where there is an actual call to measure.
//!
//! Not a ratcheted CI budget yet (that is PH7.5); this exists so the boundary
//! marshalling surface is measured from day one (four-artefact discipline).

use std::hint::black_box;
use std::path::PathBuf;

use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use lattice_completion::candidate::{
    Annotation, AnnotationSegment, CandidateData, CandidateKind, RawCandidate,
};
use lattice_core::buffer::Buffer;
use lattice_grammar::app_effect::AppEffect;
use lattice_grammar::args::{ArgValue, Args};
use lattice_grammar::effect::{Effect, QuitScope};
use lattice_picker::RoutingPayload;
use lattice_picker::outcome::PickerAcceptOutcome;
use lattice_plugin_host::WitBoundary;
use lattice_plugin_host::buffer::DocumentResource;
use lattice_protocol::position::{Position, Range};
use lattice_runtime::snapshot::DocumentSnapshot;

fn boundary_round_trip(c: &mut Criterion) {
    let args = Args::List(vec![
        ArgValue::String("pattern".into()),
        ArgValue::Int(42),
        ArgValue::Bool(true),
        ArgValue::Chord("<C-c>".into()),
    ]);
    c.bench_function("boundary_args_round_trip", |b| {
        b.iter(|| {
            let wit = black_box(&args).to_wit().expect("to_wit");
            let back = Args::from_wit(wit).expect("from_wit");
            black_box(back);
        })
    });

    let candidate = RawCandidate {
        text: "/home/alice/project/src/main.rs".into(),
        display: "main.rs".into(),
        source: None,
        kind: CandidateKind::File,
        data: CandidateData::File {
            path: PathBuf::from("/home/alice/project/src/main.rs"),
            is_dir: false,
            size: Some(4096),
        },
        accept_action: None,
        annotations: Vec::new(),
        display_spans: Vec::new(),
    };
    c.bench_function("boundary_raw_candidate_round_trip", |b| {
        b.iter(|| {
            let wit = black_box(&candidate).to_wit().expect("to_wit");
            let back = RawCandidate::from_wit(wit).expect("from_wit");
            black_box(back);
        })
    });

    let outcome = PickerAcceptOutcome::JumpToLocation {
        path: PathBuf::from("/home/alice/project/src/main.rs"),
        line: 120,
        col: 8,
    };
    c.bench_function("boundary_picker_outcome_round_trip", |b| {
        b.iter(|| {
            let wit = black_box(&outcome).to_wit().expect("to_wit");
            let back = PickerAcceptOutcome::from_wit(wit).expect("from_wit");
            black_box(back);
        })
    });

    // A representative composite `Effect::Many` — exercises the `list<effect>`
    // flatten/rebuild plus a spread of payload arms (paths, options, a nested
    // scope enum). This is the marshalling cost an operator/ex-command guest
    // export pays to return an effect (PH7.3b1b).
    let effect = Effect::Many(vec![
        Effect::RecordJump,
        Effect::OpenBufferAt {
            path: Some(PathBuf::from("/home/alice/project/src/main.rs")),
            position: lattice_protocol::position::Position { line: 120, byte: 8 },
            force: false,
        },
        Effect::QuitEditor {
            force: false,
            scope: QuitScope::Pane,
        },
        Effect::Echo {
            level: lattice_grammar::effect::EchoLevel::Info,
            text: "saved".into(),
        },
    ]);
    c.bench_function("boundary_effect_round_trip", |b| {
        b.iter(|| {
            let wit = black_box(&effect).to_wit().expect("to_wit");
            let back = Effect::from_wit(wit).expect("from_wit");
            black_box(back);
        })
    });

    // The AppEffect mirror (PH7.3b2) — the payload of the Effect::AppAction arm.
    // A payload-bearing variant (mode-state) exercises the reused ModalState
    // mirror + the app-effect variant marshalling a plugin's chord-bound action
    // would pay.
    let app_effect = AppEffect::EnterVisual(lattice_grammar::modal::VisualKind::Linewise);
    c.bench_function("boundary_app_effect_round_trip", |b| {
        b.iter(|| {
            let wit = black_box(&app_effect).to_wit().expect("to_wit");
            let back = AppEffect::from_wit(wit).expect("from_wit");
            black_box(back);
        })
    });

    // PH7.3c: the `document` resource's `get-text-range` slices only the
    // requested span out of the rope — the "zero-copy at the slice level"
    // claim (§9.6). Backing a 10k-line buffer and reading one line shows the
    // cost is O(slice), not O(document): the whole rope is never materialised.
    let big = (0..10_000)
        .map(|i| format!("line {i} with some representative content"))
        .collect::<Vec<_>>()
        .join("\n");
    let doc = DocumentResource::new(Arc::new(DocumentSnapshot {
        buffer: Buffer::from_text(&big),
        ..Default::default()
    }));
    let one_line = Range {
        start: Position {
            line: 5_000,
            byte: 0,
        },
        end: Position {
            line: 5_000,
            byte: 12,
        },
    };
    c.bench_function("document_get_text_range_one_line", |b| {
        b.iter(|| {
            let text = black_box(&doc)
                .get_text_range(black_box(one_line))
                .expect("slice");
            black_box(text);
        })
    });

    // PH7.4a: a plugin picker candidate with a marginalia column — the shape a
    // WASM file source emits per row. A `RawCandidate` carrying a `Styled`
    // permission cell (per-bit-class segments) round-trips through the boundary;
    // this measures the marshalling cost of the marginalia the picker seam adds
    // (the whole `Annotation` enum crosses so plugin sources define columns).
    let mut cand = RawCandidate::plain("src/main.rs".to_string(), CandidateKind::File);
    cand.annotations = vec![
        Annotation::Styled {
            category: Arc::from("perms"),
            segments: vec![
                AnnotationSegment {
                    text: Arc::from("-"),
                    slot: Arc::from("perm_type"),
                },
                AnnotationSegment {
                    text: Arc::from("rw-r--r--"),
                    slot: Arc::from("perm_bits"),
                },
            ],
        },
        Annotation::Custom {
            text: Arc::from("42K"),
            slot: Arc::from("annotation_size"),
        },
    ];
    c.bench_function("boundary_picker_candidate_with_marginalia_round_trip", |b| {
        b.iter(|| {
            let wit = black_box(&cand).to_wit().expect("to_wit");
            let back = RawCandidate::from_wit(wit).expect("from_wit");
            black_box(back);
        })
    });

    // PH7.4a: the per-candidate `RoutingPayload` a file source emits (the token
    // it consumes in `accept`). `OpenFile` is the fuzzy-finder's arm.
    let routing = RoutingPayload::OpenFile {
        path: PathBuf::from("/home/user/project/src/main.rs"),
    };
    c.bench_function("boundary_routing_payload_round_trip", |b| {
        b.iter(|| {
            let wit = black_box(&routing).to_wit().expect("to_wit");
            let back = RoutingPayload::from_wit(wit).expect("from_wit");
            black_box(back);
        })
    });
}

criterion_group!(benches, boundary_round_trip);
criterion_main!(benches);
