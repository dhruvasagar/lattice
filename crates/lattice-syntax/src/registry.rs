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
use std::sync::{Arc, OnceLock};

use arc_swap::ArcSwap;

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
// Custom markdown (block) highlights — the bundled tree-sitter-md query
// captures headings level-less (`@text.title`); this one distinguishes
// `@text.title.1` … `.6` by atx marker so headings get per-level size +
// colour (see the query file's header + style.rs `text.title.N`).
const MARKDOWN_HIGHLIGHTS_QUERY: &str = include_str!("../queries/markdown/highlights.scm");

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
const JAVASCRIPT_TEXTOBJECTS_QUERY: &str = include_str!("../queries/javascript/textobjects.scm");

// ── 15 additional Tier-1 language queries ───────────────────────
const BASH_FOLDS_QUERY: &str = include_str!("../queries/bash/folds.scm");
const BASH_SYMBOLS_QUERY: &str = include_str!("../queries/bash/symbols.scm");
const BASH_TEXTOBJECTS_QUERY: &str = include_str!("../queries/bash/textobjects.scm");

const C_FOLDS_QUERY: &str = include_str!("../queries/c/folds.scm");
const C_SYMBOLS_QUERY: &str = include_str!("../queries/c/symbols.scm");
const C_TEXTOBJECTS_QUERY: &str = include_str!("../queries/c/textobjects.scm");

const CPP_FOLDS_QUERY: &str = include_str!("../queries/cpp/folds.scm");
const CPP_SYMBOLS_QUERY: &str = include_str!("../queries/cpp/symbols.scm");
const CPP_TEXTOBJECTS_QUERY: &str = include_str!("../queries/cpp/textobjects.scm");

const CSS_FOLDS_QUERY: &str = include_str!("../queries/css/folds.scm");
const CSS_SYMBOLS_QUERY: &str = include_str!("../queries/css/symbols.scm");
const CSS_TEXTOBJECTS_QUERY: &str = include_str!("../queries/css/textobjects.scm");

const GO_FOLDS_QUERY: &str = include_str!("../queries/go/folds.scm");
const GO_SYMBOLS_QUERY: &str = include_str!("../queries/go/symbols.scm");
const GO_TEXTOBJECTS_QUERY: &str = include_str!("../queries/go/textobjects.scm");

const HTML_FOLDS_QUERY: &str = include_str!("../queries/html/folds.scm");
const HTML_SYMBOLS_QUERY: &str = include_str!("../queries/html/symbols.scm");
const HTML_TEXTOBJECTS_QUERY: &str = include_str!("../queries/html/textobjects.scm");

const JAVA_FOLDS_QUERY: &str = include_str!("../queries/java/folds.scm");
const JAVA_SYMBOLS_QUERY: &str = include_str!("../queries/java/symbols.scm");
const JAVA_TEXTOBJECTS_QUERY: &str = include_str!("../queries/java/textobjects.scm");

const JSON_FOLDS_QUERY: &str = include_str!("../queries/json/folds.scm");
const JSON_SYMBOLS_QUERY: &str = include_str!("../queries/json/symbols.scm");
const JSON_TEXTOBJECTS_QUERY: &str = include_str!("../queries/json/textobjects.scm");

const LUA_FOLDS_QUERY: &str = include_str!("../queries/lua/folds.scm");
const LUA_SYMBOLS_QUERY: &str = include_str!("../queries/lua/symbols.scm");
const LUA_TEXTOBJECTS_QUERY: &str = include_str!("../queries/lua/textobjects.scm");

const RUBY_FOLDS_QUERY: &str = include_str!("../queries/ruby/folds.scm");
const RUBY_SYMBOLS_QUERY: &str = include_str!("../queries/ruby/symbols.scm");
const RUBY_TEXTOBJECTS_QUERY: &str = include_str!("../queries/ruby/textobjects.scm");

