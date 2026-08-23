//! LG.3c language fixture guest.
//!
//! Declares four languages through the imported `register-language`, chosen
//! to cover the shapes that can fail silently:
//!
//!   - `lg3c-md` — the real one. A markdown grammar compiled to wasm by this
//!     crate's own build.rs, plus a highlights query. Must parse and highlight
//!     through the ordinary paths once registered.
//!   - `lg3c-badgrammar` — bytes that are not a wasm module at all. The host
//!     must reject it naming the language, WITHOUT failing the load.
//!   - `lg3c-badquery` — a valid grammar with a `folds.scm` that does not
//!     compile. This is the one worth having a fixture for: queries compile at
//!     registration precisely so this surfaces now rather than as "folding
//!     silently does nothing" three days later.
//!   - `markdown` — deliberately claims a BUNDLED language's name. Must be
//!     refused, and the real bundled markdown must be untouched. Without a
//!     fixture that tries this, nothing proves the refusal is load-bearing
//!     rather than decorative.
//!
//! The grammar is `include_bytes!`'d from `OUT_DIR`, which is the seam's
//! premise and the workflow LG.4 expects of a real plugin: the grammar is the
//! plugin's build artefact, not the editor's.

wit_bindgen::generate!({
    world: "language-plugin",
    path: "../../../../../wit",
});

use lattice::plugin_host::language::{LanguageSpec, register_language};

/// Built by build.rs. Empty when the toolchain was unavailable, in which case
/// the host rejects every registration for empty bytes and the dependent test
/// skips — a legible failure rather than a mysterious one.
const GRAMMAR: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/grammar.wasm"));

/// Captures headings and emphasis. Deliberately small: the point is to prove
/// the query reaches the registry compiled and produces spans, not to
/// re-test markdown highlighting.
const HIGHLIGHTS: &str = r#"
(atx_h1_marker) @punctuation.special
(atx_h2_marker) @punctuation.special
(fenced_code_block) @markup.raw
"#;

struct Component;

fn spec(name: &str, exts: &[&str], grammar: Vec<u8>) -> LanguageSpec {
    LanguageSpec {
        name: name.to_string(),
        // The fixture's grammar exports `tree_sitter_markdown`, but `markdown`
        // is a BUNDLED language name and is refused. Splitting the two is
        // exactly what `grammar-name` is for, and the same split lattice's own
        // `sql`-on-`sequel` needs.
        grammar_name: Some("markdown".to_string()),
        extensions: exts.iter().map(|s| (*s).to_string()).collect(),
        grammar,
        highlights: Some(HIGHLIGHTS.to_string()),
        folds: None,
        injections: None,
        indents: None,
        textobjects: None,
    }
}

impl Guest for Component {
    fn register_languages() {
        // The real one: registered as `lg3c-md`, loaded from the grammar's
        // own `tree_sitter_markdown` export via `grammar-name`.
        let _ = register_language(&spec("lg3c-md", &["lg3cmd"], GRAMMAR.to_vec()));

        // Not a wasm module.
        let _ = register_language(&spec(
            "lg3c-badgrammar",
            &["lg3cbad"],
            b"this is not wasm".to_vec(),
        ));

        // Valid grammar, uncompilable folds query. Must be rejected naming
        // the query, and must not cost the languages around it.
        let mut bad_query = spec("lg3c-badquery", &["lg3cbq"], GRAMMAR.to_vec());
        bad_query.folds = Some("(no_such_node_kind) @fold".to_string());
        let _ = register_language(&bad_query);

        // Squats a bundled language's name. Must be refused, and the real
        // bundled markdown must be untouched.
        let _ = register_language(&spec("markdown", &["lg3csquat"], GRAMMAR.to_vec()));
    }
}

export!(Component);
