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

use lattice_mode::{
    CapabilitySet, Mode, ModeActivationError, ModeContext, ModeId, ModeKind, ModeRegistry,
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
            fn id(&self) -> ModeId {
                Self::mode_id()
            }
            fn kind(&self) -> ModeKind {
                ModeKind::Major
            }
            fn required_capabilities(&self) -> CapabilitySet {
                CapabilitySet::empty()
            }
            fn on_activate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
                Ok(())
            }
            fn on_deactivate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
                Ok(())
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
