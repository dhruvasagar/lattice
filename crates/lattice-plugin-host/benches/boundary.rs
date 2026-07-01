//! PH7.3a boundary-conversion microbench.
//!
//! Measures the per-value marshalling cost of the `WitBoundary` adapter —
//! `to_wit` then `from_wit` — for the representative boundary types (`Args`,
//! `RawCandidate`, `PickerAcceptOutcome`). This is the *marshalling* component
//! of the §7 "typed host function call" budget (< 100ns p50 / < 500ns p99);
//! the **end-to-end guest↔host typed-call** gate — which also includes the
//! wasmtime canonical-ABI lift/lower + the async suspend — lands with the
//! call machinery at PH7.3d, where there is an actual call to measure.
//!
//! Not a ratcheted CI budget yet (that is PH7.5); this exists so the boundary
//! marshalling surface is measured from day one (four-artefact discipline).

use std::hint::black_box;
use std::path::PathBuf;

use criterion::{Criterion, criterion_group, criterion_main};
use lattice_completion::candidate::{CandidateData, CandidateKind, RawCandidate};
use lattice_grammar::args::{ArgValue, Args};
use lattice_picker::outcome::PickerAcceptOutcome;
use lattice_plugin_host::WitBoundary;

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
}

criterion_group!(benches, boundary_round_trip);
criterion_main!(benches);
