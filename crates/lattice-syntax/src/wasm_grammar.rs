//! Loading grammars from WebAssembly, and the stores parsers need to run
//! them.
//!
//! Design:
//! [`plugin-languages.md`](../../../docs/dev/architecture/plugin-languages.md) §2.5.
//! Slice plan: LG.3b.
//!
//! [`load`] turns a wasm side module into an ordinary
//! [`tree_sitter::Language`], which is the fact the whole design rests on:
//! downstream, `HighlightConfiguration`, folds, injections, indents and
//! the incremental reparse path cannot tell where a grammar came from.
//!
//! ## The one thing that is not transparent
//!
//! A wasm-backed `Language` can only be used by a `Parser` that owns a
//! [`WasmStore`]. A parser without one fails `set_language` outright
//! ("Failed to load the Wasm store"), so this is not something that can be
//! left to chance. The costs, measured (see `benchmarks.md`):
//!
//! | operation | cost |
//! |---|---|
//! | `WasmStore::new` | **5.1 ms** — it compiles tree-sitter's wasm libc |
//! | `load_language` | **102 ms** — Cranelift compiling the grammar |
//! | binding an already-loaded `Language` into another store | **68 µs** |
//!
//! Three properties fall out of that table, and each is load-bearing:
//!
//! 1. **`load_language` is not cached by the `Engine`.** Loading the same
//!    bytes into a second store pays the full 102 ms again. So a grammar
//!    is loaded exactly once, at registration, and the `Language` is kept.
//! 2. **A `Language` outlives the store it was loaded from**, and is
//!    portable into any other store for 68 µs — three orders of magnitude
//!    cheaper than recompiling it. So [`load`] drops its loading store
//!    immediately; nothing has to keep it alive.
//! 3. **A `Tree` survives its parser's store being taken back**, which is
//!    what makes the pooled store below safe.
//!
//! And one constraint that is easy to violate and fails opaquely: **every
//! store must share the engine the grammar was compiled with.** A `Language`
//! from engine A cannot be instantiated in a store from engine B —
//! `WasmStore::new` returns a bare `Wasm` error with no explanation. That is
//! why [`engine`] is a process-wide `OnceLock` rather than something callers
//! pass in: there is exactly one engine, so the mistake is unavailable.
//!
//! ## Two strategies, because two call shapes
//!
//! [`set_language`] gives a parser its **own** store and leaves it there.
//! That is right for [`crate::Syntax`]'s long-lived parser: 5 ms once per
//! buffer whose language is wasm-backed, off the keystroke path, and the
//! store must stay because the parser needs it for every later reparse.
//!
//! [`with_pooled_store`] **lends** a thread-local store for the duration of
//! one parse and takes it back. That is right for injection highlighting,
//! which builds a fresh `Parser` **per injection, per highlight call** — a
//! markdown file with twenty fenced blocks would otherwise pay 20 × 5 ms
//! on every highlight. Property 3 above is why taking the store back after
//! the parse is sound.
//!
//! Native grammars touch none of this: both entry points check
//! `Language::is_wasm` first and do nothing when it is false.

use std::cell::RefCell;
use std::sync::OnceLock;

use tree_sitter::{Language, Parser, WasmStore, wasmtime::Engine};

/// The engine every grammar store shares.
///
/// tree-sitter's own wasmtime (36), not the plugin host's (46) — they are
/// different crates and this must be the former's type. LG.0 proved the
/// two runtimes coexist, including under a forced guest trap in both
/// initialisation orders.
///
/// Created on first use, which means a session that loads no language
/// plugin never creates an engine and therefore never installs the second
/// runtime's signal handlers.
fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(Engine::default)
}

/// Compile a grammar from a wasm side module.
///
/// **~102 ms.** Call once, at registration, on an off-thread task — never
/// on the keystroke or frame path. The returned `Language` is what gets
/// kept; the store used to load it is dropped here, which is sound
/// because the `Language` owns the compiled module.
///
/// `name` must be the grammar's tree-sitter name — the suffix of its
/// `tree_sitter_<name>` export — or the module will load but expose no
/// entry point.
pub fn load(name: &str, bytes: &[u8]) -> Result<Language, String> {
    let mut store =
        WasmStore::new(engine()).map_err(|e| format!("wasm store for '{name}': {e}"))?;
    store
        .load_language(name, bytes)
        .map_err(|e| format!("load wasm grammar '{name}': {e}"))
}

