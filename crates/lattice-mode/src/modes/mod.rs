//! Foundation modes that ship with `lattice-mode`.
//!
//! Includes the catch-all [`TextMode`] plus the modes that
//! are genuinely foundational or shared across many features
//! (`help-mode`, `hover-mode`). Per the rule of thumb -- "a
//! mode lives with the crate that owns its associated feature"
//! -- modes for feature crates live with their crate:
//!
//! - Language modes (`rust-mode`, `markdown-mode`, ...) live
//!   in `lattice-syntax`.
//! - LSP log modes + the `lsp-mode` umbrella + its sub-modes
//!   live in `lattice-lsp`.
//! - **`oil-mode`** lives in `lattice-oil`.
//! - **`file-tree-mode`** lives in `lattice-file-tree`.
//!
//! Exceptions kept here:
//!
//! - [`TextMode`] -- the catch-all major when no language
//!   matches; foundational, no owning feature crate.
//! - [`HelpMode`] -- shared across `:help`,
//!   `:describe-*`, `:apropos`, `:keymap`, `:options`,
//!   `:customize`. Many features compose with it; living
//!   under any one of those crates would be arbitrary.
//! - [`HoverMode`] -- shared across LSP hover, signature
//!   help, future diagnostic-at-cursor popups.
//! - Display minor modes (`line-numbers-mode`, `wrap-mode`,
//!   ...) -- renderer-agnostic; they contribute typed options
//!   that the renderer reads, no owning feature crate.
//!
//! All modes registered here self-register at App boot via
//! [`register_foundation_modes`]. Feature-crate modes have
//! their own `register_<X>_modes` entry point that the App's
//! boot path calls alongside this one.

pub mod completion;
pub mod display;
pub mod help;
pub mod hover;
pub mod messages;
pub mod text;

pub use completion::{
    ActiveCompletionSources, BufferWordsMode, CompletionMode, CompletionPopupMode,
    PathCompletionMode,
};
pub use display::{
    CurrentLineHighlightMode, LineNumbersMode, ReadOnlyMode, RelativeLineNumbersMode,
    WhitespaceShowMode, WrapMode,
};
pub use help::HelpMode;
pub use hover::HoverMode;
pub use messages::MessagesMode;
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
    // `FileTreeMode` registers from
    // `lattice_file_tree::register_file_tree_modes`, `OilMode`
    // from `lattice_oil::register_oil_modes` -- both called by
    // the App's boot path alongside this function.
    registry
        .register(HelpMode)
        .expect("help-mode must register without conflict");
    // msg-mode.1: `messages-mode` is the major mode for the
    // editor's `*messages*` audit-log buffer. Replaces the
    // pre-msg-mode `text-mode + read-only-mode` combo so the
    // buffer's identity matches `lsp-log-mode`'s pattern.
    registry
        .register(MessagesMode)
        .expect("messages-mode must register without conflict");
    registry
        .register(HoverMode)
        .expect("hover-mode must register without conflict");
    // CSM.K1 (insert-completion.md §12): the two-mode split.
    // `completion-mode` is the persistent gate (auto-active on
    // writable buffers); `completion-popup-mode` is the
    // transient popup-active marker the keymap-overlay sync
    // tracks. Both are marker minors today; the host owns
    // activation lifecycle.
    registry
        .register(CompletionMode)
        .expect("completion-mode must register without conflict");
    registry
        .register(CompletionPopupMode)
        .expect("completion-popup-mode must register without conflict");
    // CSM.4: buffer-words-mode -- first source-contributing
    // mode. Auto-activates on writable kinds via
    // `auto_activated_minors_for_buffer_kind`; the popup's
    // all-sources view and `<C-b>` filter both consume its
    // contribution.
    registry
        .register(BufferWordsMode)
        .expect("buffer-words-mode must register without conflict");
    // CSM.7: path-completion-mode -- contributes filesystem-
    // entry candidates inside string scopes.
    registry
        .register(PathCompletionMode)
        .expect("path-completion-mode must register without conflict");
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
    // M.7.2: whitespace + current-line-highlight modes. Their
    // backing typed options exist; the renderer's painting hooks
    // land in M.7.3.
    registry
        .register(WhitespaceShowMode)
        .expect("whitespace-show-mode must register without conflict");
    registry
        .register(CurrentLineHighlightMode)
        .expect("current-line-highlight-mode must register without conflict");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foundation_modes_register_without_conflict() {
        let mut registry = ModeRegistry::new();
        register_foundation_modes(&mut registry);
        assert!(registry.is_registered(TextMode::mode_id()));
        // FileTreeMode + OilMode register from their feature
        // crates' helpers; not asserted here.
        assert!(registry.is_registered(HelpMode::mode_id()));
        assert!(registry.is_registered(MessagesMode::mode_id()));
        assert!(registry.is_registered(HoverMode::mode_id()));
        assert!(registry.is_registered(CompletionMode::mode_id()));
        assert!(registry.is_registered(CompletionPopupMode::mode_id()));
        assert!(registry.is_registered(BufferWordsMode::mode_id()));
        assert!(registry.is_registered(PathCompletionMode::mode_id()));
        // M.7.0 display minors.
        assert!(registry.is_registered(LineNumbersMode::mode_id()));
        assert!(registry.is_registered(RelativeLineNumbersMode::mode_id()));
        assert!(registry.is_registered(WrapMode::mode_id()));
        assert!(registry.is_registered(ReadOnlyMode::mode_id()));
        // M.7.2 display minors.
        assert!(registry.is_registered(WhitespaceShowMode::mode_id()));
        assert!(registry.is_registered(CurrentLineHighlightMode::mode_id()));
    }
}
