//! Major modes for buffer kinds owned by the TUI layer.
//!
//! Three majors corresponding to the existing `BufferKind`
//! variants beyond `Document`:
//!
//! - `help-mode` -- `:describe-*` / `:apropos` / `:keymap` views.
//!   Read-only; markdown-mode-style content with link
//!   navigation (`<CR>` follows a link).
//! - `file-tree-mode` -- the file-tree navigation buffer.
//!   Tree expansion / collapse, open-on-`<CR>`.
//! - `oil-mode` -- editable directory listing
//!   (oil.nvim-style). Writable; `:w` diffs the rope and
//!   applies filesystem ops.
//!
//! Pure declarations in M.3.0. The actual behavior currently
//! lives in scattered call sites in `app.rs` / `render.rs` /
//! `input.rs`; M.3.1 routes those through the mode-id queries
//! and M.4 unifies rendering through `ResolvedOptions`.
//!
//! Per `mode-architecture.md` §4.1 the TUI also hosts
//! `command-line-mode` and `search-line-mode` for the rich
//! minibuffer (DESIGN.md §5.9.10), but the rich minibuffer
//! refactor isn't landed yet -- those modes are deferred to
//! the slice that ships them.

use lattice_mode::{
    CapabilitySet, Mode, ModeActivationError, ModeContext, ModeId, ModeKind, ModeRegistry,
    OptionOverrideSet, TextMode,
};
use lattice_syntax::Lang;

use crate::buffers::BufferKind;

/// Macro for buffer-kind majors that are read-only (Help,
/// FileTree). Oil is writable so it gets its own impl.
macro_rules! read_only_buffer_kind_mode {
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
            fn options(&self) -> OptionOverrideSet {
                lattice_config::overrides! {
                    lattice_config::ReadOnly = true,
                }
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

read_only_buffer_kind_mode!(HelpMode, "help-mode");
read_only_buffer_kind_mode!(FileTreeMode, "file-tree-mode");

/// Oil is the editable directory listing -- writable. No
/// `ReadOnly = true` override.
pub struct OilMode;

impl OilMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("oil-mode")
    }
}

impl Mode for OilMode {
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

/// Resolve the default major-mode id for a [`BufferKind`].
/// `Document` returns `None` because the mode is determined
/// by language detection (see
/// [`lattice_syntax::major_mode_id_for_lang`]); when the
/// language detection returns `Lang::Plain` the caller falls
/// back further to `text-mode`.
pub fn major_mode_id_for_buffer_kind(kind: BufferKind) -> Option<ModeId> {
    match kind {
        BufferKind::Document => None,
        BufferKind::Help => Some(HelpMode::mode_id()),
        BufferKind::FileTree => Some(FileTreeMode::mode_id()),
        BufferKind::Oil => Some(OilMode::mode_id()),
    }
}

/// Register every TUI-owned major mode against `registry`.
pub fn register_buffer_kind_modes(registry: &mut ModeRegistry) {
    registry.register(HelpMode).expect("help-mode register");
    registry
        .register(FileTreeMode)
        .expect("file-tree-mode register");
    registry.register(OilMode).expect("oil-mode register");
}

/// Resolve the major-mode id a buffer should activate based
/// on its kind + (for `Document` kinds) detected language.
/// Combines [`major_mode_id_for_buffer_kind`] with
/// [`lattice_syntax::major_mode_id_for_lang`], falling back to
/// [`TextMode`] when neither layer matches (`Document` +
/// `Lang::Plain`). M.3.1 wires this into the buffer-creation
/// path so each new buffer auto-activates its corresponding
/// major.
pub fn resolve_major_mode(kind: BufferKind, lang: Lang) -> ModeId {
    if let Some(id) = major_mode_id_for_buffer_kind(kind) {
        return id;
    }
    // Document kind: pick by language, fall through to text-mode.
    lattice_syntax::major_mode_id_for_lang(lang).unwrap_or_else(TextMode::mode_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_buffer_kind_mode_has_distinct_id() {
        let ids = [
            HelpMode::mode_id(),
            FileTreeMode::mode_id(),
            OilMode::mode_id(),
        ];
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn buffer_kind_to_mode_id_table() {
        assert_eq!(major_mode_id_for_buffer_kind(BufferKind::Document), None);
        assert_eq!(
            major_mode_id_for_buffer_kind(BufferKind::Help),
            Some(HelpMode::mode_id())
        );
        assert_eq!(
            major_mode_id_for_buffer_kind(BufferKind::FileTree),
            Some(FileTreeMode::mode_id())
        );
        assert_eq!(
            major_mode_id_for_buffer_kind(BufferKind::Oil),
            Some(OilMode::mode_id())
        );
    }

    #[test]
    fn register_buffer_kind_modes_populates_registry() {
        let mut registry = ModeRegistry::new();
        register_buffer_kind_modes(&mut registry);
        assert!(registry.is_registered(HelpMode::mode_id()));
        assert!(registry.is_registered(FileTreeMode::mode_id()));
        assert!(registry.is_registered(OilMode::mode_id()));
    }

    #[test]
    fn resolve_major_mode_combines_kind_and_lang() {
        // Help / FileTree / Oil ignore Lang -- their kind alone
        // determines the major.
        assert_eq!(
            resolve_major_mode(BufferKind::Help, Lang::Rust),
            HelpMode::mode_id()
        );
        assert_eq!(
            resolve_major_mode(BufferKind::FileTree, Lang::Plain),
            FileTreeMode::mode_id()
        );
        assert_eq!(
            resolve_major_mode(BufferKind::Oil, Lang::Markdown),
            OilMode::mode_id()
        );
    }

    #[test]
    fn resolve_major_mode_for_document_picks_by_lang() {
        assert_eq!(
            resolve_major_mode(BufferKind::Document, Lang::Rust),
            lattice_syntax::RustMode::mode_id()
        );
        assert_eq!(
            resolve_major_mode(BufferKind::Document, Lang::Markdown),
            lattice_syntax::MarkdownMode::mode_id()
        );
    }

    #[test]
    fn resolve_major_mode_falls_back_to_text_mode() {
        // Document + Plain ⇒ text-mode (foundation catch-all).
        assert_eq!(
            resolve_major_mode(BufferKind::Document, Lang::Plain),
            TextMode::mode_id()
        );
    }
}
