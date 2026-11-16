#![allow(clippy::unwrap_used, clippy::panic)]
//! RF.2f — the `refactor` plugin's edits on a large file, end to end.
//!
//! Each case is the whole cost one key pays: the sync trampoline projects
//! the context, the guest runs (its tree walks and queries cross back into
//! the host), and its effect comes home. The file is 2000 functions; the
//! cursor is in the middle one. The claim under test is that every edit is
//! bounded by the code around the cursor, not by the file: the plugin asks
//! for nodes and for query matches over a range, never for the whole tree.
//!
//! Extract and inline are timed through their native fallback actions
//! (`refactor-extract-native` / `-inline-native`), which take the exact
//! range a `cre` / `cri` request carries. The operators themselves first
//! hand the server a request, which costs nothing here; the native half is
//! the work. Exchange (`cx`) is text only, two keys apart, and has no tree
//! to be slow in, so it is not here.
//!
//! Skips when the guest was not built (no `wasm32-wasip2`; see build.rs).

use std::any::Any;
use std::hint::black_box;
use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use lattice_core::buffers::BufferId;
use lattice_grammar::CancellationToken;
use lattice_grammar::args::Args;
use lattice_grammar::command::CommandInvocation;
use lattice_grammar::effect::Effect;
use lattice_grammar::registry::{CommandRegistry, GrammarEnv};
use lattice_mode::CapabilitySet;
use lattice_plugin_host::{PluginHost, PluginManifest, TrustTier};
use lattice_protocol::position::Position;
use lattice_syntax::{Lang, Syntax};

const FNS: u32 = 2000;
const LINES_PER_FN: u32 = 5;

/// `FNS` functions, each a declaration, a call over it and a return:
///
/// ```text
/// fn f7(a: u32, b: u32) -> u32 {
///     let q = a + 1;
///     let r = g(a, b, q * 2);
///     r
/// }
/// ```
fn corpus() -> String {
    (0..FNS)
        .map(|i| {
            format!(
                "fn f{i}(a: u32, b: u32) -> u32 {{\n    let q = a + 1;\n    let r = g(a, b, q * 2);\n    r\n}}\n"
            )
        })
        .collect()
}

/// The middle function's `let q` line and its call line.
fn mid() -> (u32, u32) {
    let first = (FNS / 2) * LINES_PER_FN;
    (first + 1, first + 2)
}

fn span_bytes(line: u32, start: u32, end: u32) -> Vec<u8> {
    [line, start, line, end]
        .iter()
        .flat_map(|n| n.to_le_bytes())
        .collect()
}

struct Case {
    name: &'static str,
    command: &'static str,
    cursor: Position,
    args: Args,
}

fn cases() -> Vec<Case> {
    let (decl, call) = mid();
    // `    let r = g(a, b, q * 2);`: `a` at 14, `b` at 17, `q * 2` at 20..25.
    vec![
        Case {
            name: "expand_from_an_argument",
            command: "refactor-expand",
            cursor: Position::new(call, 17),
            args: Args::None,
        },
        Case {
            name: "swap_next_argument",
            command: "refactor-swap-next",
            cursor: Position::new(call, 14),
            args: Args::None,
        },
        Case {
            name: "raise_out_of_a_binary",
            command: "refactor-raise",
            cursor: Position::new(call, 20),
            args: Args::None,
        },
        Case {
            name: "split_call_arguments",
            command: "refactor-split",
            cursor: Position::new(call, 17),
            args: Args::None,
        },
        Case {
            name: "extract_native",
            command: "refactor-extract-native",
            cursor: Position::new(call, 20),
            args: Args::Bytes(span_bytes(call, 20, 25)),
        },
        Case {
            name: "inline_native",
            command: "refactor-inline-native",
            cursor: Position::new(decl, 8),
            args: Args::Bytes(span_bytes(decl, 8, 9)),
        },
    ]
}

fn refactor(c: &mut Criterion) {
    let path = env!("REFACTOR_PLUGIN_WASM");
    if path.is_empty() {
        eprintln!("SKIP: refactor bench — plugin not built (add wasm32-wasip2)");
        return;
    }
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();
    let manifest = PluginManifest::new("refactor", Vec::new(), CapabilitySet::TREE_SITTER);
    let set = host
        .instantiate_grammar_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            &Arc::new(lattice_runtime::EventBus::new()),
            None,
            None,
        )
        .unwrap();
    // The engine and its epoch ticker must outlive the dispatched closures.
    Box::leak(Box::new(host));
    let mut registry = CommandRegistry::new();
    set.register_all(&mut registry);

    let src = corpus();
    let mut syntax = Syntax::for_language(Lang::Rust).unwrap().unwrap();
    syntax.parse(&src);
    let snapshot: Arc<dyn Any + Send + Sync> = Arc::new(syntax.snapshot_owned());
    let cancel = CancellationToken::never();

    let mut group = c.benchmark_group("refactor_2000_fns");
    for case in cases() {
        let id = registry
            .id_by_name(case.command)
            .unwrap_or_else(|| panic!("{} is registered", case.command));
        let invocation = CommandInvocation::of(id).with_args(case.args);
        // The document is only read: every case answers with an effect the
        // host would apply, so one copy serves every iteration.
        let mut document = lattice_core::Document::from_text(&src);
        let run = |document: &mut lattice_core::Document| {
            lattice_grammar::execute_with_env(
                &registry,
                document,
                BufferId(1),
                case.cursor,
                invocation.clone(),
                &cancel,
                GrammarEnv {
                    syntax: Some(&snapshot),
                    ..Default::default()
                },
            )
            .unwrap_or_else(|e| panic!("{} dispatches: {e:?}", case.name))
        };
        // A refusal is cheap and would time the wrong thing.
        if let Effect::Echo { text, .. } = run(&mut document) {
            panic!("{} refused: {text}", case.name);
        }
        group.bench_function(case.name, |b| {
            b.iter(|| black_box(run(black_box(&mut document))))
        });
    }
    group.finish();
}

criterion_group!(benches, refactor);
criterion_main!(benches);
