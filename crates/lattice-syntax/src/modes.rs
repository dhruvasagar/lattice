//! Per-language major modes.
//!
//! Each variant of [`crate::lang::Lang`] (other than `Plain`)
//! has a corresponding major mode declared here. The modes are
//! pure declarations in this slice (M.3.0) -- their option
//! contributions, keymap layers, and lifecycle hooks are
//! empty / no-op. Real declarative content (indent rules,
//! tree-sitter parser attach, default LSP attach, comment
//! syntax) lands as the corresponding subsystems migrate to
//! the mode model in later slices.
//!
//! `Plain` maps to `lattice_mode::TextMode`; no separate
//! plain-mode declaration here.
//!
//! All language modes register through
//! [`register_language_modes`].

use std::sync::Arc;

use lattice_completion::{
    CandidateKind, CompletionSourceContribution, CompletionSourceKind, InsertContext, RawCandidate,
    SourceId, SyncCompletionSource,
};
use lattice_core::BufferKind;
use lattice_mode::{
    CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, ModeRegistry,
};

use crate::lang::Lang;

/// Macro-internal helper: declare a unit struct + its `Mode`
/// impl with the canonical name. Reduces boilerplate while
/// keeping each mode's source plain Rust (no proc-macro
/// indirection for now).
///
/// Second arm (with `target_kind = $kind`) overrides
/// [`Mode::target_buffer_kind`] for language majors that are also
/// the default major for a non-`Document` [`BufferKind`] — today
/// only `markdown-mode`, which serves both `Document + Markdown`
/// (via `Lang` detection) and `Help` (via H.2's kind dispatch).
macro_rules! lang_mode {
    ($struct_name:ident, $mode_name:literal) => {
        lang_mode!(@inner $struct_name, $mode_name, None);
    };
    ($struct_name:ident, $mode_name:literal, target_kind = $kind:expr) => {
        lang_mode!(@inner $struct_name, $mode_name, Some($kind));
    };
    (@inner $struct_name:ident, $mode_name:literal, $target_kind:expr) => {
        pub struct $struct_name;

        impl $struct_name {
            pub fn mode_id() -> ModeId {
                ModeId::new($mode_name)
            }
        }

        impl Mode for $struct_name {
            type Guard = ();
            fn id(&self) -> ModeId {
                Self::mode_id()
            }
            fn kind(&self) -> ModeKind {
                ModeKind::Major
            }
            fn target_buffer_kind(&self) -> Option<BufferKind> {
                $target_kind
            }
            fn required_capabilities(&self) -> CapabilitySet {
                CapabilitySet::empty()
            }
            fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
                Box::pin(async { Ok(()) })
            }
        }
    };
}

lang_mode!(RustMode, "rust-mode");
lang_mode!(PythonMode, "python-mode");
lang_mode!(JavascriptMode, "javascript-mode");
lang_mode!(BashMode, "bash-mode");
lang_mode!(CMode, "c-mode");
lang_mode!(CppMode, "cpp-mode");
lang_mode!(CssMode, "css-mode");
lang_mode!(GoMode, "go-mode");
lang_mode!(HtmlMode, "html-mode");
lang_mode!(JavaMode, "java-mode");
lang_mode!(JsonMode, "json-mode");
lang_mode!(LuaMode, "lua-mode");
lang_mode!(RubyMode, "ruby-mode");
lang_mode!(SqlMode, "sql-mode");
lang_mode!(TomlMode, "toml-mode");
lang_mode!(TypeScriptMode, "typescript-mode");
lang_mode!(TsxMode, "tsx-mode");
lang_mode!(YamlMode, "yaml-mode");
// H.2 (2026-05-31): `markdown-mode` is the default major for two
// dispatch paths — `Document + Lang::Markdown` (via language
// detection) and `BufferKind::Help` (via the registry's kind
// index). Both paths land on the same mode; the kind binding
// here drives the latter.
lang_mode!(
    MarkdownMode,
    "markdown-mode",
    target_kind = BufferKind::Help
);

