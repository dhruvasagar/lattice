//! LG.5 — org folding, through the same pipeline every other language uses.
//!
//! Design:
//! [`plugin-languages.md`](../../../docs/dev/architecture/plugin-languages.md) §7.
//!
//! `compute_syntax_folds` resolves `folds.scm` by language name through the
//! live registry and knows nothing about where the grammar came from, so this
//! slice is queries and tests rather than mechanism. Which is precisely what
//! is worth checking: if a plugin language needed *anything* special to fold,
//! the seam would have failed at its purpose.
//!
//! The query under test is the reference plugin's real
//! `examples/org-plugin/queries/folds.scm`, read from disk rather than
//! duplicated, so the thing that ships is the thing that is tested.
//!
//! Fetches org's grammar on demand and **skips** when that is not possible;
//! it is deliberately not vendored (design §7).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use lattice_host::folds::compute_syntax_folds;
use lattice_syntax::{GrammarSpec, Lang, Syntax, plugin_lang};

const ORG_REPO: &str = "https://github.com/nvim-orgmode/tree-sitter-org";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/<name> is two levels below the root")
        .to_path_buf()
}

fn org_grammar_wasm() -> Option<Vec<u8>> {
    let root = repo_root();
    let built = root.join("target/wasm-grammars/tree-sitter-org.wasm");
    if let Ok(bytes) = std::fs::read(&built) {
        return Some(bytes);
    }
    let src = root.join("target/org-grammar");
    if !src.join("src/parser.c").is_file() {
        let _ = std::fs::remove_dir_all(&src);
        let ok = std::process::Command::new("git")
            .args(["clone", "--depth", "1", "--quiet", ORG_REPO])
            .arg(&src)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return None;
        }
    }
    let ok = std::process::Command::new("bash")
        .arg(root.join("scripts/build-wasm-grammar.sh"))
        .arg("org")
        .arg(src.join("src"))
        .current_dir(&root)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ok.then(|| std::fs::read(&built).ok()).flatten()
}

/// Register org with its real folds query and parse `src`.
fn org_syntax(tag: &str, src: &str) -> Option<(Syntax, u64)> {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let (name, ext, plugin) = (
        format!("lg5{tag}{n}"),
        format!("lg5x{tag}{n}"),
        7_500_000 + n,
    );

    let bytes = org_grammar_wasm()?;
    let grammar = lattice_syntax::wasm_grammar::load("org", &bytes).expect("org grammar loads");
    let root = repo_root();
    let spec = GrammarSpec {
        grammar,
        highlights: None,
        folds: Some(
            std::fs::read_to_string(root.join("examples/org-plugin/queries/folds.scm"))
                .expect("the reference plugin's folds query ships in this repo"),
        ),
        injections: None,
        indents: None,
        textobjects: None,
    };
    let interned = plugin_lang::register_with_grammar(&name, &[&ext], &spec, plugin)
        .expect("org registers — a query failure here names the offending file");

    let mut syntax = Syntax::for_language(Lang::Plugin(interned))
        .expect("registry")
        .expect("org has a grammar");
    syntax.parse(src);
    Some((syntax, plugin))
}

fn skip(what: &str) {
    eprintln!("{what}: SKIPPED — could not fetch/build the org grammar (offline?)");
}

/// Folding a headline hides its subtree — the thing org users mean by
/// folding. A `(section)` in this grammar is the headline plus everything
/// beneath it, so this is one capture doing the work.
#[test]
fn folding_a_headline_hides_its_subtree() {
    let src = "* One\nbody one\nmore body\n* Two\nbody two\n";
    let Some((syntax, plugin)) = org_syntax("head", src) else {
        return skip("folding_a_headline_hides_its_subtree");
    };

    let folds = compute_syntax_folds(syntax.snapshot()).expect("org folds.scm resolves");
    // The first headline's section spans lines 0..=2 (through `more body`),
    // stopping before the sibling headline on line 3.
    assert!(
        folds
            .iter()
            .any(|f| f.start_line == 0 && f.end_line >= 2 && f.end_line < 3),
        "the first headline should fold its body but not the next headline: {folds:?}"
    );
    plugin_lang::unregister_plugin(plugin);
}

