//! `oil-mode` -- major mode for the oil.nvim-style editable
//! directory listing buffer. Writable (no `ReadOnly`
//! contribution); diff-on-`:w` applies filesystem ops (rename,
//! delete, create) relative to the buffer's directory. Mode
//! itself is metadata only; the apply / navigate logic lives in
//! the oil owner crate.

use crate::{CapabilitySet, Mode, ModeActivationError, ModeContext, ModeId, ModeKind};

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_and_kind() {
        assert_eq!(OilMode.id(), OilMode::mode_id());
        assert_eq!(OilMode::mode_id().as_str(), "oil-mode");
        assert_eq!(OilMode.kind(), ModeKind::Major);
    }
}
