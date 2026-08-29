//! The `language` guest→host registration seam (LG.3c).
//!
//! Design:
//! [`plugin-languages.md`](../../../docs/dev/architecture/plugin-languages.md) §2.2.
//!
//! A language-contributing plugin implements the `language-plugin` world: it
//! **imports** the `language` API (`register-language`) and **exports**
//! `register-languages`, which the host calls once to drive declaration. The
//! `help-plugin` precedent, shape for shape.
//!
//! ## What this module does NOT do
//!
//! It does not compile the grammar and it does not touch `lattice-syntax`.
//! The seam collects plain [`LanguageSpec`] values — bytes and query strings —
//! and hands them back; `lattice-plugin-loader` turns them into a real
//! language. That keeps the dependency pointing the way it already does: the
//! loader is the crate that knows about the editor's native registries, the
//! host is the crate that knows about wasm.
//!
//! It matters more here than it did for `help`, because compiling a grammar
//! costs ~100 ms. Doing it inside the guest call would hold the guest's
//! `Store` alive across a Cranelift compile for no reason; doing it in the
//! drain, after the store is dropped, is both cheaper and the shape the rest
//! of the loader already has.
//!
//! ## The name is NOT namespaced, unlike `help`
//!
//! A help topic is auto-prefixed with the plugin id so a guest cannot squat
//! `:help buffers`. A language deliberately is not, and the difference is
//! principled rather than an oversight:
//!
//! - A language's name has to match its grammar's `tree_sitter_<name>` export
//!   and the `#+BEGIN_SRC <name>` / fenced-code-block identifier users
//!   already type. `org-plugin.org` would match neither.
//! - The collision it would defend against is instead refused outright:
//!   `lattice_syntax` rejects any name a bundled language already uses, and
//!   rejects a name a *different* plugin has already claimed. So the property
//!   namespacing buys for `help` is bought here by refusal, which is the
//!   right trade when the name is load-bearing rather than decorative.

use crate::{
    Component, PluginBudget, PluginHost, PluginHostError, PluginManifest, TrustTier, arm_store,
    classify_trap,
};

/// One language a guest declared, ready for the loader to compile and
/// register.
///
/// Plain data. The grammar is still bytes here — deliberately, see the module
/// docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageSpec {
    pub name: String,
    /// The grammar's `tree_sitter_<x>` export name. Defaults to `name`; they
    /// differ when a grammar's upstream name is not the language's (lattice's
    /// own `sql` rides the `sequel` grammar).
    pub grammar_name: String,
    /// Lower-cased, leading dots stripped, blanks dropped.
    pub extensions: Vec<String>,
    /// The grammar, compiled to wasm, exactly as the guest supplied it.
    pub grammar: Vec<u8>,
    pub highlights: Option<String>,
    pub folds: Option<String>,
    pub injections: Option<String>,
    pub indents: Option<String>,
    pub textobjects: Option<String>,
    /// H.2: `(pattern, hide-groups)` as declared, uncompiled.
    ///
    /// Not compiled here for the same reason the queries are not: this
    /// runs with the guest's store alive, and compilation belongs in the
    /// loader's drain beside the grammar. The shape checks that DO run
    /// here are the ones that need no engine — see [`validate_language`].
    pub conceal_rules: Vec<(String, Vec<u32>)>,
}

/// The wire record, as bindgen generates it. Taken whole by
/// [`validate_language`] rather than splatted into nine positional
/// arguments — the fields are all `String`/`Option<String>`, so a
/// positional signature is exactly the shape a caller silently gets wrong.
pub use bindings::lattice::plugin_host::language::LanguageSpec as WitLanguageSpec;

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        world: "language-plugin",
        path: "../../wit",
        // Same async linker as WASI + the host func, like `help-plugin`.
        // Registration is off every hot path, so async costs nothing.
        exports: { default: async },
        with: {
            "lattice:plugin-host/logging": crate::lattice::plugin_host::logging,
            "lattice:plugin-host/project": crate::lattice::plugin_host::project,
        },
    });
}

