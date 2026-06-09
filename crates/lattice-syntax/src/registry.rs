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

// Symbol queries -- one per language that supports the
// `gen:tree-sitter-symbol` insert-completion source (Phase
// 4.2.g.6 (1/2)). Markdown / markdown_inline don't ship one;
// prose has no symbols-to-define semantic in this sense.
const RUST_SYMBOLS_QUERY: &str = include_str!("../queries/rust/symbols.scm");
const PYTHON_SYMBOLS_QUERY: &str = include_str!("../queries/python/symbols.scm");
const JAVASCRIPT_SYMBOLS_QUERY: &str = include_str!("../queries/javascript/symbols.scm");

// Text-object queries -- one per language that supports narrow-mode's
// tree-sitter targets (`:narrow-function` / `:narrow-class` /
// `:narrow-block`, N.1.3) and the future text-object operator set.
// Capture names follow the nvim-treesitter / Helix convention
// (`@function.outer`, `@class.outer`, `@block.outer`).
// `SyntaxSnapshot::scope_at_cursor` reads them. Markdown ships none
// (prose has no function/class/block semantics).
const RUST_TEXTOBJECTS_QUERY: &str = include_str!("../queries/rust/textobjects.scm");
const PYTHON_TEXTOBJECTS_QUERY: &str = include_str!("../queries/python/textobjects.scm");
const JAVASCRIPT_TEXTOBJECTS_QUERY: &str =
    include_str!("../queries/javascript/textobjects.scm");

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
    /// Compiled `folds.scm` query, when the language ships one.
    /// `None` means the syntax fold provider falls through to the
    /// indent / markdown cascades for buffers in this language --
    /// not every language we register has folds.scm yet (e.g.
    /// `markdown_inline`, which is purely inline content).
    pub(crate) folds: Option<Query>,
    /// Compiled `highlights.scm` query for the hand-rolled native
    /// highlighter (Step 3 of Option B). Same query string the
    /// legacy `HighlightConfiguration` consumes, but compiled
    /// here so we can run it directly via `QueryCursor` without
    /// going through the streaming-event API.
    pub(crate) highlights: Query,
    /// Pre-resolved style for each capture index in `highlights`.
    /// Indexed by `cap.index as usize`. Out-of-range values fall
    /// back to `Style::Default` at lookup time.
    pub(crate) highlight_styles: Vec<crate::style::Style>,
    /// Pre-resolved CAPTURE_NAMES priority for each capture index in
    /// `highlights` (lower value = higher precedence on overlap).
    /// Used by the native pipeline's tie-break logic so e.g.
    /// `@function` wins over `@variable` on the same byte range.
    pub(crate) highlight_priorities: Vec<u32>,
    /// Compiled `injections.scm` query, when the language ships one.
    /// Markdown's block grammar is the canonical user (block →
    /// inline + fenced code blocks → embedded language); some
    /// languages (rust, JS) include light-weight injections too.
    pub(crate) injections: Option<Query>,
    /// Compiled `symbols.scm` query (Phase 4.2.g.6 (1/2)) --
    /// captures definition-position identifiers the
    /// `gen:tree-sitter-symbol` insert-completion source
    /// surfaces. `None` for languages that don't ship a
    /// symbols query (markdown family, currently); the source
    /// emits no candidates for those buffers.
    pub(crate) symbols: Option<Query>,
    /// Compiled `textobjects.scm` query (N.1.0) -- captures
    /// whole-construct (`@*.outer`) tree-sitter text objects that
    /// narrow-mode targets via [`crate::SyntaxSnapshot::scope_at_cursor`].
    /// `None` for languages that don't ship one (markdown family);
    /// `scope_at_cursor` returns `None` for those buffers.
    pub(crate) textobjects: Option<Query>,
}