/// Resolve a [`Lang`] to its corresponding major-mode id.
/// `Lang::Plain` returns `None` because `text-mode` (the
/// fallback) is owned by `lattice-mode`; the caller falls
/// through to that when the lookup misses.
pub fn major_mode_id_for_lang(lang: Lang) -> Option<ModeId> {
    match lang {
        Lang::Plain => None,
        Lang::Rust => Some(RustMode::mode_id()),
        Lang::Python => Some(PythonMode::mode_id()),
        Lang::JavaScript => Some(JavascriptMode::mode_id()),
        Lang::Bash => Some(BashMode::mode_id()),
        Lang::C => Some(CMode::mode_id()),
        Lang::Cpp => Some(CppMode::mode_id()),
        Lang::Css => Some(CssMode::mode_id()),
        Lang::Go => Some(GoMode::mode_id()),
        Lang::Html => Some(HtmlMode::mode_id()),
        Lang::Java => Some(JavaMode::mode_id()),
        Lang::Json => Some(JsonMode::mode_id()),
        Lang::Lua => Some(LuaMode::mode_id()),
        Lang::Ruby => Some(RubyMode::mode_id()),
        Lang::Sql => Some(SqlMode::mode_id()),
        Lang::Toml => Some(TomlMode::mode_id()),
        Lang::TypeScript => Some(TypeScriptMode::mode_id()),
        Lang::Tsx => Some(TsxMode::mode_id()),
        Lang::Yaml => Some(YamlMode::mode_id()),
        Lang::Markdown => Some(MarkdownMode::mode_id()),
    }
}

/// Resolve a major-mode id back to its corresponding [`Lang`].
/// Returns `None` when the mode has no language binding (e.g.
/// `TextMode`, minor modes).
pub fn lang_for_mode_id(id: ModeId) -> Option<Lang> {
    match id.as_str() {
        "rust-mode" => Some(Lang::Rust),
        "python-mode" => Some(Lang::Python),
        "javascript-mode" => Some(Lang::JavaScript),
        "bash-mode" => Some(Lang::Bash),
        "c-mode" => Some(Lang::C),
        "cpp-mode" => Some(Lang::Cpp),
        "css-mode" => Some(Lang::Css),
        "go-mode" => Some(Lang::Go),
        "html-mode" => Some(Lang::Html),
        "java-mode" => Some(Lang::Java),
        "json-mode" => Some(Lang::Json),
        "lua-mode" => Some(Lang::Lua),
        "ruby-mode" => Some(Lang::Ruby),
        "sql-mode" => Some(Lang::Sql),
        "toml-mode" => Some(Lang::Toml),
        "typescript-mode" => Some(Lang::TypeScript),
        "tsx-mode" => Some(Lang::Tsx),
        "yaml-mode" => Some(Lang::Yaml),
        "markdown-mode" => Some(Lang::Markdown),
        _ => None,
    }
}

/// Register every language major mode against `registry`.
/// Called from the App's mode-registry boot path. Idempotent
/// only by duplication (registry's existing invariant).
///
/// Also registers [`TreeSitterCompletionMode`] (CSM.6) -- the
/// syntax-feature minor that contributes
/// `gen:tree-sitter-symbol` candidates to the completion popup.
pub fn register_language_modes(registry: &mut ModeRegistry) {
    registry
        .register(RustMode)
        .expect("rust-mode register without conflict");
    registry
        .register(PythonMode)
        .expect("python-mode register without conflict");
    registry
        .register(JavascriptMode)
        .expect("javascript-mode register without conflict");
    registry
        .register(BashMode)
        .expect("bash-mode register without conflict");
    registry
        .register(CMode)
        .expect("c-mode register without conflict");
    registry
        .register(CppMode)
        .expect("cpp-mode register without conflict");
    registry
        .register(CssMode)
        .expect("css-mode register without conflict");
    registry
        .register(GoMode)
        .expect("go-mode register without conflict");
    registry
        .register(HtmlMode)
        .expect("html-mode register without conflict");
    registry
        .register(JavaMode)
        .expect("java-mode register without conflict");
    registry
        .register(JsonMode)
        .expect("json-mode register without conflict");
    registry
        .register(LuaMode)
        .expect("lua-mode register without conflict");
    registry
        .register(RubyMode)
        .expect("ruby-mode register without conflict");
    registry
        .register(SqlMode)
        .expect("sql-mode register without conflict");
    registry
        .register(TomlMode)
        .expect("toml-mode register without conflict");
    registry
        .register(TypeScriptMode)
        .expect("typescript-mode register without conflict");
    registry
        .register(TsxMode)
        .expect("tsx-mode register without conflict");
    registry
        .register(YamlMode)
        .expect("yaml-mode register without conflict");
    registry
        .register(MarkdownMode)
        .expect("markdown-mode register without conflict");
    registry
        .register(TreeSitterCompletionMode)
        .expect("tree-sitter-completion-mode register without conflict");
}

