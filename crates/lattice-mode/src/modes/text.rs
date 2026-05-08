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

use crate::{CapabilitySet, Mode, ModeContext, ModeId, ModeKind, ModeActivationError};

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

    fn on_activate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
        // No setup work; text-mode is content-free.
        Ok(())
    }

    fn on_deactivate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
        Ok(())
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

    #[test]
    fn no_capability_requirements() {
        let m = TextMode;
        assert_eq!(m.required_capabilities(), CapabilitySet::empty());
    }
}
