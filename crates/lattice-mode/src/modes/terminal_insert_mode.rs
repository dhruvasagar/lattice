//! TerminalInsert minor mode for terminal buffers (Slice T2).
//! In TerminalInsert mode, keystrokes send input to PTY instead of editor.

use crate::{Mode, ModeContext, ModeKind, ModeId, OptionOverrideSet, CapabilitySet};

/// Minor mode for TerminalInsert state in terminal buffers.
pub struct TerminalInsertMode;

impl TerminalInsertMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("terminal-insert-mode")
    }
}

impl Mode for TerminalInsertMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    fn options(&self) -> OptionOverrideSet {
        OptionOverrideSet::default()
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn on_activate(&self, _ctx: ModeContext) -> lattice_mode::LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn on_deactivate(&self, _ctx: ModeContext) -> lattice_mode::LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