// ---------------------------------------------------------
// CSM.6: tree-sitter completion source + mode.
// ---------------------------------------------------------

/// Stable id for the tree-sitter symbol completion source.
/// Must match `lattice_completion::TREE_SITTER_SYMBOL_SOURCE_ID`
/// -- the host's per-language allowlist and `:set
/// completion.source.<id>.priority` key off this string.
pub const TREE_SITTER_COMPLETION_SOURCE_ID: &str = lattice_completion::TREE_SITTER_SYMBOL_SOURCE_ID;

/// The `SyncCompletionSource` impl that emits tree-sitter
/// local-symbol candidates. Stateless -- reads the pre-computed
/// symbol slice off `InsertContext::tree_sitter_symbols` (the
/// host walks `collect_symbols()` once per populate). Filters
/// out the cursor's own current query so the user doesn't get
/// "complete this word with itself."
#[derive(Debug, Clone, Default)]
pub struct TreeSitterSymbolSource;

impl SyncCompletionSource for TreeSitterSymbolSource {
    fn produce(&self, ctx: &InsertContext<'_>) -> Vec<RawCandidate> {
        ctx.tree_sitter_symbols
            .iter()
            .filter(|sym| sym.as_str() != ctx.query)
            .map(|sym| {
                RawCandidate::plain(sym.clone(), CandidateKind::Plain)
                    .with_source(SourceId::new(TREE_SITTER_COMPLETION_SOURCE_ID))
            })
            .collect()
    }
}

/// `tree-sitter-completion-mode` (CSM.6). Contributes the
/// tree-sitter symbol source while active. Auto-activates on
/// Document buffers via `auto_activated_minors_for_buffer_kind`
/// in `lattice-ui-tui::modes`. `popup_filter_chord = Some('t')`
/// ⇒ `<C-t>` inside `completion-popup-mode` narrows the popup
/// to tree-sitter symbols only.
pub struct TreeSitterCompletionMode;

impl TreeSitterCompletionMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("tree-sitter-completion-mode")
    }
}

