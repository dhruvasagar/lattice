//! The reference org language plugin.
//!
//! Implements the `language-plugin` world: imports `register-language`,
//! exports `register-languages`. The grammar is compiled to wasm by this
//! crate's build.rs and baked in with `include_bytes!`; the queries ship as
//! source and are compiled host-side at registration, so a malformed one
//! fails at load naming the file.
//!
//! This is the whole plugin. Everything else org needs — headline promotion,
//! TODO cycling, visibility cycling, agenda — is org-mode the MAJOR MODE, a
//! separate track riding seams that already exist (`modes`, `keymap`,
//! `grammar`). It is gated on nothing here.

wit_bindgen::generate!({
    world: "language-plugin",
    path: "../../wit",
});

use lattice::plugin_host::language::{LanguageSpec, register_language};

const GRAMMAR: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/grammar.wasm"));

struct Component;

impl Guest for Component {
    fn register_languages() {
        let _ = register_language(&LanguageSpec {
            name: "org".to_string(),
            // The grammar's export is `tree_sitter_org`, which matches the
            // language name — so this is the common case and the field is
            // absent. It exists for grammars whose upstream name differs
            // (lattice's own `sql` rides `sequel`).
            grammar_name: None,
            extensions: vec!["org".to_string(), "org_archive".to_string()],
            grammar: GRAMMAR.to_vec(),
            highlights: Some(include_str!("../queries/highlights.scm").to_string()),
            // LG.5 adds folds over (section)/(block)/(drawer)/(list).
            folds: None,
            injections: None,
            indents: None,
            textobjects: None,
        });
    }
}

export!(Component);
