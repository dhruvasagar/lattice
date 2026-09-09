//! `text-mode` -- the default major mode for plain-text content.
//!
//! Catch-all major mode that any buffer falls back to when no
//! more-specific major mode applies. Provides:
//!
//! - No tree-sitter parser.
//! - No LSP attachment.
//! - The default keymap layer (vim grammar).
//! - No mode-scoped option overrides (all options take their
//!   global / default values).
//!
//! Per `mode-architecture.md` §4.1, this is the foundation
//! catch-all. Buffer content with no language detection lands
//! here. Buffer kinds with their own behavior (Help, FileTree,
//! Oil, language-specific Documents) declare their own majors;
//! those modes can `implies` text-mode if they want the
//! default keymap, or specify their own from scratch.

use crate::{
    CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
};

/// Catch-all major mode for plain-text content.
pub struct TextMode;

impl TextMode {
    /// Canonical id for this mode. Used for `:enable text-mode`,
    /// `:customize text-mode` (if it ever has options), etc.
    pub fn mode_id() -> ModeId {
        ModeId::new("text-mode")
    }
}

impl Mode for TextMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }

    fn required_capabilities(&self) -> CapabilitySet {
        // text-mode imposes no requirements -- it activates on
        // any buffer.
        CapabilitySet::empty()
    }

    /// RF.4: plain text is prose, so `autowrap` covers every line.
    ///
    /// The one override this mode carries, and it does not contradict
    /// the module docs above ("no mode-scoped option overrides"): that
    /// described a mode with nothing to say, and prose-versus-code IS
    /// something a catch-all text major can say. A `.txt` buffer that
    /// does not wrap while a `.md` one does would be an arbitrary split.
    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::AutoWrapOption = lattice_core::AutoWrap::All,
        }
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        // No setup work; text-mode is content-free.
        Box::pin(async { Ok(()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_and_kind() {
        let m = TextMode;
        assert_eq!(m.id(), TextMode::mode_id());
        assert_eq!(m.id().as_str(), "text-mode");
        assert_eq!(m.kind(), ModeKind::Major);
    }

    /// RF.4: plain text is prose. A `.txt` buffer that does not wrap
    /// while a `.md` one does would be an arbitrary split.
    #[test]
    fn text_mode_wraps_prose() {
        let opts = TextMode.options();
        assert!(
            opts.iter()
                .any(|o| o.option_type_id
                    == std::any::TypeId::of::<lattice_config::AutoWrapOption>()),
            "text-mode must override autowrap to `all`"
        );
    }

    #[test]
    fn no_capability_requirements() {
        let m = TextMode;
        assert_eq!(m.required_capabilities(), CapabilitySet::empty());
    }
}
