//! Process-wide registry of `HighlightConfiguration`s, keyed by language
//! name. Owned via `Arc` so every `Syntax` instance holds a cheap clone
//! and the injection callback in `Highlighter::highlight` can look up
//! sibling configs (markdown block → markdown_inline; markdown's fenced
//! code blocks → rust / python / javascript / etc.) without each
//! `Syntax` carrying its own copy.
//!
//! ## Why an `Arc` registry instead of per-document configs
//!
//! `HighlightConfiguration::new` does the expensive setup (compiling
//! the highlights and injections queries against the language). Per-
//! document copies would multiply that cost and inflate memory. The
//! registry constructs each config once at startup; `Syntax` instances
//! borrow refs through the shared `Arc<LangRegistry>`.
//!
//! The injection callback that `Highlighter::highlight` takes returns
//! `Option<&'a HighlightConfiguration>` where `'a` is the highlighter's
//! borrow lifetime. Closing the callback over `&self.registry` lets the
//! callback return refs that live for the highlight call.

use std::collections::HashMap;
use std::sync::Arc;

use tree_sitter::{Language, Query};
use tree_sitter_highlight::HighlightConfiguration;

use crate::style::CAPTURE_NAMES;
use crate::syntax::SyntaxError;

// Embedded folds.scm queries. Files live at
// `crates/lattice-syntax/queries/<lang>/folds.scm`; we ship them in
// the binary via `include_str!` (no runtime path lookup, parity with
// the embedded HIGHLIGHTS_QUERY constants exposed by the
// tree-sitter-* crates).
const RUST_FOLDS_QUERY: &str = include_str!("../queries/rust/folds.scm");
const PYTHON_FOLDS_QUERY: &str = include_str!("../queries/python/folds.scm");
const JAVASCRIPT_FOLDS_QUERY: &str = include_str!("../queries/javascript/folds.scm");
const MARKDOWN_FOLDS_QUERY: &str = include_str!("../queries/markdown/folds.scm");

/// Per-language compiled state held by the shared registry.
///
/// Phase note: `highlight` is the legacy `tree_sitter_highlight`
/// configuration that the streaming highlighter consumes; it stays
/// in place during Step 1. `language` is the raw `tree_sitter::Language`
/// that downstream owners (`Syntax::parse`, future query consumers
/// like `folds.scm`, `textobjects.scm`, `indents.scm`) feed into
/// their own `Parser` instances. Step 2 adds optional compiled
/// queries (folds, etc.) to this struct without touching the
/// highlight field.
pub(crate) struct LangConfig {
    pub(crate) language: Language,
    pub(crate) highlight: HighlightConfiguration,
    /// Compiled `folds.scm` query, when the language ships one.
    /// `None` means the syntax fold provider falls through to the
    /// indent / markdown cascades for buffers in this language --
    /// not every language we register has folds.scm yet (e.g.
    /// `markdown_inline`, which is purely inline content).
    pub(crate) folds: Option<Query>,
}

/// Catalog of every supported language's parser + highlight + injection
/// configuration. Construct once via [`Self::standard`] and share by
/// `Arc` clone.
pub struct LangRegistry {
    configs: HashMap<&'static str, LangConfig>,
}