/// Bind `lang` into `parser`, giving the parser its own store first when
/// the grammar is wasm-backed.
///
/// The store stays in the parser for its lifetime, because the parser
/// needs it for every subsequent parse — this is not a lending API.
pub(crate) fn set_language(parser: &mut Parser, lang: &Language) -> Result<(), String> {
    if lang.is_wasm() {
        let store = WasmStore::new(engine()).map_err(|e| format!("wasm store: {e}"))?;
        parser
            .set_wasm_store(store)
            .map_err(|e| format!("attach wasm store: {e}"))?;
    }
    parser.set_language(lang).map_err(|e| e.to_string())
}

thread_local! {
    /// One store per thread, created on first use by a wasm grammar and
    /// reused thereafter. `Syntax` runs on `spawn_blocking` workers, so
    /// this is a handful of stores per process rather than one per buffer.
    static POOLED: RefCell<Option<WasmStore>> = const { RefCell::new(None) };
}

/// Run `f` with `parser` bound to `lang`, lending a pooled store when the
/// grammar needs one and returning it afterwards.
///
/// For short-lived parsers only — the store goes back to the pool when
/// this returns, so anything `f` produces must not need it. A `Tree` does
/// not (proven, and the reason this is safe); a parser kept for later
/// reparses does, which is what [`set_language`] is for.
///
/// Returns `None` if the language could not be bound at all.
pub(crate) fn with_pooled_store<R>(
    parser: &mut Parser,
    lang: &Language,
    f: impl FnOnce(&mut Parser) -> R,
) -> Option<R> {
    if !lang.is_wasm() {
        parser.set_language(lang).ok()?;
        return Some(f(parser));
    }

    let store = POOLED
        .with(|p| p.borrow_mut().take())
        .or_else(|| WasmStore::new(engine()).ok())?;
    parser.set_wasm_store(store).ok()?;
    // From here the store is inside `parser` and must come back out on
    // every path, or the pool silently empties and each later injection
    // pays 5 ms to build a fresh one.
    let bound = parser.set_language(lang).is_ok();
    let out = bound.then(|| f(parser));
    if let Some(store) = parser.take_wasm_store() {
        POOLED.with(|p| *p.borrow_mut() = Some(store));
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    /// The artefact the LG.1 tests already build. Absent means the
    /// prerequisites are missing, which is a skip rather than a failure.
    fn markdown_wasm() -> Option<Vec<u8>> {
        let p = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/wasm-grammars/tree-sitter-markdown.wasm"
        );
        std::fs::read(p).ok()
    }

    #[test]
    fn a_wasm_grammar_loads_and_binds_to_a_fresh_parser() {
        let Some(bytes) = markdown_wasm() else {
            eprintln!("SKIPPED — run scripts/build-wasm-grammar.sh first");
            return;
        };
        let lang = load("markdown", &bytes).expect("loads");
        assert!(lang.is_wasm());

        let mut parser = Parser::new();
        set_language(&mut parser, &lang).expect("binds");
        let tree = parser.parse("# hi\n\n- a\n", None).expect("parses");
        assert!(!tree.root_node().has_error());
    }

    /// The property the pool depends on: the borrowed store goes back, so
    /// a second call does not build another one. Checked by observing that
    /// the pool is non-empty afterwards — the cheap proxy for "we did not
    /// leak it into the parser".
    #[test]
    fn the_pooled_store_is_returned_after_use() {
        let Some(bytes) = markdown_wasm() else {
            eprintln!("SKIPPED — run scripts/build-wasm-grammar.sh first");
            return;
        };
        let lang = load("markdown", &bytes).expect("loads");

        let mut parser = Parser::new();
        let sexp = with_pooled_store(&mut parser, &lang, |p| {
            p.parse("# hi\n", None).map(|t| t.root_node().to_sexp())
        })
        .expect("bound")
        .expect("parsed");
        assert!(sexp.starts_with("(document"));

        assert!(
            POOLED.with(|p| p.borrow().is_some()),
            "the store must return to the pool, or every injection pays ~6 ms"
        );

        // And a second use works off the returned store.
        let mut parser2 = Parser::new();
        let again = with_pooled_store(&mut parser2, &lang, |p| p.parse("## two\n", None).is_some());
        assert_eq!(again, Some(true));
    }

    /// A native grammar must not touch any of this — no store created, no
    /// pool entry, nothing.
    #[test]
    fn native_grammars_never_create_a_store() {
        let lang: Language = tree_sitter_json::LANGUAGE.into();
        assert!(!lang.is_wasm());

        let mut parser = Parser::new();
        set_language(&mut parser, &lang).expect("binds");
        assert!(
            parser.take_wasm_store().is_none(),
            "a native language must not have been given a wasm store"
        );

        let mut parser2 = Parser::new();
        let ok = with_pooled_store(&mut parser2, &lang, |p| p.parse("{}", None).is_some());
        assert_eq!(ok, Some(true));
    }
}