const SQL_FOLDS_QUERY: &str = include_str!("../queries/sql/folds.scm");
const SQL_SYMBOLS_QUERY: &str = include_str!("../queries/sql/symbols.scm");
const SQL_TEXTOBJECTS_QUERY: &str = include_str!("../queries/sql/textobjects.scm");

const WIT_HIGHLIGHTS_QUERY: &str = include_str!("../queries/wit/highlights.scm");
const WIT_FOLDS_QUERY: &str = include_str!("../queries/wit/folds.scm");
const TOML_FOLDS_QUERY: &str = include_str!("../queries/toml/folds.scm");
const TOML_SYMBOLS_QUERY: &str = include_str!("../queries/toml/symbols.scm");
const TOML_TEXTOBJECTS_QUERY: &str = include_str!("../queries/toml/textobjects.scm");

const TYPESCRIPT_FOLDS_QUERY: &str = include_str!("../queries/typescript/folds.scm");
const TYPESCRIPT_SYMBOLS_QUERY: &str = include_str!("../queries/typescript/symbols.scm");
const TYPESCRIPT_TEXTOBJECTS_QUERY: &str = include_str!("../queries/typescript/textobjects.scm");

const YAML_FOLDS_QUERY: &str = include_str!("../queries/yaml/folds.scm");
const YAML_SYMBOLS_QUERY: &str = include_str!("../queries/yaml/symbols.scm");
const YAML_TEXTOBJECTS_QUERY: &str = include_str!("../queries/yaml/textobjects.scm");

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
    /// IN.2: compiled `indents.scm` query, when the language ships
    /// one. `None` means predictive indent falls back to the lexical
    /// bridge for buffers in this language -- a graceful degradation
    /// to vim's `smartindent`, not a failure.
    ///
    /// Unlike its siblings above, the SOURCE is not threaded through
    /// `build_config`'s parameter list; it is looked up by language
    /// name from `crate::indent::indents_source`. `build_config`
    /// already carries eight positional arguments and a
    /// `too_many_arguments` lint to match, so a ninth `Option<&str>`
    /// repeated across twenty call sites is the wrong direction. The
    /// name-keyed table also keeps the `include_str!` beside the
    /// engine that consumes it.
    pub(crate) indents: Option<Query>,
    /// LG.3a: the host-issued id of the plugin that contributed this
    /// language, or `None` for one compiled into the editor. Teardown
    /// is `retain(|c| c.provenance != Some(id))` — by provenance, not by
    /// a token list the caller has to remember to keep.
    pub(crate) provenance: Option<u64>,
    /// H.2: compiled display-time elision rules
    /// (`docs/dev/architecture/conceal.md`). Empty for every language
    /// that declares none, which is every language today except one —
    /// and the emptiness is what makes the zero-cost path zero-cost.
    ///
    /// Lives here rather than on `LanguageRegistration` because this is
    /// per-language *render* config, the same shelf `highlights` sits
    /// on. That also means a NATIVE language gains rules by populating
    /// one field in `build_native_config`, with no new plumbing —
    /// markdown's `**`/`[]()` set is a rule list, not a mechanism.
    pub(crate) conceal_rules: Arc<[crate::conceal::ConcealRule]>,
}

/// Catalog of every supported language's parser + highlight + injection
/// configuration. Construct once via [`Self::standard`] and share by
/// `Arc` clone.
///
/// `Default` produces an empty registry -- used by
/// `lattice_host::editor::Editor::default()` for headless / placeholder
/// construction. Production uses [`Self::standard`].
/// `Clone` is cheap by construction: each config sits behind an `Arc`, so
/// a clone is one refcount bump per language plus a small map. That is
/// what makes the copy-on-write registration path affordable — compiling
/// the queries for ~19 languages costs ~1.2 s and must never be repeated
/// just because a plugin registered a twentieth.
#[derive(Default, Clone)]
pub struct LangRegistry {
    configs: HashMap<&'static str, Arc<LangConfig>>,
}

