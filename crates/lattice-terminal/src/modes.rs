use lattice_mode::{Mode, ModeId, ModeKind, ModeRegistry, ModeContext, CapabilitySet, LifecycleFuture};

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
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

pub fn register_terminal_modes(registry: &mut ModeRegistry) {
    registry.register(TerminalMode).expect("terminal-mode register")
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
