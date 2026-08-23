//! LG.1 — a wasm-loaded grammar is indistinguishable from the native one.
//!
//! Design: [`plugin-languages.md`](../../../docs/dev/architecture/plugin-languages.md) §2, §4.
//!
//! The design's whole claim to being small rests on one fact: `WasmStore::
//! load_language` yields a **real `tree_sitter::Language`**, so everything
//! downstream — `HighlightConfiguration`, folds, injections, text objects,
//! the incremental reparse path — works unchanged. This test is that claim,
//! checked rather than asserted in prose: the same grammar is loaded both
//! ways and the two must produce byte-identical trees.
//!
//! It also guards the **build recipe**, which is the fragile part. The
//! artefact is produced by `scripts/build-wasm-grammar.sh` (clang + rustup,
//! no emscripten / docker / tree-sitter CLI — see that script's header),
//! and its link flags are not obvious: drop `--Bsymbolic` and the external
//! scanner entry points stay preemptible, so the module imports
//! `GOT.func.tree_sitter_markdown_external_scanner_create` and instantiation
//! fails. That mistake was made once already. Building through the script
//! here means the documented route is what gets exercised.
//!
//! Gated on `--features wasm-grammar`, which pulls tree-sitter's own
//! wasmtime 36 alongside the plugin host's 46 (LG.0 cleared that; see
//! `plugin-languages.md` §3.1). An ordinary build links neither.
#![cfg(feature = "wasm-grammar")]
#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/<name> is two levels below the root")
        .to_path_buf()
}

/// The vendored `tree-sitter-markdown` C sources. They ship inside the
/// `tree-sitter-md` crate, which is already a dependency, but cargo
/// exposes no path to a dependency's sources — hence the registry walk.
/// `LATTICE_TS_MD_SRC` overrides it for anyone with a vendored or
/// non-default registry layout.
fn markdown_grammar_src() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("LATTICE_TS_MD_SRC") {
        let p = PathBuf::from(p);
        return p.join("parser.c").is_file().then_some(p);
    }
    let cargo_home = std::env::var("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".cargo")))
        .ok()?;
    // registry/src/<index-hash>/tree-sitter-md-<ver>/tree-sitter-markdown/src
    let mut best: Option<PathBuf> = None;
    for index in std::fs::read_dir(cargo_home.join("registry/src")).ok()? {
        let Ok(index) = index else { continue };
        let Ok(entries) = std::fs::read_dir(index.path()) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            if !name.starts_with("tree-sitter-md-") {
                continue;
            }
            let src = e.path().join("tree-sitter-markdown/src");
            if src.join("parser.c").is_file() {
                // Lexicographically-greatest version wins; any of them
                // proves the mechanism, but pinning to the newest keeps
                // this aligned with what the crate graph actually uses.
                if best.as_ref().is_none_or(|b| b.as_path() < src.as_path()) {
                    best = Some(src);
                }
            }
        }
    }
    best
}

/// Build the wasm grammar through the documented script, returning its
/// bytes. `None` means the prerequisites are absent, which is a skip
/// rather than a failure — see the call site.
fn build_wasm_grammar() -> Option<Vec<u8>> {
    let root = repo_root();
    let out = root.join("target/wasm-grammars/tree-sitter-markdown.wasm");
    let src = markdown_grammar_src()?;

    let status = std::process::Command::new("bash")
        .arg(root.join("scripts/build-wasm-grammar.sh"))
        .arg("markdown")
        .arg(&src)
        .current_dir(&root)
        .status()
        .ok()?;
    if !status.success() {
        panic!(
            "build-wasm-grammar.sh failed for {}. The script needs clang and a \
             rustup toolchain (for rust-lld); if one is missing, that is the fix.",
            src.display()
        );
    }
    std::fs::read(&out).ok()
}

/// Markdown that reaches the external scanner (list continuation, fence
/// matching, block quotes) as well as the ordinary block parser. A
/// paragraph-only corpus would pass even with the scanner mislinked,
/// which is the failure this file exists to catch.
fn corpus() -> String {
    let mut s = String::from("# Top\n\n");
    for i in 0..24 {
        s.push_str(&format!(
            "## Section {i}\n\n\
             Body with **bold**, `code`, _emphasis_ and a [link](https://example.com).\n\n\
             - item one\n- item two\n  - nested\n\n\
             > quoted\n\n\
             ```rust\nfn f_{i}() -> u32 {{ {i} }}\n```\n\n"
        ));
    }
    s
}

