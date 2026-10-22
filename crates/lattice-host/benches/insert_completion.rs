#![allow(clippy::unwrap_used, clippy::panic)]
//! IC.3: the cost of re-ranking an open completion popup.
//!
//! `refilter_insert_completion` is the one match / rank / dedup path: the
//! popup's open, every keystroke while it is open, and every async answer
//! landing all run it over the popup's whole raw set. It runs on the actor
//! inside a keystroke, so it is on the keystroke→glyph path (paramount #1).
//!
//! Sizes: 200 (buffer words and symbols), 700 (plus a server's capped 500
//! items), 2000 (several servers at the cap). The bonus reads every source's
//! priority, the five builtins from options and any other from its
//! contribution — a lookup per candidate.
//!
//! IC.6 adds what an AUTOMATIC open costs. With `completion.auto_trigger`
//! on, a keystroke that does not open the popup adds a map lookup and an
//! option read; one that does pays a whole open — every sync source, buffer
//! words walking the document — which is what `open/<lines>` measures.
//!
//! Run:
//!
//!   cargo bench -p lattice-host --bench insert_completion

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use lattice_completion::{
    CandidateKind, CompletionTrigger, InsertCompletionState, RawCandidate, SourceId,
};
use lattice_core::Document;
use lattice_host::editor::Editor;
use lattice_protocol::position::Position;

const SOURCES: [&str; 4] = [
    "gen:lsp-completion",
    "gen:buffer-words",
    "gen:tree-sitter-symbol",
    "gen:some-plugin",
];

/// An open popup for `query` over `n` candidates spread across four
/// sources, a plugin's among them.
fn popup(n: usize, query: &str) -> InsertCompletionState {
    let mut state = InsertCompletionState::open(
        CompletionTrigger::Manual,
        Position::new(0, 0),
        Position::new(0, query.len() as u32),
        query.to_string(),
    );
    state.raw = (0..n)
        .map(|i| {
            RawCandidate::plain(format!("item_{i:04}_value"), CandidateKind::Plain)
                .with_source(SourceId::new(SOURCES[i % SOURCES.len()]))
        })
        .collect();
    state
}

fn refilter(c: &mut Criterion) {
    let editor = Editor::boot(Document::empty());
    let mut group = c.benchmark_group("insert_completion_refilter");
    for n in [200usize, 700, 2000] {
        // `it` matches every row; `item_01` a twentieth of 2000.
        for query in ["it", "item_01"] {
            group.bench_with_input(BenchmarkId::new(format!("q={query}"), n), &n, |b, &n| {
                b.iter_batched(
                    || popup(n, query),
                    |mut state| {
                        editor.refilter_insert_completion(&mut state);
                        black_box(state.rendered.len())
                    },
                    criterion::BatchSize::SmallInput,
                );
            });
        }
    }
    group.finish();
}

/// A document of `lines` lines of distinct words, in Insert mode, the
/// cursor after `al` on a fresh last line — where an automatic open fires.
fn editor_typing_al(lines: usize) -> Editor {
    let mut text = String::new();
    for i in 0..lines {
        text.push_str(&format!("alpha_{i} beta_{i} gamma_{i} delta_{i}\n"));
    }
    text.push_str("al");
    let mut editor = Editor::boot(Document::from_text(text));
    // What a peer does once at start: activate the document's modes
    // (completion's among them), then let the minor cascade run.
    let _ = editor.activate_buffer_state();
    let _ = editor.run_tick_pending();
    editor.modal = lattice_grammar::ModalState::Insert;
    editor.cursor = Position::new(lines as u32, 2);
    editor
}

fn open(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert_completion_open");
    for lines in [100usize, 1_000, 10_000] {
        let mut editor = editor_typing_al(lines);
        editor.do_completion_trigger();
        assert!(
            editor.insert_completion.is_some(),
            "precondition: the popup opens over {lines} lines"
        );
        group.bench_with_input(BenchmarkId::from_parameter(lines), &lines, |b, _| {
            b.iter(|| {
                editor.insert_completion = None;
                editor.do_completion_trigger();
                black_box(editor.insert_completion.as_ref().map(|s| s.rendered.len()))
            });
        });
    }
    group.finish();
}

criterion_group!(benches, refilter, open);
criterion_main!(benches);