/// Catalog of every supported language's parser + highlight + injection
/// configuration. Construct once via [`Self::standard`] and share by
/// `Arc` clone.
///
/// `Default` produces an empty registry -- used by
/// `lattice_host::editor::Editor::default()` for headless / placeholder
/// construction. Production uses [`Self::standard`].
#[derive(Default)]
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
                Some(RUST_SYMBOLS_QUERY),
                Some(RUST_TEXTOBJECTS_QUERY),
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
                Some(PYTHON_SYMBOLS_QUERY),
                Some(PYTHON_TEXTOBJECTS_QUERY),
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
                Some(JAVASCRIPT_SYMBOLS_QUERY),
                Some(JAVASCRIPT_TEXTOBJECTS_QUERY),
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
                None,
                None,
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
                None,
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

    /// Compiled `highlights.scm` query for the native pipeline.
    pub fn highlights_query(&self, name: &str) -> Option<&Query> {
        self.lookup(name).map(|c| &c.highlights)
    }

    /// Per-capture `Style` table aligned with `highlights_query`'s
    /// `capture_names()`.
    pub fn highlight_styles(&self, name: &str) -> Option<&[crate::style::Style]> {
        self.lookup(name).map(|c| c.highlight_styles.as_slice())
    }

    /// Per-capture priority table (CAPTURE_NAMES position) aligned
    /// with `highlights_query`'s `capture_names()`. Lower = higher
    /// precedence on overlap.
    pub fn highlight_priorities(&self, name: &str) -> Option<&[u32]> {
        self.lookup(name).map(|c| c.highlight_priorities.as_slice())
    }

    /// Compiled `injections.scm` query, when one is registered.
    pub fn injections_query(&self, name: &str) -> Option<&Query> {
        self.lookup(name).and_then(|c| c.injections.as_ref())
    }

    /// Compiled `symbols.scm` query for the
    /// `gen:tree-sitter-symbol` insert-completion source
    /// (Phase 4.2.g.6 (1/2)). `None` for languages that don't
    /// ship a symbols query (markdown family today).
    pub fn symbols_query(&self, name: &str) -> Option<&Query> {
        self.lookup(name).and_then(|c| c.symbols.as_ref())
    }

    /// Compiled `textobjects.scm` query for `name` (N.1.0), when the
    /// language ships one. Used by
    /// [`crate::SyntaxSnapshot::scope_at_cursor`] to resolve narrow-mode's
    /// tree-sitter targets (`:narrow-function` / `:narrow-class` /
    /// `:narrow-block`). `None` for languages without a text-object
    /// query (markdown family today). Resolves the same aliases as the
    /// other per-query lookups.
    pub fn textobjects_query(&self, name: &str) -> Option<&Query> {
        self.lookup(name).and_then(|c| c.textobjects.as_ref())
    }

    /// Resolve the `tree_sitter::Language` for `name` (with the same
    /// alias mapping as the other per-query lookups). Returned by value because
    /// `Language` is a cheap `Arc`-equivalent handle in tree-sitter
    /// 0.24+.
    pub fn tree_sitter_language(&self, name: &str) -> Option<Language> {
        self.lookup(name).map(|c| c.language.clone())
    }

    /// Internal lookup that returns the full per-language config
    /// (includes the raw `Language` plus -- after Step 2 -- the
    /// compiled folds / textobjects / indents queries).
    pub(crate) fn lookup(&self, name: &str) -> Option<&LangConfig> {
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
        self.lookup(name).is_some()
    }

    /// Iterate the registered language names. Useful for tests + the
    /// future `:describe-mode` content.
    pub fn lang_names(&self) -> impl Iterator<Item = &&'static str> {
        self.configs.keys()
    }
}

// Crate-private per-language config builder. The positional arg list
// grows by one optional query source per query type (folds, symbols,
// textobjects, ...); past the clippy default of 7 it's still clearer
// here than threading a builder struct through five call sites.
#[allow(clippy::too_many_arguments)]
fn build_config(
    language: tree_sitter::Language,
    name: &str,
    highlights: &str,
    injections: &str,
    locals: &str,
    folds: Option<&str>,
    symbols: Option<&str>,
    textobjects: Option<&str>,
) -> Result<LangConfig, SyntaxError> {
    let folds = match folds {
        Some(src) => Some(
            Query::new(&language, src)
                .map_err(|e| SyntaxError::Language(format!("compile {name} folds.scm: {e}")))?,
        ),
        None => None,
    };
    let highlights_query = Query::new(&language, highlights)
        .map_err(|e| SyntaxError::Language(format!("compile {name} highlights.scm: {e}")))?;
    let highlight_styles: Vec<crate::style::Style> = highlights_query
        .capture_names()
        .iter()
        .map(|n| crate::style::name_to_style_pub(n))
        .collect();
    let highlight_priorities: Vec<u32> = highlights_query
        .capture_names()
        .iter()
        .map(|n| crate::style::capture_priority(n))
        .collect();
    let injections =
        if injections.is_empty() {
            None
        } else {
            Some(Query::new(&language, injections).map_err(|e| {
                SyntaxError::Language(format!("compile {name} injections.scm: {e}"))
            })?)
        };
    let symbols = match symbols {
        Some(src) => Some(
            Query::new(&language, src)
                .map_err(|e| SyntaxError::Language(format!("compile {name} symbols.scm: {e}")))?,
        ),
        None => None,
    };
    let textobjects = match textobjects {
        Some(src) => Some(Query::new(&language, src).map_err(|e| {
            SyntaxError::Language(format!("compile {name} textobjects.scm: {e}"))
        })?),
        None => None,
    };
    let _ = locals; // locals.scm support deferred to a follow-up commit.
    Ok(LangConfig {
        language,
        folds,
        highlights: highlights_query,
        highlight_styles,
        highlight_priorities,
        injections,
        symbols,
        textobjects,
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
        assert!(
            r.folds_query("javascript").is_some(),
            "javascript folds.scm"
        );
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
    fn textobjects_query_compiled_for_each_code_language() {
        // N.1.0: every code language ships a textobjects.scm; if any
        // capture references an invalid node kind, `Query::new` errors
        // at `standard()` construction and `.unwrap()` panics here.
        let r = LangRegistry::standard().unwrap();
        assert!(r.textobjects_query("rust").is_some(), "rust textobjects");
        assert!(
            r.textobjects_query("python").is_some(),
            "python textobjects"
        );
        assert!(
            r.textobjects_query("javascript").is_some(),
            "javascript textobjects"
        );
        // Prose has no function/class/block semantics -- no query.
        assert!(
            r.textobjects_query("markdown").is_none(),
            "markdown ships no textobjects.scm"
        );
        assert!(
            r.textobjects_query("markdown_inline").is_none(),
            "markdown_inline ships no textobjects.scm"
        );
    }

    #[test]
    fn textobjects_query_resolves_aliases() {
        let r = LangRegistry::standard().unwrap();
        assert!(r.textobjects_query("rs").is_some());
        assert!(r.textobjects_query("py").is_some());
        assert!(r.textobjects_query("js").is_some());
    }

    #[test]
    fn unregistered_language_returns_none() {
        let r = LangRegistry::standard().unwrap();
        assert!(!r.has_lang("zig"));
        assert!(!r.has_lang(""));
    }
}
