#![allow(clippy::unwrap_used, clippy::panic)]
//! LG.1 — what does a wasm-loaded grammar cost against the same grammar
//! linked natively?
//!
//! Design: [`plugin-languages.md`](../../../docs/dev/architecture/plugin-languages.md) §4.
//! Slice plan: `docs/dev/operations/slice-plans/plugin-languages.md`.
//!
//! The ecosystem quotes roughly 2–5× for wasm-vs-native tree-sitter
//! parsing. LG.1 exists to **measure it here rather than inherit it**,
//! because it is the number that decides whether plugin-contributed
//! languages ship at all or the design falls back to §6.
//!
//! ## What is isolated
//!
//! Both sides run **the same grammar** (`tree-sitter-md`) over the same
//! input: one via `tree_sitter_md::LANGUAGE` (the crate's natively
//! compiled parser), one via `WasmStore::load_language` on the same
//! `parser.c` + `scanner.c` built to wasm. So the comparison isolates
//! the *loading mechanism*, not the language — a wasm org grammar
//! against a native rust one would have measured the wrong thing.
//!
//! ## Cold vs incremental
//!
//! Both are timed, and they answer different questions. **Cold parse**
//! is what a user waits behind on file open, and shows up as
//! "highlighting catches up" — the UX contract permits it as eventual
//! consistency, but with a limit. **Incremental reparse** is the one on
//! a path a user waits behind repeatedly: it is what runs after an edit,
//! and its budget is a recorded number in `benchmarks.md`.
//!
//! ## Getting the wasm artefact
//!
//! ```sh
//! scripts/build-wasm-grammar.sh markdown \
//!     "$(ls -d ~/.cargo/registry/src/*/tree-sitter-md-*/tree-sitter-markdown)/src"
//! cargo bench -p lattice-syntax --bench wasm_vs_native_parse
//! ```
//!
//! The script needs only clang + rustup — no emscripten, no docker, no
//! tree-sitter CLI; see its header for why that works. If the artefact
//! is missing the bench **says so and skips** rather than silently
//! reporting native-only numbers, which would read as "wasm is free".

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

/// Markdown that exercises the parser broadly rather than one node
/// kind — headings, emphasis, lists, fenced code and blockquotes, so
/// the external scanner (block continuation, fence matching) is on the
/// measured path. A corpus of nothing but paragraphs would flatter both
/// sides equally but tell us less.
fn markdown_corpus(n_sections: usize) -> String {
    let mut s = String::with_capacity(n_sections * 320);
    s.push_str("# Top\n\n");
    for i in 0..n_sections {
        s.push_str(&format!(
            "## Section {i}\n\n\
             Some **bold** text and `code` for section {i}, plus a [link](https://example.com).\n\
             Another body line with _emphasis_ in it.\n\n\
             - item one\n- item two\n  - nested item\n\n\
             > a quoted line\n\n\
             ```rust\nfn handler_{i}() -> u32 {{ {i} }}\n```\n\n"
        ));
    }
    s
}

/// The wasm grammar, or `None` when it has not been built. Returned
/// rather than `expect`ed so a plain `cargo bench` on a fresh checkout
/// reports the missing step instead of failing opaquely.
fn wasm_grammar_bytes() -> Option<Vec<u8>> {
    let path = std::env::var("LATTICE_WASM_GRAMMAR").unwrap_or_else(|_| {
        format!(
            "{}/target/wasm-grammars/tree-sitter-markdown.wasm",
            env!("CARGO_MANIFEST_DIR").trim_end_matches("/crates/lattice-syntax")
        )
    });
    std::fs::read(&path).ok()
}

