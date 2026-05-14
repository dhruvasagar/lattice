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
use lattice_mode::{
    CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, ModeRegistry,
};

use crate::lang::Lang;

/// Macro-internal helper: declare a unit struct + its `Mode`
/// impl with the canonical name. Reduces boilerplate while
/// keeping each mode's source plain Rust (no proc-macro
/// indirection for now).
macro_rules! lang_mode {
    ($struct_name:ident, $mode_name:literal) => {
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
lang_mode!(MarkdownMode, "markdown-mode");

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
        Lang::Markdown => Some(MarkdownMode::mode_id()),
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

    #[test]
    fn each_lang_mode_has_distinct_id() {
        let ids = [
            RustMode::mode_id(),
            PythonMode::mode_id(),
            JavascriptMode::mode_id(),
            MarkdownMode::mode_id(),
        ];
        // Any pair differs.
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn major_mode_id_for_lang_round_trips() {
        assert_eq!(major_mode_id_for_lang(Lang::Plain), None);
        assert_eq!(
            major_mode_id_for_lang(Lang::Rust),
            Some(RustMode::mode_id())
        );
        assert_eq!(
            major_mode_id_for_lang(Lang::Python),
            Some(PythonMode::mode_id())
        );
        assert_eq!(
            major_mode_id_for_lang(Lang::JavaScript),
            Some(JavascriptMode::mode_id())
        );
        assert_eq!(
            major_mode_id_for_lang(Lang::Markdown),
            Some(MarkdownMode::mode_id())
        );
    }

    #[test]
    fn register_language_modes_populates_registry() {
        let mut registry = ModeRegistry::new();
        register_language_modes(&mut registry);
        assert!(registry.is_registered(RustMode::mode_id()));
        assert!(registry.is_registered(PythonMode::mode_id()));
        assert!(registry.is_registered(JavascriptMode::mode_id()));
        assert!(registry.is_registered(MarkdownMode::mode_id()));
    }

    #[test]
    fn each_lang_mode_is_major() {
        assert_eq!(RustMode.kind(), ModeKind::Major);
        assert_eq!(PythonMode.kind(), ModeKind::Major);
        assert_eq!(JavascriptMode.kind(), ModeKind::Major);
        assert_eq!(MarkdownMode.kind(), ModeKind::Major);
    }
}