/// Nested headlines fold independently, which is what gives org its
/// per-level cycling without any special handling — they are simply separate
/// `(section)` nodes.
#[test]
fn nested_headlines_fold_independently() {
    let src = "* Top\nintro\n** Child\nchild body\n*** Grand\ndeep body\n";
    let Some((syntax, plugin)) = org_syntax("nest", src) else {
        return skip("nested_headlines_fold_independently");
    };

    let folds = compute_syntax_folds(syntax.snapshot()).expect("folds");
    for start in [0u32, 2, 4] {
        assert!(
            folds.iter().any(|f| f.start_line == start),
            "expected a fold starting at line {start}: {folds:?}"
        );
    }
    // The outer fold must contain the inner ones.
    let top = folds.iter().find(|f| f.start_line == 0).expect("top fold");
    assert!(
        top.end_line >= 5,
        "the top-level section should span the whole subtree, got {top:?}"
    );
    plugin_lang::unregister_plugin(plugin);
}

/// A `#+BEGIN_SRC` block folds, and so does a drawer — the two things that
/// are noise most of the time.
#[test]
fn blocks_and_drawers_fold() {
    let src = "* Head\n:PROPERTIES:\n:ID: 42\n:CUSTOM: x\n:END:\n\
               #+BEGIN_SRC rust\nfn f() {}\nlet x = 1;\n#+END_SRC\n";
    let Some((syntax, plugin)) = org_syntax("block", src) else {
        return skip("blocks_and_drawers_fold");
    };

    let folds = compute_syntax_folds(syntax.snapshot()).expect("folds");
    assert!(
        folds.iter().any(|f| f.start_line == 1 && f.end_line >= 3),
        "the property drawer (lines 1..4) should fold: {folds:?}"
    );
    assert!(
        folds.iter().any(|f| f.start_line == 5 && f.end_line >= 7),
        "the #+BEGIN_SRC block (lines 5..8) should fold: {folds:?}"
    );
    plugin_lang::unregister_plugin(plugin);
}

/// The pipeline drops single-line captures, so a childless headline is not
/// foldable. Worth pinning because the alternative — a zero-height fold
/// marker on every bare headline — would be visible noise in exactly the
/// files org users have most of.
#[test]
fn a_childless_headline_is_not_foldable() {
    let src = "* Alone\n* Also alone\n";
    let Some((syntax, plugin)) = org_syntax("bare", src) else {
        return skip("a_childless_headline_is_not_foldable");
    };
    let folds = compute_syntax_folds(syntax.snapshot()).expect("folds");
    assert!(
        folds.is_empty(),
        "bare headlines have nothing to hide, got {folds:?}"
    );
    plugin_lang::unregister_plugin(plugin);
}

/// `compute_syntax_folds` returning `Some` at all is the seam's claim: a
/// plugin language is authoritative for its own folds, so the caller does not
/// cascade to the indent heuristic. `None` would mean org silently fell back
/// to indent-based folding, which for a file full of `*` markers would be
/// wrong in a way nobody would report as a fold bug.
#[test]
fn org_is_authoritative_for_its_own_folds() {
    let Some((syntax, plugin)) = org_syntax("auth", "plain text, no structure\n") else {
        return skip("org_is_authoritative_for_its_own_folds");
    };
    // `Fold` is not `PartialEq`, so this checks the two things that matter
    // separately: authoritative (`Some`), and empty.
    let folds = compute_syntax_folds(syntax.snapshot());
    assert!(
        folds.is_some(),
        "a plugin language must be authoritative for its own folds — `None` \
         would cascade to the indent heuristic, which for a file full of `*` \
         markers is wrong in a way nobody reports as a fold bug"
    );
    assert!(
        folds.unwrap().is_empty(),
        "nothing to fold in a structureless file"
    );
    plugin_lang::unregister_plugin(plugin);
}