/// A one-line edit in the middle of the corpus, applied as tree-sitter
/// sees it: `Tree::edit` with the byte/point delta, then a reparse
/// passing the old tree. This is the shape the editor's reparse path
/// uses, so the number transfers.
fn edited(corpus: &str) -> (String, tree_sitter::InputEdit) {
    // Insert a word at the start of a body line roughly halfway down.
    let mid = corpus.len() / 2;
    let at = corpus[mid..]
        .find("Another body line")
        .map(|o| mid + o)
        .expect("corpus shape changed");
    let inserted = "very ";
    let mut out = String::with_capacity(corpus.len() + inserted.len());
    out.push_str(&corpus[..at]);
    out.push_str(inserted);
    out.push_str(&corpus[at..]);

    let row = corpus[..at].matches('\n').count();
    let col = at - corpus[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let edit = tree_sitter::InputEdit {
        start_byte: at,
        old_end_byte: at,
        new_end_byte: at + inserted.len(),
        start_position: tree_sitter::Point::new(row, col),
        old_end_position: tree_sitter::Point::new(row, col),
        new_end_position: tree_sitter::Point::new(row, col + inserted.len()),
    };
    (out, edit)
}

fn bench(c: &mut Criterion) {
    let Some(wasm) = wasm_grammar_bytes() else {
        eprintln!(
            "\nwasm_vs_native_parse: SKIPPED — no wasm grammar found.\n\
             Build it with:\n  scripts/build-wasm-grammar.sh markdown \
             \"$(ls -d ~/.cargo/registry/src/*/tree-sitter-md-*/tree-sitter-markdown)/src\"\n"
        );
        return;
    };

    let native: tree_sitter::Language = tree_sitter_md::LANGUAGE.into();

    // One store, one engine, held for the whole run: `WasmStore::new`
    // compiles tree-sitter's wasm libc, which is startup cost rather
    // than per-parse cost and would swamp the thing being measured.
    let engine = tree_sitter::wasmtime::Engine::default();
    let mut store = tree_sitter::WasmStore::new(&engine).expect("wasm store");
    let wasm_lang = store
        .load_language("markdown", &wasm)
        .expect("load wasm grammar");
    assert!(wasm_lang.is_wasm());

    let mut wasm_parser = tree_sitter::Parser::new();
    wasm_parser.set_wasm_store(store).unwrap();
    wasm_parser.set_language(&wasm_lang).unwrap();

    let mut native_parser = tree_sitter::Parser::new();
    native_parser.set_language(&native).unwrap();

    // Guard the comparison itself: if the two grammars disagree the
    // ratio is meaningless. Cheap, and it has already caught a
    // link-flag mistake once (see build-wasm-grammar.sh's --Bsymbolic).
    {
        let probe = markdown_corpus(4);
        let a = native_parser.parse(&probe, None).unwrap();
        let b = wasm_parser.parse(&probe, None).unwrap();
        assert_eq!(
            a.root_node().to_sexp(),
            b.root_node().to_sexp(),
            "wasm and native grammars produced different trees"
        );
        assert!(!b.root_node().has_error(), "wasm parse has errors");
    }

    for sections in [16usize, 128, 512] {
        let corpus = markdown_corpus(sections);
        let (after, edit) = edited(&corpus);

        let mut g = c.benchmark_group("parse_cold");
        g.throughput(Throughput::Bytes(corpus.len() as u64));
        g.bench_with_input(BenchmarkId::new("native", sections), &corpus, |b, src| {
            b.iter(|| black_box(native_parser.parse(black_box(src), None).unwrap()));
        });
        g.bench_with_input(BenchmarkId::new("wasm", sections), &corpus, |b, src| {
            b.iter(|| black_box(wasm_parser.parse(black_box(src), None).unwrap()));
        });
        g.finish();

        // Incremental. `Tree::edit` mutates, so each iteration needs its
        // own pre-edit tree or the second one would measure a reparse of
        // an already-reparsed tree. The clean tree is parsed ONCE here
        // and the per-iteration setup clones it — cloning keeps the
        // parser out of the setup closure (it is already mutably
        // borrowed by the timed routine) and is cheaper than reparsing.
        let native_base = native_parser.parse(&corpus, None).unwrap();
        let wasm_base = wasm_parser.parse(&corpus, None).unwrap();

        let mut g = c.benchmark_group("parse_incremental");
        g.throughput(Throughput::Bytes(corpus.len() as u64));
        g.bench_function(BenchmarkId::new("native", sections), |b| {
            b.iter_batched(
                || {
                    let mut t = native_base.clone();
                    t.edit(&edit);
                    t
                },
                |old| black_box(native_parser.parse(black_box(&after), Some(&old)).unwrap()),
                criterion::BatchSize::SmallInput,
            );
        });
        g.bench_function(BenchmarkId::new("wasm", sections), |b| {
            b.iter_batched(
                || {
                    let mut t = wasm_base.clone();
                    t.edit(&edit);
                    t
                },
                |old| black_box(wasm_parser.parse(black_box(&after), Some(&old)).unwrap()),
                criterion::BatchSize::SmallInput,
            );
        });
        g.finish();
    }

    // LG.3b: what a wasm-backed grammar costs OUTSIDE the parse itself.
    // A wasm `Language` can only be used by a `Parser` that owns a
    // `WasmStore`, and these three numbers are what decide where stores
    // come from — see `wasm_grammar.rs`.
    let mut g = c.benchmark_group("wasm_store");
    g.sample_size(10);
    g.bench_function("WasmStore::new", |b| {
        b.iter(|| {
            black_box(tree_sitter::WasmStore::new(black_box(&engine)).expect("store"));
        });
    });
    g.bench_function("load_language", |b| {
        b.iter(|| {
            let mut s = tree_sitter::WasmStore::new(&engine).expect("store");
            black_box(s.load_language("markdown", black_box(&wasm)).expect("load"));
        });
    });
    // The reason a grammar is loaded ONCE and the `Language` kept: binding
    // an already-compiled one into another store is three orders of
    // magnitude cheaper than compiling it again.
    g.bench_function("bind_existing_language", |b| {
        // The store is built in SETUP, not in the timed routine — it costs
        // 5 ms and would swamp the thing being measured, which is exactly
        // the mistake that makes a bench say nothing.
        b.iter_batched(
            || {
                let mut p = tree_sitter::Parser::new();
                p.set_wasm_store(tree_sitter::WasmStore::new(&engine).expect("store"))
                    .expect("attach");
                p
            },
            |mut p| p.set_language(black_box(&wasm_lang)).expect("bind"),
            criterion::BatchSize::SmallInput,
        );
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
