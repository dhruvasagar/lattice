//! `help-mode` -- minor mode that turns a markdown buffer into a
//! help buffer (DESIGN.md §5.11). Composes with `markdown-mode`
//! (the major), which carries the syntax pipeline + motion
//! semantics. Help-mode adds:
//!
//! - `ReadOnly = true` (option contribution).
//! - Link / anchor metadata parsing (today carried on the
//!   `HelpContent` bundle; future: contributed by help-mode's
//!   `on_activate`).
//! - `<CR>` follow-link dispatch (gated on this minor being
//!   active).
//! - The `:help` / `:describe-*` / `:apropos` / `:keymap` / etc.
//!   workflow commands (gated on this minor being active).
//!
//! Decoupling stance (M.4): the popup UI component is buffer-
//! agnostic. It can render any buffer; help-mode-tagged buffers
//! just happen to be the only popup content kind today. The
//! user's display preference (popup / split / tab / minibuffer)
//! is orthogonal to which mode the buffer carries.

use lattice_config::OptionOverrideSet;

use crate::{CapabilitySet, Mode, ModeActivationError, ModeContext, ModeId, ModeKind};

pub struct HelpMode;

impl HelpMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("help-mode")
    }
}

impl Mode for HelpMode {
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_and_kind() {
        assert_eq!(HelpMode.id(), HelpMode::mode_id());
        assert_eq!(HelpMode::mode_id().as_str(), "help-mode");
        assert_eq!(HelpMode.kind(), ModeKind::Minor);
    }

    #[test]
    fn contributes_read_only() {
        let opts = HelpMode.options();
        assert!(!opts.is_empty());
    }
}
