use lattice_config::OptionOverrideSet;
use lattice_mode::{
    CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, ModeRegistry,
};

pub struct TerminalMode;

impl TerminalMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("terminal-mode")
    }
}

impl Mode for TerminalMode {
    type Guard = ();
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }
    fn options(&self) -> OptionOverrideSet {
        // Terminal buffers are PTY-backed cell grids, not
        // on-disk files: `:q` must not warn about unsaved
        // changes, `:w` is a no-op. Mutation flows through the
        // PTY stdin path (T2), not the rope-operator path; we
        // flag read-only here so the dispatcher rejects naive
        // text inserts in Normal-in-terminal until T2's encoder
        // gate is in place.
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
        }
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// Terminal-mode T2.a (2026-05-25): the minor mode that, when
/// active on a Terminal buffer, switches the translate layer
/// from vim-grammar-over-scrollback to keystroke-encoded-PTY-input.
///
/// Conceptually analogous to Insert mode but scoped per buffer
/// (a minor) rather than globally (a `ModalState` variant): the
/// editor's modal state stays `Normal` underneath, and pane
/// switches automatically pick up the destination buffer's mode
/// set — no implicit auto-Esc handshake when leaving the
/// terminal pane mid-Insert.
///
/// Entry chord: `i` (Normal-in-terminal). Exit chord:
/// `<C-\><C-n>`. T2.b adds `a` / `I` / `A` entry variants and
/// the optional `<Esc>` exit gated by `terminal.esc_exits`.
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
        // No option contributions — the mode is a pure
        // translate-layer discriminator. Read-only / NoFile
        // already come from the underlying terminal-mode major.
        OptionOverrideSet::default()
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

pub fn register_terminal_modes(registry: &mut ModeRegistry) {
    registry
        .register(TerminalMode)
        .expect("terminal-mode register");
    registry
        .register(TerminalInsertMode)
        .expect("terminal-insert-mode register");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_mode_id_kind() {
        assert_eq!(TerminalMode.id(), TerminalMode::mode_id());
        assert_eq!(TerminalMode::mode_id().as_str(), "terminal-mode");
        assert_eq!(TerminalMode.kind(), ModeKind::Major);
    }

    #[test]
    fn terminal_insert_mode_id_kind() {
        assert_eq!(TerminalInsertMode.id(), TerminalInsertMode::mode_id());
        assert_eq!(
            TerminalInsertMode::mode_id().as_str(),
            "terminal-insert-mode",
        );
        assert_eq!(TerminalInsertMode.kind(), ModeKind::Minor);
    }

    #[test]
    fn register_terminal_modes_populates_both() {
        let mut registry = ModeRegistry::new();
        register_terminal_modes(&mut registry);
        assert!(registry.is_registered(TerminalMode::mode_id()));
        assert!(registry.is_registered(TerminalInsertMode::mode_id()));
    }
}
