//! `hover-mode` -- minor mode activated on the hover popup buffer
//! (M.4 hover-popup unification). Marker mode for v1; the only
//! behaviour gated on it today is the State-A auto-dismiss-on-
//! doc-cursor-motion check in the App's dispatch loop. Future
//! hover-only contributions (auto-close timer, bound-`<Esc>`-to-
//! dismiss, signature-help fan-in) layer on without touching the
//! popup-overlay code.
//!
//! Hover content is markdown; the major mode the App activates
//! alongside `hover-mode` is `markdown-mode`, so the renderer's
//! syntax + link extraction treats hover content as any other
//! markdown buffer. Help-mode is intentionally NOT activated on
//! hover popups -- hover content's links are typically external
//! URLs we don't follow internally.

use crate::{CapabilitySet, Mode, ModeActivationError, ModeContext, ModeId, ModeKind};

pub struct HoverMode;

impl HoverMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("hover-mode")
    }
}

impl Mode for HoverMode {
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
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
        assert_eq!(HoverMode.id(), HoverMode::mode_id());
        assert_eq!(HoverMode::mode_id().as_str(), "hover-mode");
        assert_eq!(HoverMode.kind(), ModeKind::Minor);
    }
}
