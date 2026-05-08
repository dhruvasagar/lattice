//! Foundation major modes that ship with `lattice-mode`.
//!
//! These are the modes whose declaration doesn't require any
//! feature crate (parser, LSP, ...). Other built-in modes live
//! with their owner crates: language modes in `lattice-grammar`,
//! help / file-tree / oil modes in `lattice-ui-tui`, LSP log
//! modes in `lattice-lsp`.
//!
//! All foundation modes self-register at App boot via
//! [`register_foundation_modes`].

pub mod text;

pub use text::TextMode;

use crate::registry::ModeRegistry;

/// Register every foundation major mode against `registry`.
/// Called from the App's mode-registry boot path before any
/// buffer is created. Idempotent: re-registration is the
/// existing `ModeRegistry::register` invariant (panics on
/// duplicate, but the App calls this once).
pub fn register_foundation_modes(registry: &mut ModeRegistry) {
    registry
        .register(TextMode)
        .expect("text-mode must register without conflict");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foundation_modes_register_without_conflict() {
        let mut registry = ModeRegistry::new();
        register_foundation_modes(&mut registry);
        assert!(registry.is_registered(TextMode::mode_id()));
    }
}