impl std::fmt::Debug for LangRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LangRegistry")
            .field("languages", &self.configs.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl LangRegistry {
    /// The standard registry: rust, python, javascript, markdown
    /// (block + inline) and the rest. Returns an `Arc` so the App /
    /// multiple `Syntax` instances can share one allocation.
    ///
    /// **Memoised process-wide.** Building it compiles five tree-sitter
    /// queries (highlights, injections, folds, symbols, textobjects)
    /// for each of ~19 languages, which measured at **~1.2 s** — and
    /// `Editor::boot` calls this every time. In the test suite that was
    /// the dominant cost of the entire crate: one App-building test
    /// took 1.28 s against 0.01 s for a test that builds no App, and
    /// `lattice-ui-tui`'s 1683 tests took 779 s almost entirely on
    /// this. In production it is paid at startup, to open one file.
    ///
    /// Sharing is safe by construction rather than by convention: a
    /// `LangRegistry` has no interior mutability and no `&mut self`
    /// method, so a snapshot cannot be observed to change under its
    /// holder. **Which snapshot you get can change** — LG.3a made this
    /// the live RCU value so a plugin language appears in the same map
    /// bundled ones live in, with no call-site change and no kind-branch
    /// anywhere. Registration replaces the value; holders of an older
    /// `Arc` keep a coherent older view until they next call this.
    ///
    /// See [`live`], [`register_plugin_language`] and
    /// [`unregister_plugin`].
    ///
    /// Only success is cached. A failure here means a static query
    /// string does not compile, which is deterministic and fatal (every
    /// caller `expect`s it), so re-deriving it costs nothing real and
    /// avoids requiring `SyntaxError: Clone`.
    pub fn standard() -> Result<Arc<Self>, SyntaxError> {
        live()
    }

    /// The uncached construction of the bundled set. Separated so the
    /// live handle can build it exactly once; not public, because every
    /// caller wants the shared, live one.
    fn build_standard() -> Result<Self, SyntaxError> {
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
                MARKDOWN_HIGHLIGHTS_QUERY,
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

        // ── 15 additional Tier-1 languages ───────────────────────
        configs.insert(
            "bash",
            build_config(
                tree_sitter_bash::LANGUAGE.into(),
                "bash",
                tree_sitter_bash::HIGHLIGHT_QUERY,
                "",
                "",
                Some(BASH_FOLDS_QUERY),
                Some(BASH_SYMBOLS_QUERY),
                Some(BASH_TEXTOBJECTS_QUERY),
            )?,
        );
        configs.insert(
            "c",
            build_config(
                tree_sitter_c::LANGUAGE.into(),
                "c",
                tree_sitter_c::HIGHLIGHT_QUERY,
                "",
                "",
                Some(C_FOLDS_QUERY),
                Some(C_SYMBOLS_QUERY),
                Some(C_TEXTOBJECTS_QUERY),
            )?,
        );
        configs.insert(
            "cpp",
            build_config(
                tree_sitter_cpp::LANGUAGE.into(),
                "cpp",
                tree_sitter_cpp::HIGHLIGHT_QUERY,
                "",
                "",
                Some(CPP_FOLDS_QUERY),
                Some(CPP_SYMBOLS_QUERY),
                Some(CPP_TEXTOBJECTS_QUERY),
            )?,
        );
        configs.insert(
            "css",
            build_config(
                tree_sitter_css::LANGUAGE.into(),
                "css",
                tree_sitter_css::HIGHLIGHTS_QUERY,
                "",
                "",
                Some(CSS_FOLDS_QUERY),
                Some(CSS_SYMBOLS_QUERY),
                Some(CSS_TEXTOBJECTS_QUERY),
            )?,
        );
        // NOTE: Dockerfile's tree-sitter grammar (0.2.0) depends on
        // tree-sitter 0.20 — a different type than our 0.26 core.
        // Skipped; Dockerfile buffers use Plain fallback.
        configs.insert(
            "go",
            build_config(
                tree_sitter_go::LANGUAGE.into(),
                "go",
                tree_sitter_go::HIGHLIGHTS_QUERY,
                "",
                "",
                Some(GO_FOLDS_QUERY),
                Some(GO_SYMBOLS_QUERY),
                Some(GO_TEXTOBJECTS_QUERY),
            )?,
        );
        configs.insert(
            "html",
            build_config(
                tree_sitter_html::LANGUAGE.into(),
                "html",
                tree_sitter_html::HIGHLIGHTS_QUERY,
                tree_sitter_html::INJECTIONS_QUERY,
                "",
                Some(HTML_FOLDS_QUERY),
                Some(HTML_SYMBOLS_QUERY),
                Some(HTML_TEXTOBJECTS_QUERY),
            )?,
        );
        configs.insert(
            "java",
            build_config(
                tree_sitter_java::LANGUAGE.into(),
                "java",
                tree_sitter_java::HIGHLIGHTS_QUERY,
                "",
                "",
                Some(JAVA_FOLDS_QUERY),
                Some(JAVA_SYMBOLS_QUERY),
                Some(JAVA_TEXTOBJECTS_QUERY),
            )?,
        );
        configs.insert(
            "json",
            build_config(
                tree_sitter_json::LANGUAGE.into(),
                "json",
                tree_sitter_json::HIGHLIGHTS_QUERY,
                "",
                "",
                Some(JSON_FOLDS_QUERY),
                Some(JSON_SYMBOLS_QUERY),
                Some(JSON_TEXTOBJECTS_QUERY),
            )?,
        );
        configs.insert(
            "lua",
            build_config(
                tree_sitter_lua::LANGUAGE.into(),
                "lua",
                tree_sitter_lua::HIGHLIGHTS_QUERY,
                "",
                "",
                Some(LUA_FOLDS_QUERY),
                Some(LUA_SYMBOLS_QUERY),
                Some(LUA_TEXTOBJECTS_QUERY),
            )?,
        );
        configs.insert(
            "ruby",
            build_config(
                tree_sitter_ruby::LANGUAGE.into(),
                "ruby",
                tree_sitter_ruby::HIGHLIGHTS_QUERY,
                "",
                "",
                Some(RUBY_FOLDS_QUERY),
                Some(RUBY_SYMBOLS_QUERY),
                Some(RUBY_TEXTOBJECTS_QUERY),
            )?,
        );
        configs.insert(
            "sql",
            build_config(
                tree_sitter_sequel::LANGUAGE.into(),
                "sql",
                tree_sitter_sequel::HIGHLIGHTS_QUERY,
                "",
                "",
                Some(SQL_FOLDS_QUERY),
                Some(SQL_SYMBOLS_QUERY),
                Some(SQL_TEXTOBJECTS_QUERY),
            )?,
        );
        // WIT. Two things differ from every sibling here, both forced:
        //   * `tree_sitter_wit` exposes `language()` — a plain fn returning a
        //     `Language` — rather than a `LanguageFn` constant, so there is no
        //     `.into()`.
        //   * the highlights query is OURS (`queries/wit/highlights.scm`). The
        //     crate ships one, but in the TextMate capture vocabulary, and
        //     `style::name_to_style` keys on the first dot-segment — so
        //     `entity.name.type.interface` and its peers resolve to
        //     `Style::Default` and every declaration NAME would render plain.
        configs.insert(
            "wit",
            build_config(
                tree_sitter_wit::language(),
                "wit",
                WIT_HIGHLIGHTS_QUERY,
                "",
                "",
                Some(WIT_FOLDS_QUERY),
                None,
                None,
            )?,
        );
        configs.insert(
            "toml",
            build_config(
                tree_sitter_toml_ng::LANGUAGE.into(),
                "toml",
                tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
                "",
                "",
                Some(TOML_FOLDS_QUERY),
                Some(TOML_SYMBOLS_QUERY),
                Some(TOML_TEXTOBJECTS_QUERY),
            )?,
        );
        // NOTE: tree-sitter-typescript exposes two grammars:
        // LANGUAGE_TYPESCRIPT (strict TS) and LANGUAGE_TSX (TS+JSX).
        // We register "typescript" with the TS grammar; see "tsx"
        // for the TSX variant.
        configs.insert(
            "typescript",
            build_config(
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                "typescript",
                tree_sitter_typescript::HIGHLIGHTS_QUERY,
                "",
                tree_sitter_typescript::LOCALS_QUERY,
                Some(TYPESCRIPT_FOLDS_QUERY),
                Some(TYPESCRIPT_SYMBOLS_QUERY),
                Some(TYPESCRIPT_TEXTOBJECTS_QUERY),
            )?,
        );
        // TSX grammar (TypeScript + JSX) — separate language object
        // but shares the same query files (the TSX grammar is a
        // superset of the TypeScript grammar so all node type names
        // valid in TS are valid in TSX, and TSX-only constructs will
        // simply produce no captures if the query doesn't reference
        // them).
        configs.insert(
            "tsx",
            build_config(
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                "tsx",
                tree_sitter_typescript::HIGHLIGHTS_QUERY,
                "",
                tree_sitter_typescript::LOCALS_QUERY,
                Some(TYPESCRIPT_FOLDS_QUERY),
                Some(TYPESCRIPT_SYMBOLS_QUERY),
                Some(TYPESCRIPT_TEXTOBJECTS_QUERY),
            )?,
        );
        configs.insert(
            "yaml",
            build_config(
                tree_sitter_yaml::LANGUAGE.into(),
                "yaml",
                tree_sitter_yaml::HIGHLIGHTS_QUERY,
                "",
                "",
                Some(YAML_FOLDS_QUERY),
                Some(YAML_SYMBOLS_QUERY),
                Some(YAML_TEXTOBJECTS_QUERY),
            )?,
        );

        // Wrapped here rather than at each of the twenty `insert` sites:
        // the `Arc` exists to make `Clone` cheap, and nothing about
        // *building* the standard set cares.
        Ok(Self {
            configs: configs.into_iter().map(|(k, v)| (k, Arc::new(v))).collect(),
        })
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

    /// IN.2: the compiled `indents.scm` query for `name`, when the
    /// language ships one. Consumed by `crate::indent`.
    pub fn indents_query(&self, name: &str) -> Option<&Query> {
        self.lookup(name).and_then(|c| c.indents.as_ref())
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

    /// H.2: this language's compiled conceal rules, or an empty slice.
    ///
    /// Returns the `Arc` rather than a borrow so the display-matrix
    /// build can hold the rules across a rebuild without pinning the
    /// whole registry snapshot, and so a plugin unloading mid-rebuild
    /// cannot pull them out from under it.
    ///
    /// **Empty is the answer for every language but one**, and callers
    /// are expected to branch on that before doing any per-line work —
    /// the conceal path must cost a Rust buffer nothing at all.
    pub fn conceal_rules(&self, name: &str) -> Arc<[crate::conceal::ConcealRule]> {
        self.lookup(name)
            .map(|c| Arc::clone(&c.conceal_rules))
            .unwrap_or_else(|| Arc::from([] as [crate::conceal::ConcealRule; 0]))
    }

    /// Internal lookup that returns the full per-language config
    /// (includes the raw `Language` plus -- after Step 2 -- the
    /// compiled folds / textobjects / indents queries).
    pub(crate) fn lookup(&self, name: &str) -> Option<&LangConfig> {
        let canonical = match name {
            "rs" => "rust",
            "py" => "python",
            "js" | "mjs" | "cjs" => "javascript",
            "sh" => "bash",
            "h" => "c",
            "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
            "htm" | "xhtml" => "html",
            "rb" => "ruby",
            "ts" | "mts" | "cts" => "typescript",
            "yml" => "yaml",
            "md" => "markdown",
            other => other,
        };
        self.configs.get(canonical).map(|c| &**c)
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
    let textobjects =
        match textobjects {
            Some(src) => Some(Query::new(&language, src).map_err(|e| {
                SyntaxError::Language(format!("compile {name} textobjects.scm: {e}"))
            })?),
            None => None,
        };
    // IN.2: resolved by name rather than passed in -- see the
    // `indents` field's doc on `LangConfig`.
    let indents = match crate::indent::indents_source(name) {
        Some(src) => Some(
            Query::new(&language, src)
                .map_err(|e| SyntaxError::Language(format!("compile {name} indents.scm: {e}")))?,
        ),
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
        indents,
        provenance: None,
        conceal_rules: Arc::from([] as [crate::conceal::ConcealRule; 0]),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn standard_registry_includes_every_supported_language() {
        let r = LangRegistry::standard().unwrap();
        for name in [
            "rust",
            "python",
            "javascript",
            "markdown",
            "markdown_inline",
            "bash",
            "c",
            "cpp",
            "css",
            "go",
            "html",
            "java",
            "json",
            "lua",
            "ruby",
            "sql",
            "toml",
            "typescript",
            "tsx",
            "wit",
            "yaml",
        ] {
            assert!(r.has_lang(name), "missing language: {name}");
        }
    }

    #[test]
    fn registry_resolves_common_aliases() {
        let r = LangRegistry::standard().unwrap();
        for alias in [
            "rs", "py", "js", "md", "sh", "h", "cc", "cxx", "hpp", "hh", "hxx", "rb", "ts", "mts",
            "cts", "yml",
        ] {
            assert!(r.has_lang(alias), "alias not resolved: {alias}");
        }
    }

    #[test]
    fn folds_query_compiled_for_each_language_with_folds_scm() {
        let r = LangRegistry::standard().unwrap();
        // Every language except markdown_inline ships folds.scm.
        let with_folds = [
            "rust",
            "python",
            "javascript",
            "markdown",
            "bash",
            "c",
            "cpp",
            "css",
            "go",
            "html",
            "java",
            "json",
            "lua",
            "ruby",
            "sql",
            "toml",
            "typescript",
            "tsx",
            "wit",
            "yaml",
        ];
        for lang in with_folds {
            assert!(r.folds_query(lang).is_some(), "{lang} folds.scm");
        }
        assert!(
            r.folds_query("markdown_inline").is_none(),
            "markdown_inline should not ship a folds.scm"
        );
    }

    #[test]
    fn folds_query_resolves_aliases() {
        let r = LangRegistry::standard().unwrap();
        for alias in ["rs", "py", "js", "md", "sh", "cc", "rb", "ts", "yml"] {
            assert!(r.folds_query(alias).is_some(), "folds alias: {alias}");
        }
    }

    #[test]
    fn textobjects_query_compiled_for_each_code_language() {
        // N.1.0: every code language ships a textobjects.scm; if any
        // capture references an invalid node kind, `Query::new` errors
        // at `standard()` construction and `.unwrap()` panics here.
        let r = LangRegistry::standard().unwrap();
        let with_textobjects = [
            "rust",
            "python",
            "javascript",
            "bash",
            "c",
            "cpp",
            "css",
            "go",
            "html",
            "java",
            "json",
            "lua",
            "ruby",
            "sql",
            "toml",
            "typescript",
            "tsx",
            "yaml",
        ];
        for lang in with_textobjects {
            assert!(r.textobjects_query(lang).is_some(), "{lang} textobjects");
        }
        // Prose has no function/class/block semantics -- no query.
        for lang in ["markdown", "markdown_inline"] {
            assert!(
                r.textobjects_query(lang).is_none(),
                "{lang} ships textobjects (unexpected)"
            );
        }
    }

    #[test]
    fn textobjects_query_resolves_aliases() {
        let r = LangRegistry::standard().unwrap();
        for alias in ["rs", "py", "js", "sh", "cc", "rb", "ts", "yml"] {
            assert!(
                r.textobjects_query(alias).is_some(),
                "textobjects alias: {alias}"
            );
        }
    }

    #[test]
    fn unregistered_language_returns_none() {
        let r = LangRegistry::standard().unwrap();
        assert!(!r.has_lang("zig"));
        assert!(!r.has_lang(""));
    }

    #[test]
    fn each_tier1_language_has_highlights_query() {
        let r = LangRegistry::standard().unwrap();
        for name in r.lang_names() {
            assert!(r.highlights_query(name).is_some(), "{name} highlights.scm");
        }
    }

    #[test]
    fn each_tier1_language_has_tree_sitter_language() {
        let r = LangRegistry::standard().unwrap();
        for name in r.lang_names() {
            assert!(
                r.tree_sitter_language(name).is_some(),
                "{name} tree_sitter_language"
            );
        }
    }
}

// ── LG.3a: the live registry ────────────────────────────────────────
//
// Design: `plugin-languages.md` §2.3. The bundled set is built once and
// then becomes the initial value of an RCU cell; a plugin language is
// registered by cloning that value, inserting, and storing.
//
// `standard()` returns the LIVE snapshot, which is the whole point: a
// plugin language is found by `registry.highlights_query(lang.name())`
// exactly as `rust` is, so no lookup path anywhere in the tree learns
// that plugin languages exist. The alternative — a second map consulted
// when `Lang::Plugin` matches — would be a kind-branch in every accessor,
// which is what the architecture rules forbid.

/// The process-wide live registry.
///
/// The bundled set is built on first touch (~1.2 s of query compilation)
/// and never rebuilt; registration clones the map, not the queries.
fn handle() -> &'static Arc<ArcSwap<LangRegistry>> {
    static HANDLE: OnceLock<Arc<ArcSwap<LangRegistry>>> = OnceLock::new();
    HANDLE.get_or_init(|| {
        // A failure here means a bundled `.scm` does not compile, which
        // is deterministic and fatal — every caller of `standard()`
        // already `expect`s it. Storing an empty registry rather than
        // panicking in an initialiser keeps the error at the call site,
        // where it can name itself.
        let built = LangRegistry::build_standard().unwrap_or_default();
        Arc::new(ArcSwap::from_pointee(built))
    })
}

/// Snapshot the live registry.
///
/// Wait-free. Returns `Err` only if the bundled set failed to compile,
/// which is a bug in a checked-in query rather than anything a plugin
/// can cause.
pub fn live() -> Result<Arc<LangRegistry>, SyntaxError> {
    let snapshot = handle().load_full();
    if snapshot.configs.is_empty() {
        // Re-derive so the caller gets the real compiler diagnostic
        // rather than "empty registry".
        return LangRegistry::build_standard().map(Arc::new);
    }
    Ok(snapshot)
}

/// Everything a runtime-registered language supplies beyond its identity.
///
/// Query sources are compiled **at registration, not first use**. A
/// malformed `folds.scm` is the plugin author's error and has to surface
/// at load with the offending query named — not silently disable folding
/// three days later, which is indistinguishable from the feature not
/// existing.
#[derive(Debug, Clone)]
pub struct GrammarSpec {
    pub grammar: Language,
    /// H.2: `(pattern, hide-groups)` as the guest declared them,
    /// uncompiled. Compiled in [`compile_plugin_config`], where a
    /// refused rule is dropped and logged rather than failing the
    /// language — see `conceal.rs` for why that is asymmetric with
    /// query compilation.
    pub conceal_rules: Vec<(String, Vec<u32>)>,
    pub highlights: Option<String>,
    pub folds: Option<String>,
    pub injections: Option<String>,
    pub indents: Option<String>,
    pub textobjects: Option<String>,
}

/// Compile `spec` against `name`, WITHOUT installing anything.
///
/// Split from [`install_plugin_config`] so registration can be made
/// atomic across two registries: the caller compiles first, claims the
/// language's identity second, and installs only if both succeeded. A
/// failed query therefore leaves no trace in either — a language works
/// or is legibly absent, never half-registered.
pub(crate) fn compile_plugin_config(
    name: &'static str,
    spec: &GrammarSpec,
    provenance: u64,
) -> Result<Arc<LangConfig>, SyntaxError> {
    let compile = |what: &str, src: &str| -> Result<Query, SyntaxError> {
        Query::new(&spec.grammar, src)
            .map_err(|e| SyntaxError::Language(format!("compile {name} {what}: {e}")))
    };
    let opt = |what: &str, src: &Option<String>| -> Result<Option<Query>, SyntaxError> {
        match src {
            Some(src) if !src.trim().is_empty() => compile(what, src).map(Some),
            // Absent means "that feature is unavailable for this
            // language", never an error — the design says so explicitly.
            _ => Ok(None),
        }
    };

    // An absent highlights query is legal: the language parses, folds and
    // indents, and simply renders unstyled. An empty `Query` compiles
    // against any grammar, so this needs no special case downstream.
    let highlights = match &spec.highlights {
        Some(src) if !src.trim().is_empty() => compile("highlights.scm", src)?,
        _ => compile("highlights.scm", "")?,
    };
    let highlight_styles = highlights
        .capture_names()
        .iter()
        .map(|n| crate::style::name_to_style_pub(n))
        .collect();
    let highlight_priorities = highlights
        .capture_names()
        .iter()
        .map(|n| crate::style::capture_priority(n))
        .collect();

    // Refusals are logged HERE, once, at registration — naming the
    // language and which rule. Deferring the check to match time would
    // log at rebuild rate, which is the `debug!`-not-`info!` mistake in
    // a different costume.
    let (conceal_rules, conceal_errs) = crate::conceal::compile_rules(&spec.conceal_rules);
    for (i, e) in &conceal_errs {
        tracing::warn!(
            language = name,
            rule = i,
            "conceal rule refused, the language's other rules still apply: {e}"
        );
    }

    let config = LangConfig {
        language: spec.grammar.clone(),
        folds: opt("folds.scm", &spec.folds)?,
        highlights,
        highlight_styles,
        highlight_priorities,
        injections: opt("injections.scm", &spec.injections)?,
        // A plugin language ships no symbols query yet; the WIT spec has
        // no field for one, so this is absent rather than empty-by-choice.
        symbols: None,
        textobjects: opt("textobjects.scm", &spec.textobjects)?,
        indents: opt("indents.scm", &spec.indents)?,
        provenance: Some(provenance),
        conceal_rules: Arc::from(conceal_rules.into_boxed_slice()),
    };

    Ok(Arc::new(config))
}

/// Install an already-compiled config. Infallible by construction — all
/// the ways this can fail happened in [`compile_plugin_config`].
pub(crate) fn install_plugin_config(name: &'static str, config: Arc<LangConfig>) {
    handle().rcu(|current| {
        let mut next = LangRegistry::clone(current);
        next.configs.insert(name, Arc::clone(&config));
        next
    });
}

/// Withdraw every language contributed by `provenance`, returning how
/// many were removed. Bundled languages carry `None` and are untouchable.
pub(crate) fn unregister_plugin(provenance: u64) -> usize {
    let mut removed = 0;
    handle().rcu(|current| {
        let mut next = LangRegistry::clone(current);
        let before = next.configs.len();
        next.configs.retain(|_, c| c.provenance != Some(provenance));
        removed = before - next.configs.len();
        next
    });
    removed
}
