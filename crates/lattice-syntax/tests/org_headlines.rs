//! LG.4 — org's per-level headlines, the first real consumer of the
//! plugin-language seam.
//!
//! Design:
//! [`plugin-languages.md`](../../../docs/dev/architecture/plugin-languages.md) §7.
//!
//! Org is the forcing case the design picked, and it exercises the seam
//! properly rather than thinly:
//!
//! - **Its grammar is not on crates.io.** `nvim-orgmode/tree-sitter-org` is
//!   the maintained fork; the crates.io `tree-sitter-org` is a stale ancestor
//!   pinned to tree-sitter <0.21 against our 0.26. So it cannot be a workspace
//!   dependency at all — which is the whole argument for the seam.
//! - **Per-level headlines the hard way.** Org's `stars` is ONE node whose
//!   *text length* is the level, unlike markdown's distinct
//!   `atx_h1_marker`…`atx_h6_marker`. Per-level capture therefore needs
//!   `#eq?` text predicates, and **no query bundled with lattice uses one**.
//!   This file is what proves the pipeline evaluates them.
//!
//! The query under test is the reference plugin's real one
//! (`examples/org-plugin/queries/highlights.scm`), read from disk rather than
//! duplicated here, so the thing that ships is the thing that is tested.
//!
//! ## Why this fetches
//!
//! Org's grammar deliberately does not live in this repo — that is the point
//! of the seam, and it is why `plugins/` (which every workspace build
//! compiles) is the wrong home for a 2.2 MB generated `parser.c`. The test
//! clones it on demand into `target/` and **skips** when that is not
//! possible, so an offline checkout stays green.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use lattice_syntax::{GrammarSpec, Lang, Style, Syntax, plugin_lang};

const ORG_REPO: &str = "https://github.com/nvim-orgmode/tree-sitter-org";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/<name> is two levels below the root")
        .to_path_buf()
}

fn unique(tag: &str) -> (String, String, u64) {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    (
        format!("lg4{tag}{n}"),
        format!("lg4x{tag}{n}"),
        7_400_000 + n,
    )
}