/// Validate a guest's language declaration, or reject it.
///
/// Guest output is untrusted, and the rejections here are the ones a buggy
/// guest actually produces. Each is an `Err` back to the guest rather than a
/// trap, so one bad language costs itself and the plugin's others still
/// register.
///
/// Note what is NOT validated here: whether the grammar bytes are a loadable
/// wasm module, and whether the queries compile. Both need
/// `lattice-syntax` and both happen in the loader's drain — checking them
/// here would mean either duplicating that logic or holding the guest's store
/// alive across a ~100 ms Cranelift compile.
pub fn validate_language(raw: WitLanguageSpec) -> Result<LanguageSpec, String> {
    let WitLanguageSpec {
        name,
        grammar_name,
        grammar,
        extensions,
        highlights,
        folds,
        injections,
        indents,
        textobjects,
        conceal_rules,
    } = raw;
    let name = name.trim();
    if name.is_empty() {
        return Err("register-language: name is empty".to_string());
    }
    // The name keys the query cache, so anything that is not a plain
    // identifier is a mistake worth catching at the boundary.
    let plain = |s: &str| {
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    };
    if !plain(name) {
        return Err(format!(
            "register-language({name}): name must be alphanumeric, '_' or '-'"
        ));
    }
    // The grammar name must additionally be able to appear in
    // `tree_sitter_<x>`, so a confusing "module has no entry point" after a
    // ~100 ms compile becomes a legible rejection now.
    let grammar_name = grammar_name
        .map(|g| g.trim().to_string())
        .filter(|g| !g.is_empty())
        .unwrap_or_else(|| name.to_string());
    if !plain(&grammar_name) {
        return Err(format!(
            "register-language({name}): grammar-name '{grammar_name}' must be \
             alphanumeric, '_' or '-' — it has to match the grammar's \
             tree_sitter_<grammar-name> export"
        ));
    }

    let extensions: Vec<String> = extensions
        .iter()
        .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .collect();
    if extensions.is_empty() {
        return Err(format!(
            "register-language({name}): no file extensions, so the language \
             could never be selected"
        ));
    }

    if grammar.is_empty() {
        return Err(format!("register-language({name}): grammar is empty"));
    }

    // Blank query sources are normalised away rather than passed on as
    // `Some("")`. The design says an absent query means the feature is
    // unavailable, and `Some("   ")` should mean the same thing rather than
    // compiling to an empty query that silently matches nothing.
    let blank_to_none = |s: Option<String>| s.filter(|q| !q.trim().is_empty());

    Ok(LanguageSpec {
        name: name.to_string(),
        grammar_name,
        extensions,
        grammar,
        highlights: blank_to_none(highlights),
        folds: blank_to_none(folds),
        injections: blank_to_none(injections),
        indents: blank_to_none(indents),
        textobjects: blank_to_none(textobjects),
        // Shape only. A blank pattern or an empty `hide` cannot become a
        // working rule under any engine, so refusing them here spares the
        // loader a compile and gives the guest the reason while it is
        // still alive to log it. Everything that needs the regex engine —
        // does it compile, does group N exist — happens at compile time
        // in `lattice-syntax`, where a refusal drops one rule rather than
        // failing the language.
        conceal_rules: conceal_rules
            .into_iter()
            .filter_map(|r| {
                if r.pattern.trim().is_empty() {
                    tracing::warn!(language = name, "conceal rule dropped: empty pattern");
                    return None;
                }
                if r.hide.is_empty() {
                    tracing::warn!(
                        language = name,
                        pattern = %r.pattern,
                        "conceal rule dropped: hide is empty, so it would hide nothing"
                    );
                    return None;
                }
                Some((r.pattern, r.hide))
            })
            .collect(),
    })
}

