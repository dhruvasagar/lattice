//! TC.5/TC.10 — every bundled `@context` query must compile against the
//! grammar it names.
//!
//! A tree-sitter query is compiled ALL-OR-NOTHING: one bad node kind or one
//! bad field name and `Query::new` rejects the entire source. Inside the
//! plugin that failure surfaces as `compile_query` returning `err`, the
//! producer returning no scopes, and the strip silently never appearing for
//! that language — indistinguishable from "this language has no query", which
//! is a legitimate state the plugin is designed to have.
//!
//! That is a bad failure to discover by opening a Go file. These queries are
//! static text and every grammar they name is linked into the workspace, so
//! the check costs a test rather than a bug report. It became load-bearing at
//! TC.10, when the queries gained `@context.end` captures written against
//! per-language FIELD names — the exact thing that is easy to get wrong once
//! per language and impossible to notice without opening each one.
//!
//! This deliberately does not assert what the queries capture: the shape of a
//! good context set is a judgement call that belongs in review, not a test
//! that fails whenever a construct is added.

#![allow(clippy::unwrap_used)]

use tree_sitter::Query;

/// The bundled queries, paired with the grammar id the plugin dispatches them
/// on. Kept in step with `query_for` in `plugins/treesitter-context/src/lib.rs`
/// by hand — a divergence shows up here as a query nothing tests, which is
/// preferable to the reverse.
const BUNDLED: &[(&str, &str)] = &[
    (
        "rust",
        include_str!("../../../plugins/treesitter-context/queries/rust.scm"),
    ),
    (
        "python",
        include_str!("../../../plugins/treesitter-context/queries/python.scm"),
    ),
    (
        "go",
        include_str!("../../../plugins/treesitter-context/queries/go.scm"),
    ),
    (
        "javascript",
        include_str!("../../../plugins/treesitter-context/queries/javascript.scm"),
    ),
    (
        "typescript",
        include_str!("../../../plugins/treesitter-context/queries/typescript.scm"),
    ),
    (
        "c",
        include_str!("../../../plugins/treesitter-context/queries/c.scm"),
    ),
    (
        "cpp",
        include_str!("../../../plugins/treesitter-context/queries/c.scm"),
    ),
    (
        "markdown",
        include_str!("../../../plugins/treesitter-context/queries/markdown.scm"),
    ),
];

#[test]
fn every_bundled_context_query_compiles_against_its_grammar() {
    let registry = lattice_syntax::LangRegistry::standard().expect("standard registry builds");

    for (lang, source) in BUNDLED {
        let language = registry
            .tree_sitter_language(lang)
            .unwrap_or_else(|| panic!("grammar `{lang}` is linked into the workspace"));
        if let Err(e) = Query::new(&language, source) {
            panic!(
                "the bundled `{lang}` context query does not compile, so the \
                 strip would silently never appear for {lang} files: {e}"
            );
        }
    }
}

/// The header span is derived from the `@context.end` capture, so a query that
/// captures no ends yields single-line headers everywhere — a wrapped
/// signature would pin only its first line, which reads as a truncation bug
/// rather than a missing capture.
///
/// Only asserted for the languages whose constructs genuinely have bodies;
/// markdown sections do not, and pinning a heading's single line is correct
/// there.
#[test]
fn queries_for_body_bearing_languages_capture_context_end() {
    for (lang, source) in BUNDLED {
        if *lang == "markdown" {
            continue;
        }
        assert!(
            source.contains("@context.end"),
            "`{lang}` captures no `@context.end`, so every header it produces \
             collapses to one line"
        );
    }
}