impl Mode for TreeSitterCompletionMode {
    type Guard = ();
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn completion_sources(&self) -> Vec<CompletionSourceContribution> {
        vec![CompletionSourceContribution {
            id: SourceId::new(TREE_SITTER_COMPLETION_SOURCE_ID),
            // 80 per insert-completion.md §3.4 -- buffer-words
            // (100) wins on ties because it's a superset of
            // tree-sitter symbols.
            default_priority: 80,
            auto_trigger: true,
            trigger_chars: Vec::new(),
            popup_filter_chord: Some('t'),
            kind: CompletionSourceKind::Sync(Arc::new(TreeSitterSymbolSource)),
        }]
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    fn all_lang_mode_ids() -> Vec<ModeId> {
        vec![
            RustMode::mode_id(),
            PythonMode::mode_id(),
            JavascriptMode::mode_id(),
            BashMode::mode_id(),
            CMode::mode_id(),
            CppMode::mode_id(),
            CssMode::mode_id(),
            GoMode::mode_id(),
            HtmlMode::mode_id(),
            JavaMode::mode_id(),
            JsonMode::mode_id(),
            LuaMode::mode_id(),
            RubyMode::mode_id(),
            SqlMode::mode_id(),
            TomlMode::mode_id(),
            TypeScriptMode::mode_id(),
            TsxMode::mode_id(),
            YamlMode::mode_id(),
            MarkdownMode::mode_id(),
        ]
    }

    #[test]
    fn each_lang_mode_has_distinct_id() {
        let ids = all_lang_mode_ids();
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn major_mode_id_for_lang_round_trips() {
        let cases: Vec<(Lang, Option<ModeId>)> = vec![
            (Lang::Plain, None),
            (Lang::Rust, Some(RustMode::mode_id())),
            (Lang::Python, Some(PythonMode::mode_id())),
            (Lang::JavaScript, Some(JavascriptMode::mode_id())),
            (Lang::Bash, Some(BashMode::mode_id())),
            (Lang::C, Some(CMode::mode_id())),
            (Lang::Cpp, Some(CppMode::mode_id())),
            (Lang::Css, Some(CssMode::mode_id())),
            (Lang::Go, Some(GoMode::mode_id())),
            (Lang::Html, Some(HtmlMode::mode_id())),
            (Lang::Java, Some(JavaMode::mode_id())),
            (Lang::Json, Some(JsonMode::mode_id())),
            (Lang::Lua, Some(LuaMode::mode_id())),
            (Lang::Ruby, Some(RubyMode::mode_id())),
            (Lang::Sql, Some(SqlMode::mode_id())),
            (Lang::Toml, Some(TomlMode::mode_id())),
            (Lang::TypeScript, Some(TypeScriptMode::mode_id())),
            (Lang::Tsx, Some(TsxMode::mode_id())),
            (Lang::Yaml, Some(YamlMode::mode_id())),
            (Lang::Markdown, Some(MarkdownMode::mode_id())),
        ];
        for (lang, expected) in cases {
            assert_eq!(major_mode_id_for_lang(lang), expected, "{lang:?}");
        }
    }

    #[test]
    fn register_language_modes_populates_registry() {
        let mut registry = ModeRegistry::new();
        register_language_modes(&mut registry);
        for id in all_lang_mode_ids() {
            assert!(registry.is_registered(id), "mode not registered: {id:?}");
        }
    }

    #[test]
    fn lang_for_mode_id_round_trips() {
        for (lang, mode_id) in [
            (Lang::Rust, RustMode::mode_id()),
            (Lang::Python, PythonMode::mode_id()),
            (Lang::JavaScript, JavascriptMode::mode_id()),
            (Lang::Bash, BashMode::mode_id()),
            (Lang::C, CMode::mode_id()),
            (Lang::Cpp, CppMode::mode_id()),
            (Lang::Css, CssMode::mode_id()),
            (Lang::Go, GoMode::mode_id()),
            (Lang::Html, HtmlMode::mode_id()),
            (Lang::Java, JavaMode::mode_id()),
            (Lang::Json, JsonMode::mode_id()),
            (Lang::Lua, LuaMode::mode_id()),
            (Lang::Ruby, RubyMode::mode_id()),
            (Lang::Sql, SqlMode::mode_id()),
            (Lang::Toml, TomlMode::mode_id()),
            (Lang::TypeScript, TypeScriptMode::mode_id()),
            (Lang::Tsx, TsxMode::mode_id()),
            (Lang::Yaml, YamlMode::mode_id()),
            (Lang::Markdown, MarkdownMode::mode_id()),
        ] {
            assert_eq!(
                lang_for_mode_id(mode_id),
                Some(lang),
                "{}",
                mode_id.as_str()
            );
        }
        assert_eq!(lang_for_mode_id(ModeId::new("text-mode")), None);
    }

    #[test]
    fn each_lang_mode_is_major() {
        for id in all_lang_mode_ids() {
            let mode: Box<dyn Mode<Guard = ()>> = match id.as_str() {
                "rust-mode" => Box::new(RustMode),
                "python-mode" => Box::new(PythonMode),
                "javascript-mode" => Box::new(JavascriptMode),
                "bash-mode" => Box::new(BashMode),
                "c-mode" => Box::new(CMode),
                "cpp-mode" => Box::new(CppMode),
                "css-mode" => Box::new(CssMode),
                "go-mode" => Box::new(GoMode),
                "html-mode" => Box::new(HtmlMode),
                "java-mode" => Box::new(JavaMode),
                "json-mode" => Box::new(JsonMode),
                "lua-mode" => Box::new(LuaMode),
                "ruby-mode" => Box::new(RubyMode),
                "sql-mode" => Box::new(SqlMode),
                "toml-mode" => Box::new(TomlMode),
                "typescript-mode" => Box::new(TypeScriptMode),
                "tsx-mode" => Box::new(TsxMode),
                "yaml-mode" => Box::new(YamlMode),
                "markdown-mode" => Box::new(MarkdownMode),
                other => panic!("unexpected mode id: {other}"),
            };
            assert_eq!(mode.kind(), ModeKind::Major, "{} is not Major", id.as_str());
        }
    }
}