impl PluginHost {
    /// Instantiate a `language-plugin` component under its capability grant,
    /// drive its `register-languages` export once, and return the host-issued
    /// id plus the languages it declared.
    ///
    /// Mirror of [`spawn_help_plugin`](Self::spawn_help_plugin). Nothing about
    /// the guest outlives this call: the bytes and query sources are already
    /// across, so the `Store` is dropped when the function returns and parsing
    /// never touches the guest again.
    pub async fn spawn_language_plugin(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        budget: PluginBudget,
    ) -> Result<(crate::PluginId, Vec<LanguageSpec>), PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, tier);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "language plugin loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(wasi, outcome.grant, budget, Some(&manifest.id))?;
        let bindings =
            bindings::LanguagePlugin::instantiate_async(&mut store, component, &self.linker)
                .await
                .map_err(|e| PluginHostError::Instantiate(e.into()))?;

        let id = self.alloc_id();
        store.data_mut().log_ctx = self.log_ctx_for(id);

        arm_store(&mut store, budget)?;
        bindings
            .call_register_languages(&mut store)
            .await
            .map_err(|source| PluginHostError::Trap {
                func: "register-languages",
                kind: classify_trap(&source),
                source: source.into(),
            })?;

        let languages = std::mem::take(&mut store.data_mut().language_contributions);
        Ok((id, languages))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(name: &str, exts: &[&str]) -> WitLanguageSpec {
        WitLanguageSpec {
            name: name.to_string(),
            grammar_name: None,
            grammar: vec![0, 1, 2],
            extensions: exts.iter().map(|s| (*s).to_string()).collect(),
            highlights: None,
            folds: None,
            injections: None,
            indents: None,
            textobjects: None,
            conceal_rules: vec![],
        }
    }

    fn spec(name: &str, exts: &[&str]) -> Result<LanguageSpec, String> {
        validate_language(wire(name, exts))
    }

    /// H.2: the boundary keeps rules whose shape could work under any
    /// engine and drops the two that could not, without failing the
    /// language. Compilation — does the pattern parse, does group N
    /// exist — happens in `lattice-syntax`, deliberately not here: this
    /// runs with the guest's store alive.
    #[test]
    fn h2_shape_invalid_conceal_rules_drop_without_failing_the_language() {
        use crate::language_host::bindings::lattice::plugin_host::language::ConcealRule;
        let mut w = wire("org", &["org"]);
        w.conceal_rules = vec![
            ConcealRule {
                pattern: r"(\[\[)([^]]+)(\]\])".to_string(),
                hide: vec![1, 3],
            },
            ConcealRule {
                pattern: "   ".to_string(),
                hide: vec![1],
            },
            ConcealRule {
                pattern: "(x)".to_string(),
                hide: vec![],
            },
            // Refused later, in lattice-syntax — the boundary has no
            // engine and must not pretend to.
            ConcealRule {
                pattern: "(unclosed".to_string(),
                hide: vec![1],
            },
        ];
        let s = validate_language(w).expect("a bad rule must not fail the language");
        assert_eq!(s.conceal_rules.len(), 2);
        assert_eq!(s.conceal_rules[0].0, r"(\[\[)([^]]+)(\]\])");
        assert_eq!(s.conceal_rules[1].0, "(unclosed");
    }

    #[test]
    fn h2_a_language_declaring_no_conceal_rules_is_unchanged() {
        assert!(spec("org", &["org"]).unwrap().conceal_rules.is_empty());
    }

    #[test]
    fn grammar_name_defaults_to_the_language_name_but_may_differ() {
        assert_eq!(spec("org", &["org"]).unwrap().grammar_name, "org");
        // The bundled `sql`/`sequel` shape: the grammar's export name is not
        // what users call the language.
        let mut w = wire("sql", &["sql"]);
        w.grammar_name = Some("sequel".to_string());
        let s = validate_language(w).expect("accepted");
        assert_eq!(s.name, "sql");
        assert_eq!(s.grammar_name, "sequel");
        // Blank means absent, not an empty export name.
        let mut w = wire("sql", &["sql"]);
        w.grammar_name = Some("  ".to_string());
        assert_eq!(validate_language(w).unwrap().grammar_name, "sql");
    }

    #[test]
    fn a_well_formed_language_converts() {
        let s = spec("org", &[".ORG", "org_archive"]).expect("accepted");
        assert_eq!(s.name, "org");
        // Dots stripped, lower-cased.
        assert_eq!(
            s.extensions,
            vec!["org".to_string(), "org_archive".to_string()]
        );
    }

    #[test]
    fn an_empty_name_is_rejected() {
        assert!(spec("", &["org"]).is_err());
        assert!(spec("   ", &["org"]).is_err());
    }

    /// The name has to match `tree_sitter_<name>`, so a name that cannot be
    /// one is caught at the boundary rather than as an obscure "no entry
    /// point" failure after a ~100 ms compile.
    #[test]
    fn a_name_that_cannot_be_a_c_symbol_is_rejected() {
        for bad in ["my lang", "org!", "org.mode", "org/mode"] {
            assert!(spec(bad, &["org"]).is_err(), "{bad} should be rejected");
        }
        for good in ["org", "org_mode", "tree-sitter-org", "c99"] {
            assert!(spec(good, &["x"]).is_ok(), "{good} should be accepted");
        }
    }

    #[test]
    fn a_language_with_no_usable_extensions_is_rejected() {
        assert!(spec("org", &[]).is_err());
        // A lone dot and blanks normalise away to nothing, which is the same
        // mistake spelled differently.
        assert!(spec("org", &[".", "  "]).is_err());
    }

    #[test]
    fn an_empty_grammar_is_rejected() {
        let mut w = wire("org", &["org"]);
        w.grammar = Vec::new();
        assert!(validate_language(w).is_err());
    }

    /// A blank query means the same thing as an absent one — the feature is
    /// unavailable — rather than compiling to an empty query that matches
    /// nothing and looks like a broken highlighter.
    #[test]
    fn blank_query_sources_normalise_to_absent() {
        let mut w = wire("org", &["org"]);
        w.highlights = Some("   \n\t ".to_string());
        w.folds = Some(String::new());
        w.textobjects = Some("(x) @y".to_string());
        let s = validate_language(w).expect("accepted");
        assert_eq!(s.highlights, None);
        assert_eq!(s.folds, None);
        assert_eq!(s.textobjects.as_deref(), Some("(x) @y"));
    }
}
