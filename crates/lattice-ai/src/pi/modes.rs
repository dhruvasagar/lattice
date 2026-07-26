//! `pi-mode` — the minor mode layered on the pi terminal buffer.
//!
//! v1 is a Manual-activation **marker** minor over `terminal-mode`, activated
//! by `:pi` on the terminal running the agent. It carries no per-buffer
//! resources: pi's own TUI owns the conversation, the prompt (readline,
//! `/` commands, model switching), history, and session tree, so lattice adds
//! nothing on the hot path. The mode exists as the buffer's *identity* and as
//! the **seam** for future lattice-native integration (RPC conversation buffer,
//! a headerline status row) — mirroring `opencode-mode`'s marker shell.

use lattice_mode::registry::ModeRegistry;
use lattice_mode::{ActivationPolicy, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind};

/// The pi terminal minor mode.
pub struct PiMode;

impl PiMode {
    /// The mode id (`pi-mode`).
    pub fn mode_id() -> ModeId {
        ModeId::new("pi-mode")
    }
}

impl Mode for PiMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    /// Manual — activated explicitly by `:pi` on the agent terminal.
    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Manual
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// Register `pi-mode` against `registry`. Called from
/// [`super::install`].
pub fn register_pi_modes(registry: &mut ModeRegistry) {
    registry.register(PiMode).expect("pi-mode register");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_kind_and_policy() {
        assert_eq!(PiMode::mode_id().as_str(), "pi-mode");
        assert_eq!(PiMode.kind(), ModeKind::Minor);
        assert_eq!(PiMode.activation_policy(), ActivationPolicy::Manual);
    }

    #[test]
    fn registers_without_conflict() {
        let mut registry = ModeRegistry::new();
        register_pi_modes(&mut registry);
        assert!(registry.is_registered(PiMode::mode_id()));
    }
}