#[test]
fn wasm_grammar_parses_identically_to_native() {
    let Some(wasm) = build_wasm_grammar() else {
        eprintln!(
            "wasm_grammar_parses_identically_to_native: SKIPPED — could not \
             locate tree-sitter-md sources (set LATTICE_TS_MD_SRC) or build the \
             artefact. See scripts/build-wasm-grammar.sh."
        );
        return;
    };

    let engine = tree_sitter::wasmtime::Engine::default();
    let mut store = tree_sitter::WasmStore::new(&engine).expect("wasm store");
    let wasm_lang = store
        .load_language("markdown", &wasm)
        .expect("wasm grammar should load; check --Bsymbolic in the build script");
    assert!(
        wasm_lang.is_wasm(),
        "language should report wasm provenance"
    );

    let mut wasm_parser = tree_sitter::Parser::new();
    wasm_parser.set_wasm_store(store).unwrap();
    wasm_parser.set_language(&wasm_lang).unwrap();

    let mut native_parser = tree_sitter::Parser::new();
    native_parser
        .set_language(&tree_sitter_md::LANGUAGE.into())
        .unwrap();

    let src = corpus();

    // Cold parse: identical trees, no errors.
    let native = native_parser.parse(&src, None).unwrap();
    let via_wasm = wasm_parser.parse(&src, None).unwrap();
    assert!(!native.root_node().has_error(), "native parse has errors");
    assert!(!via_wasm.root_node().has_error(), "wasm parse has errors");
    assert_eq!(
        native.root_node().to_sexp(),
        via_wasm.root_node().to_sexp(),
        "wasm and native grammars disagree on the cold parse"
    );

    // Incremental reparse: the path an edit actually takes, and the one
    // the LG.1 bench measures. A grammar that agrees cold but diverges
    // after `Tree::edit` would be worse than one that never loaded.
    let at = src.find("Body with").expect("corpus shape changed");
    let inserted = "very ";
    let after = format!("{}{}{}", &src[..at], inserted, &src[at..]);
    let row = src[..at].matches('\n').count();
    let col = at - src[..at].rfind('\n').map_or(0, |i| i + 1);
    let edit = tree_sitter::InputEdit {
        start_byte: at,
        old_end_byte: at,
        new_end_byte: at + inserted.len(),
        start_position: tree_sitter::Point::new(row, col),
        old_end_position: tree_sitter::Point::new(row, col),
        new_end_position: tree_sitter::Point::new(row, col + inserted.len()),
    };

    let mut native_old = native;
    let mut wasm_old = via_wasm;
    native_old.edit(&edit);
    wasm_old.edit(&edit);
    let native_new = native_parser.parse(&after, Some(&native_old)).unwrap();
    let wasm_new = wasm_parser.parse(&after, Some(&wasm_old)).unwrap();
    assert!(!wasm_new.root_node().has_error(), "wasm reparse has errors");
    assert_eq!(
        native_new.root_node().to_sexp(),
        wasm_new.root_node().to_sexp(),
        "wasm and native grammars disagree after an incremental reparse"
    );
}

#[test]
fn wasm_grammar_module_has_no_unresolvable_imports() {
    let Some(wasm) = build_wasm_grammar() else {
        eprintln!("wasm_grammar_module_has_no_unresolvable_imports: SKIPPED");
        return;
    };
    // tree-sitter's store resolves exactly two families of import: its
    // builtins (memory / stack pointer / table / bases, plus abort and
    // friends) and the 24 symbols of its bundled wasm libc. Anything
    // else fails instantiation with "invalid import", which is how the
    // missing --Bsymbolic showed up. Loading is the check; this test
    // exists to name the failure when it recurs.
    let engine = tree_sitter::wasmtime::Engine::default();
    let mut store = tree_sitter::WasmStore::new(&engine).expect("wasm store");
    match store.load_language("markdown", &wasm) {
        Ok(_) => {}
        Err(e) => panic!(
            "grammar module has an import tree-sitter's store cannot provide: {e}\n\
             The usual cause is a missing --Bsymbolic in scripts/build-wasm-grammar.sh, \
             which leaves the external-scanner entry points preemptible and turns them \
             into GOT.func imports."
        ),
    }
}
