//! PH7.7a grammar-extension boundary-marshalling microbench.
//!
//! Measures the per-call marshalling cost of the grammar seam's boundary
//! conversions — the *host-side* half of the < 5µs p99 grammar round-trip
//! budget (design §5.5.2). Each bench isolates one direction of one seam:
//!   - `project_motion_context` — the host→guest projection a plugin motion's
//!     trampoline runs on every dispatch (the hot path).
//!   - `motion_result_round_trip` — the guest→host result the trampoline maps
//!     back to native.
//!   - `project_operator_context` + `effect_from_wit` — the operator seam
//!     (context in, `effect` out).
//!   - `project_text_object_context` — the text-object seam (range out reuses
//!     the PH7.3b `NativeRange::from_wit`, benched in `boundary.rs`).
//!
//! This is the *marshalling* component only; the end-to-end guest↔host
//! trampoline (wasmtime canonical-ABI lift/lower + the sync guest call) lands
//! and is gated with PH7.7c/d, where an actual guest call exists to measure.
//! Not a ratcheted CI budget here (that is the PH7.7d gate row); this exists so
//! the grammar boundary surface is measured from day one (four-artefact
//! discipline).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use lattice_core::Document;
use lattice_core::buffer::Buffer;
use lattice_core::buffers::BufferId;
use lattice_grammar::CancellationToken;
use lattice_grammar::args::{ArgValue, Args};
use lattice_grammar::command::Count;
use lattice_grammar::effect::Effect;
use lattice_grammar::register::Register;
use lattice_grammar::registry::{MotionContext, MotionResult, OperatorContext, TextObjectContext};
use lattice_plugin_host::WitBoundary;
use lattice_plugin_host::boundary_grammar::{
    project_motion_context, project_operator_context, project_text_object_context,
};
use lattice_protocol::position::{Position, Range};

fn pos(line: u32, byte: u32) -> Position {
    Position { line, byte }
}

fn grammar_marshalling(c: &mut Criterion) {
    let buffer = Buffer::from_text("hello world\nsecond line of text\nthird\n");
    let cancel = CancellationToken::never();
    let args = Args::List(vec![ArgValue::String("w".into()), ArgValue::Int(3)]);

    let motion_ctx = MotionContext {
        buffer: &buffer,
        buffer_id: BufferId(7),
        from: pos(1, 4),
        count: Count(3),
        has_explicit_count: true,
        args: args.clone(),
        cancel: &cancel,
        scope_resolver: None,
    };
    c.bench_function("grammar_project_motion_context", |b| {
        b.iter(|| {
            let wit = project_motion_context(black_box(&motion_ctx)).expect("project");
            black_box(wit);
        })
    });

    let result = MotionResult {
        target: pos(2, 0),
        linewise: true,
    };
    c.bench_function("grammar_motion_result_round_trip", |b| {
        b.iter(|| {
            let wit = black_box(&result).to_wit().expect("to_wit");
            let back = MotionResult::from_wit(wit).expect("from_wit");
            black_box(back);
        })
    });

    let text_object_ctx = TextObjectContext {
        buffer: &buffer,
        at: pos(1, 6),
        count: Count(1),
        args: Args::None,
        cancel: &cancel,
        scope_resolver: None,
        comment_syntax: None,
    };
    c.bench_function("grammar_project_text_object_context", |b| {
        b.iter(|| {
            let wit = project_text_object_context(black_box(&text_object_ctx)).expect("project");
            black_box(wit);
        })
    });

    let mut document = Document::from_text("hello world\nsecond line of text\n");
    let operator_ctx = OperatorContext {
        document: &mut document,
        range: Range {
            start: pos(0, 0),
            end: pos(1, 5),
        },
        linewise: false,
        register: Register::Named('a'),
        count: Count(1),
        args,
        cancel: &cancel,
        indent: Default::default(),
    };
    c.bench_function("grammar_project_operator_context", |b| {
        b.iter(|| {
            let wit = project_operator_context(black_box(&operator_ctx)).expect("project");
            black_box(wit);
        })
    });

    // The operator/ex-command guest→host result is an `effect`; bench the
    // `from_wit` the trampoline runs to map it back to native.
    let effect_wit = Effect::SetColorscheme("nord".into())
        .to_wit()
        .expect("to_wit");
    c.bench_function("grammar_effect_from_wit", |b| {
        b.iter(|| {
            let back = Effect::from_wit(black_box(effect_wit.clone())).expect("from_wit");
            black_box(back);
        })
    });
}

criterion_group!(benches, grammar_marshalling);
criterion_main!(benches);
