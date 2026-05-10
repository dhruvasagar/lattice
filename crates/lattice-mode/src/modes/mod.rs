//! Foundation modes that ship with `lattice-mode`.
//!
//! Includes the catch-all [`TextMode`] plus the per-buffer-kind
//! modes that ship with the editor's built-in buffer model
//! (help / hover / file-tree / oil). Language modes live in
//! `lattice-syntax`; LSP log modes live in `lattice-lsp` (where
//! their feature crate is). The rule of thumb: a mode lives
//! with the crate that owns its associated feature, unless the
//! mode is itself foundational (catch-all `text-mode`) or
//! shared across many features (`hover-mode` covers any
//! buffer that pops up as a hover annotation).
//!
//! All modes registered here self-register at App boot via
//! [`register_foundation_modes`].

pub mod display;
pub mod file_tree;
pub mod help;
pub mod hover;
pub mod oil;
pub mod text;

pub use display::{LineNumbersMode, ReadOnlyMode, RelativeLineNumbersMode, WrapMode};
pub use file_tree::FileTreeMode;
pub use help::HelpMode;
pub use hover::HoverMode;
pub use oil::OilMode;
pub use text::TextMode;

use crate::registry::ModeRegistry;

/// Register every foundation mode against `registry`. Called
/// from the App's mode-registry boot path before any buffer is
/// created. Idempotent: re-registration is the existing
/// `ModeRegistry::register` invariant (panics on duplicate, but
/// the App calls this once).
pub fn register_foundation_modes(registry: &mut ModeRegistry) {
    registry
        .register(TextMode)
        .expect("text-mode must register without conflict");
    registry
        .register(FileTreeMode)
        .expect("file-tree-mode must register without conflict");
    registry
        .register(OilMode)
        .expect("oil-mode must register without conflict");
    registry
        .register(HelpMode)
        .expect("help-mode must register without conflict");
    registry
        .register(HoverMode)
        .expect("hover-mode must register without conflict");
    // M.7.0: display minor modes -- user-toggleable wrappers
    // around typed display options.
    registry
        .register(LineNumbersMode)
        .expect("line-numbers-mode must register without conflict");
    registry
        .register(RelativeLineNumbersMode)
        .expect("relative-line-numbers-mode must register without conflict");
    registry
        .register(WrapMode)
        .expect("wrap-mode must register without conflict");
    registry
        .register(ReadOnlyMode)
        .expect("read-only-mode must register without conflict");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foundation_modes_register_without_conflict() {
        let mut registry = ModeRegistry::new();
        register_foundation_modes(&mut registry);
        assert!(registry.is_registered(TextMode::mode_id()));
        assert!(registry.is_registered(FileTreeMode::mode_id()));
        assert!(registry.is_registered(OilMode::mode_id()));
        assert!(registry.is_registered(HelpMode::mode_id()));
        assert!(registry.is_registered(HoverMode::mode_id()));
        // M.7.0 display minors.
        assert!(registry.is_registered(LineNumbersMode::mode_id()));
        assert!(registry.is_registered(RelativeLineNumbersMode::mode_id()));
        assert!(registry.is_registered(WrapMode::mode_id()));
        assert!(registry.is_registered(ReadOnlyMode::mode_id()));
    }
}
