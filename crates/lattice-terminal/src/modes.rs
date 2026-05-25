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

pub fn register_terminal_modes(registry: &mut ModeRegistry) {
    registry
        .register(TerminalMode)
        .expect("terminal-mode register");
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
    fn register_terminal_mode_populates_registry() {
        let mut registry = ModeRegistry::new();
        register_terminal_modes(&mut registry);
        assert!(registry.is_registered(TerminalMode::mode_id()));
    }
}
