//! LG.3b — a wasm grammar, registered as a plugin language, is
//! indistinguishable from a bundled one all the way to styled spans.
//!
//! Design:
//! [`plugin-languages.md`](../../../docs/dev/architecture/plugin-languages.md) §2.5.
//!
//! LG.3a proved the registration pipeline is provenance-agnostic using a
//! *bundled* grammar registered at runtime. That deliberately did not
//! exercise wasm. This file closes the gap: the grammar is compiled from a
//! wasm side module, registered through the public API, selected by file
//! extension, and highlighted — and the resulting spans are compared
//! against the same grammar linked natively.
//!
//! Comparing against native is the point. "It produced some spans" would
//! pass with a subtly wrong parse; "it produced the same spans as the
//! bundled markdown grammar" cannot.
#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use lattice_syntax::{GrammarSpec, Lang, Syntax, plugin_lang, wasm_grammar};

/// The registry is process-wide and cargo runs tests in parallel, so each
/// test claims a name and extension no other test uses.
fn unique(tag: &str) -> (String, String, u64) {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    (
        format!("lg3b{tag}{n}"),
        format!("lg3bx{tag}{n}"),
        7_100_000 + n,
    )
}

/// Built by `scripts/build-wasm-grammar.sh`, the same artefact LG.1's
/// tests build. Absent means the prerequisites are missing — a skip, not
/// a failure.
fn markdown_wasm() -> Option<Vec<u8>> {
    let p = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wasm-grammars/tree-sitter-markdown.wasm"
    );
    std::fs::read(p).ok()
}

const SOURCE: &str = "# Title\n\nSome *emphasis* and `code` here.\n\n- one\n- two\n";

#[test]
fn a_wasm_grammar_registered_as_a_plugin_language_highlights_like_the_native_one() {
    let Some(bytes) = markdown_wasm() else {
        eprintln!(
            "SKIPPED — no wasm grammar. Build it with:\n  \
             scripts/build-wasm-grammar.sh markdown \
             \"$(ls -d ~/.cargo/registry/src/*/tree-sitter-md-*/tree-sitter-markdown)/src\""
        );
        return;
    };
    let (name, ext, plugin) = unique("e2e");

    // What LG.3c's loader will do, minus the WIT: compile the grammar
    // once, then register it like any other language.
    let grammar = wasm_grammar::load("markdown", &bytes).expect("wasm grammar loads");
    assert!(grammar.is_wasm());

    let spec = GrammarSpec {
        grammar,
        // The same query the bundled markdown language uses, so the
        // comparison below is against a real highlighter rather than a
        // toy one.
        highlights: Some(include_str!("../queries/markdown/highlights.scm").to_string()),
        folds: None,
        // The block grammar's own injections query, wiring `(inline)` into
        // `markdown_inline`. Worth having rather than `None` for two
        // reasons: without it the comparison below silently omits every
        // inline span (emphasis, inline code) and would pass while proving
        // much less — and the resolved child, `markdown_inline`, is a
        // BUNDLED language, so this exercises a wasm parent injecting into
        // a native child. That is exactly the shape org needs for
        // `#+BEGIN_SRC rust`.
        injections: Some(tree_sitter_md::INJECTION_QUERY_BLOCK.to_string()),
        indents: None,
        textobjects: None,
    };
    let interned =
        plugin_lang::register_with_grammar(&name, &[&ext], &spec, plugin).expect("registers");

    // Selected by extension, exactly as a bundled language is.
    let lang = Lang::detect_from_path(Some(&PathBuf::from(format!("notes.{ext}"))));
    assert_eq!(lang, Lang::Plugin(interned));

    let via_wasm = spans(lang);
    let native = spans(Lang::Markdown);

    assert!(
        !via_wasm.is_empty(),
        "the wasm-backed language produced no spans at all"
    );
    assert_eq!(
        via_wasm, native,
        "a wasm grammar must highlight identically to the same grammar linked natively"
    );

    plugin_lang::unregister_plugin(plugin);
}

/// Injections are the case with the sharp edge: a fresh `Parser` is built
/// **per injection, per highlight call**, so a wasm-backed grammar there
/// must borrow the pooled store rather than build one. This exercises that
/// path repeatedly — if the store were leaked into each parser instead of
/// returned, this would still pass but crawl, so the assertion is
/// correctness and `benchmarks.md` carries the cost.
#[test]
fn repeated_highlighting_of_a_wasm_language_stays_correct() {
    let Some(bytes) = markdown_wasm() else {
        eprintln!("SKIPPED — no wasm grammar built");
        return;
    };
    let (name, ext, plugin) = unique("repeat");

    let grammar = wasm_grammar::load("markdown", &bytes).expect("loads");
    let spec = GrammarSpec {
        grammar,
        highlights: Some(include_str!("../queries/markdown/highlights.scm").to_string()),
        folds: None,
        injections: Some(tree_sitter_md::INJECTION_QUERY_BLOCK.to_string()),
        indents: None,
        textobjects: None,
    };
    plugin_lang::register_with_grammar(&name, &[&ext], &spec, plugin).expect("registers");
    let lang = Lang::detect_from_path(Some(&PathBuf::from(format!("a.{ext}"))));

    let first = spans(lang);
    for i in 0..8 {
        assert_eq!(spans(lang), first, "highlight {i} diverged from the first");
    }

    plugin_lang::unregister_plugin(plugin);
}

/// Unload must withdraw the grammar too, not just the name — otherwise a
/// buffer open at unload keeps parsing through a language its plugin no
/// longer provides.
#[test]
fn unload_withdraws_a_wasm_backed_language() {
    let Some(bytes) = markdown_wasm() else {
        eprintln!("SKIPPED — no wasm grammar built");
        return;
    };
    let (name, ext, plugin) = unique("unload");

    let grammar = wasm_grammar::load("markdown", &bytes).expect("loads");
    let spec = GrammarSpec {
        grammar,
        highlights: Some("(atx_heading) @text.title.1".to_string()),
        folds: None,
        injections: None,
        indents: None,
        textobjects: None,
    };
    let interned =
        plugin_lang::register_with_grammar(&name, &[&ext], &spec, plugin).expect("registers");
    assert!(
        Syntax::for_language(Lang::Plugin(interned))
            .unwrap()
            .is_some()
    );

    plugin_lang::unregister_plugin(plugin);

    assert!(
        Syntax::for_language(Lang::Plugin(interned))
            .unwrap()
            .is_none(),
        "the wasm grammar must be withdrawn with the language"
    );
    assert_eq!(
        Lang::detect_from_path(Some(&PathBuf::from(format!("a.{ext}")))),
        Lang::Plain
    );
}

/// Styled spans for `SOURCE`, as (line, start, end, style) so the two
/// languages can be compared without depending on span ordering details.
/// `Style` is rendered via `Debug` because it is not `Ord`, and a stable
/// sort key is what makes the comparison order-independent.
fn spans(lang: Lang) -> Vec<(usize, usize, usize, String)> {
    let mut syntax = Syntax::for_language(lang)
        .expect("registry")
        .expect("language should have a grammar");
    syntax.parse(SOURCE);
    let line_count = SOURCE.split('\n').count() as u32;
    let lines = syntax.highlight_lines_native(0, line_count).expect("spans");
    let mut out: Vec<_> = lines
        .iter()
        .enumerate()
        .flat_map(|(i, l)| {
            l.iter()
                .map(move |s| (i, s.start, s.end, format!("{:?}", s.style)))
        })
        .collect();
    out.sort();
    out
}