impl std::fmt::Debug for LangRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LangRegistry")
            .field("languages", &self.configs.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl LangRegistry {
    /// Build the standard registry: rust, python, javascript, markdown
    /// (block + inline). Returns an `Arc` so the App / multiple
    /// `Syntax` instances can share one allocation.
    pub fn standard() -> Result<Arc<Self>, SyntaxError> {
        let mut configs: HashMap<&'static str, LangConfig> = HashMap::new();

        configs.insert(
            "rust",
            build_config(
                tree_sitter_rust::LANGUAGE.into(),
                "rust",
                tree_sitter_rust::HIGHLIGHTS_QUERY,
                tree_sitter_rust::INJECTIONS_QUERY,
                "",
                Some(RUST_FOLDS_QUERY),
            )?,
        );
        configs.insert(
            "python",
            build_config(
                tree_sitter_python::LANGUAGE.into(),
                "python",
                tree_sitter_python::HIGHLIGHTS_QUERY,
                "",
                "",
                Some(PYTHON_FOLDS_QUERY),
            )?,
        );
        configs.insert(
            "javascript",
            build_config(
                tree_sitter_javascript::LANGUAGE.into(),
                "javascript",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::INJECTIONS_QUERY,
                tree_sitter_javascript::LOCALS_QUERY,
                Some(JAVASCRIPT_FOLDS_QUERY),
            )?,
        );

        // Markdown: dual-grammar split. The block parser handles
        // headings / lists / fenced blocks / blockquotes; its
        // injections.scm wires the inline parser into paragraph
        // content (`(inline) @injection.content (#set! injection.language "markdown_inline")`).
        // The block grammar's injections.scm also drives fenced code
        // blocks via `(fenced_code_block (info_string (language) @injection.language) (code_fence_content) @injection.content)`,
        // so a `\`\`\`rust ... \`\`\`` block recurses into the rust
        // config in this same registry.
        configs.insert(
            "markdown",
            build_config(
                tree_sitter_md::LANGUAGE.into(),
                "markdown",
                tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
                tree_sitter_md::INJECTION_QUERY_BLOCK,
                "",
                Some(MARKDOWN_FOLDS_QUERY),
            )?,
        );
        configs.insert(
            "markdown_inline",
            build_config(
                tree_sitter_md::INLINE_LANGUAGE.into(),
                "markdown_inline",
                tree_sitter_md::HIGHLIGHT_QUERY_INLINE,
                tree_sitter_md::INJECTION_QUERY_INLINE,
                "",
                None,
            )?,
        );

        Ok(Arc::new(Self { configs }))
    }

    /// The compiled `folds.scm` `Query` for `name`, when the language
    /// ships one. Used by `lattice-ui-tui::folds::compute_syntax_folds`.
    pub fn folds_query(&self, name: &str) -> Option<&Query> {
        self.lookup(name).and_then(|c| c.folds.as_ref())
    }

    /// Resolve the `tree_sitter::Language` for `name` (with the same
    /// alias mapping as [`Self::config`]). Returned by value because
    /// `Language` is a cheap `Arc`-equivalent handle in tree-sitter
    /// 0.24+.
    pub fn tree_sitter_language(&self, name: &str) -> Option<Language> {
        self.lookup(name).map(|c| c.language.clone())
    }

    /// Look up a config by language name as it appears in tree-sitter
    /// queries (the `(language)` capture in `(fenced_code_block ...)`,
    /// or the `injection.language` `#set!` value). Returns `None` for
    /// unregistered languages -- the highlighter then leaves the span
    /// unhighlighted, which is the correct fallback.
    ///
    /// Aliases are folded here so users can write the language tag
    /// they expect (`rs` ≡ `rust`, `py` ≡ `python`, `js` ≡
    /// `javascript`, `md` ≡ `markdown`).
    pub fn config(&self, name: &str) -> Option<&HighlightConfiguration> {
        self.lookup(name).map(|c| &c.highlight)
    }

    /// Internal lookup that returns the full per-language config
    /// (includes the raw `Language` plus -- after Step 2 -- the
    /// compiled folds / textobjects / indents queries).
    fn lookup(&self, name: &str) -> Option<&LangConfig> {
        let canonical = match name {
            "rs" => "rust",
            "py" => "python",
            "js" | "mjs" | "cjs" => "javascript",
            "md" => "markdown",
            other => other,
        };
        self.configs.get(canonical)
    }

    /// True if a config exists for the given canonical language name.
    pub fn has_lang(&self, name: &str) -> bool {
        self.config(name).is_some()
    }

    /// Iterate the registered language names. Useful for tests + the
    /// future `:describe-mode` content.
    pub fn lang_names(&self) -> impl Iterator<Item = &&'static str> {
        self.configs.keys()
    }
}

fn build_config(
    language: tree_sitter::Language,
    name: &str,
    highlights: &str,
    injections: &str,
    locals: &str,
    folds: Option<&str>,
) -> Result<LangConfig, SyntaxError> {
    let mut highlight =
        HighlightConfiguration::new(language.clone(), name, highlights, injections, locals)
            .map_err(|e| SyntaxError::Language(e.to_string()))?;
    highlight.configure(CAPTURE_NAMES);
    let folds = match folds {
        Some(src) => Some(
            Query::new(&language, src).map_err(|e| {
                SyntaxError::Language(format!("compile {name} folds.scm: {e}"))
            })?,
        ),
        None => None,
    };
    Ok(LangConfig {
        language,
        highlight,
        folds,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn standard_registry_includes_every_supported_language() {
        let r = LangRegistry::standard().unwrap();
        assert!(r.has_lang("rust"));
        assert!(r.has_lang("python"));
        assert!(r.has_lang("javascript"));
        assert!(r.has_lang("markdown"));
        assert!(r.has_lang("markdown_inline"));
    }

    #[test]
    fn registry_resolves_common_aliases() {
        let r = LangRegistry::standard().unwrap();
        assert!(r.has_lang("rs"));
        assert!(r.has_lang("py"));
        assert!(r.has_lang("js"));
        assert!(r.has_lang("md"));
    }

    #[test]
    fn folds_query_compiled_for_each_language_with_folds_scm() {
        let r = LangRegistry::standard().unwrap();
        assert!(r.folds_query("rust").is_some(), "rust folds.scm");
        assert!(r.folds_query("python").is_some(), "python folds.scm");
        assert!(r.folds_query("javascript").is_some(), "javascript folds.scm");
        assert!(r.folds_query("markdown").is_some(), "markdown folds.scm");
        // Inline grammar is purely inline content; no folds.scm is
        // appropriate. The block grammar handles markdown folding.
        assert!(
            r.folds_query("markdown_inline").is_none(),
            "markdown_inline should not ship a folds.scm"
        );
    }

    #[test]
    fn folds_query_resolves_aliases() {
        let r = LangRegistry::standard().unwrap();
        assert!(r.folds_query("rs").is_some());
        assert!(r.folds_query("py").is_some());
        assert!(r.folds_query("js").is_some());
        assert!(r.folds_query("md").is_some());
    }

    #[test]
    fn unregistered_language_returns_none() {
        let r = LangRegistry::standard().unwrap();
        assert!(r.config("zig").is_none());
        assert!(r.config("").is_none());
    }
}