/// Clone (once) and build org's grammar to wasm. `None` means no network and
/// no cached clone, which is a skip rather than a failure.
fn org_grammar_wasm() -> Option<Vec<u8>> {
    let root = repo_root();
    let built = root.join("target/wasm-grammars/tree-sitter-org.wasm");
    if let Ok(bytes) = std::fs::read(&built) {
        return Some(bytes);
    }

    let src_repo = root.join("target/org-grammar");
    if !src_repo.join("src/parser.c").is_file() {
        let _ = std::fs::remove_dir_all(&src_repo);
        let ok = std::process::Command::new("git")
            .args(["clone", "--depth", "1", "--quiet", ORG_REPO])
            .arg(&src_repo)
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
        .arg(src_repo.join("src"))
        .current_dir(&root)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ok.then(|| std::fs::read(&built).ok()).flatten()
}

fn org_highlights() -> String {
    std::fs::read_to_string(repo_root().join("examples/org-plugin/queries/highlights.scm"))
        .expect("the reference plugin's query ships in this repo")
}

fn register_org(tag: &str) -> Option<(Lang, u64)> {
    let bytes = org_grammar_wasm()?;
    let (name, ext, plugin) = unique(tag);
    let grammar = lattice_syntax::wasm_grammar::load("org", &bytes).expect("org grammar loads");
    let spec = GrammarSpec {
        grammar,
        highlights: Some(org_highlights()),
        folds: None,
        injections: None,
        indents: None,
        textobjects: None,
    };
    let interned = plugin_lang::register_with_grammar(&name, &[&ext], &spec, plugin)
        .expect("org registers — a query failure here names the offending file");
    let lang = Lang::detect_from_path(Some(&PathBuf::from(format!("notes.{ext}"))));
    assert_eq!(lang, Lang::Plugin(interned));
    Some((lang, plugin))
}

fn skip(what: &str) {
    eprintln!(
        "{what}: SKIPPED — could not fetch/build the org grammar (offline?). \
         It is deliberately not vendored; see the module docs."
    );
}

/// The headline claim, level by level. `*` → Heading1 … `******` → Heading6.
///
/// This is the assertion the whole slice exists for. If `#eq?` were ignored
/// by the pipeline, EVERY pattern would match every headline and the last one
/// would win — so a failure here shows up as "everything is Heading6", not as
/// an error.
#[test]
fn stars_select_the_heading_level() {
    let Some((lang, plugin)) = register_org("levels") else {
        return skip("stars_select_the_heading_level");
    };

    let src = "* One\n** Two\n*** Three\n**** Four\n***** Five\n****** Six\n";
    let mut syntax = Syntax::for_language(lang).unwrap().unwrap();
    syntax.parse(src);
    let lines = syntax.highlight_lines_native(0, 6).expect("highlights");

    let expected = [
        Style::Heading1,
        Style::Heading2,
        Style::Heading3,
        Style::Heading4,
        Style::Heading5,
        Style::Heading6,
    ];
    for (i, want) in expected.iter().enumerate() {
        let styles: Vec<Style> = lines[i].iter().map(|s| s.style).collect();
        assert!(
            styles.contains(want),
            "line {i} ({:?}) should carry {want:?}, got {styles:?}",
            src.lines().nth(i).unwrap()
        );
        // And ONLY that heading level — the proof that predicates filtered.
        for other in expected.iter().filter(|s| *s != want) {
            assert!(
                !styles.contains(other),
                "line {i} carries {other:?} as well as {want:?} — \
                 #eq? predicates are not being evaluated"
            );
        }
    }
    plugin_lang::unregister_plugin(plugin);
}

/// The stars themselves stay a base-size marker style, which is what lets the
/// GPUI peer render `[stars][title]` as two pieces on one baseline. Same
/// style markdown's `#` markers take, so the renderer needs no org case.
#[test]
fn the_stars_are_a_base_size_marker_run() {
    let Some((lang, plugin)) = register_org("stars") else {
        return skip("the_stars_are_a_base_size_marker_run");
    };

    let mut syntax = Syntax::for_language(lang).unwrap().unwrap();
    syntax.parse("** Title\n");
    let lines = syntax.highlight_lines_native(0, 1).expect("highlights");

    let stars = lines[0]
        .iter()
        .find(|s| s.style == Style::Markup)
        .expect("the stars run is captured as a marker");
    assert_eq!((stars.start, stars.end), (0, 2), "just the `**`");
    let title = lines[0]
        .iter()
        .find(|s| s.style == Style::Heading2)
        .expect("the title run is the heading");
    assert!(
        title.start >= stars.end,
        "the title must follow the markers, so the prefix can stay base-size"
    );
    plugin_lang::unregister_plugin(plugin);
}

/// Org has no depth limit; the theme's scale ramp does. Seven stars or more
/// shares level 6 rather than losing its heading identity entirely.
#[test]
fn headlines_deeper_than_six_stay_headings() {
    let Some((lang, plugin)) = register_org("deep") else {
        return skip("headlines_deeper_than_six_stay_headings");
    };
    let mut syntax = Syntax::for_language(lang).unwrap().unwrap();
    syntax.parse("******* Seven\n");
    let styles: Vec<Style> = syntax.highlight_lines_native(0, 1).unwrap()[0]
        .iter()
        .map(|s| s.style)
        .collect();
    assert!(
        styles.contains(&Style::Heading6),
        "a 7-star headline should degrade to Heading6, got {styles:?}"
    );
    plugin_lang::unregister_plugin(plugin);
}

/// Body text is not a headline. Cheap, but it is what would catch a query
/// that matched too broadly — the failure mode `#eq?` is guarding against.
#[test]
fn body_text_is_not_a_headline() {
    let Some((lang, plugin)) = register_org("body") else {
        return skip("body_text_is_not_a_headline");
    };
    let mut syntax = Syntax::for_language(lang).unwrap().unwrap();
    syntax.parse("* Head\nplain body line\n");
    let styles: Vec<Style> = syntax.highlight_lines_native(1, 2).unwrap()[0]
        .iter()
        .map(|s| s.style)
        .collect();
    for h in [Style::Heading1, Style::Heading2, Style::Heading6] {
        assert!(!styles.contains(&h), "body line got {h:?}: {styles:?}");
    }
    plugin_lang::unregister_plugin(plugin);
}
