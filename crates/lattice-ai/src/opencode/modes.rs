//! `opencode-mode` — the minor mode layered on the opencode terminal buffer.
//!
//! v1 is a Manual-activation **marker** minor over `terminal-mode`, activated
//! by `:opencode` on the terminal running the agent. It carries no per-buffer
//! resources: opencode's own TUI owns the conversation, the prompt (readline,
//! `/` commands, model switching), history, and edit review, so lattice adds
//! nothing on the hot path. The mode exists as the buffer's *identity* and as
//! the **seam** for future lattice-native integration (IDE-native edit review,
//! a headerline status row) — mirroring `claude-code-mode`'s I1 marker shell.

use lattice_mode::registry::ModeRegistry;
use lattice_mode::{ActivationPolicy, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind};

/// The opencode terminal minor mode.
pub struct OpencodeMode;

impl OpencodeMode {
    /// The mode id (`opencode-mode`).
    pub fn mode_id() -> ModeId {
        ModeId::new("opencode-mode")
    }
}

impl Mode for OpencodeMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    /// Manual — activated explicitly by `:opencode` on the agent terminal.
    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Manual
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// Register `opencode-mode` against `registry`. Called from
/// [`super::install`].
pub fn register_opencode_modes(registry: &mut ModeRegistry) {
    registry
        .register(OpencodeMode)
        .expect("opencode-mode register");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_kind_and_policy() {
        assert_eq!(OpencodeMode::mode_id().as_str(), "opencode-mode");
        assert_eq!(OpencodeMode.kind(), ModeKind::Minor);
        assert_eq!(OpencodeMode.activation_policy(), ActivationPolicy::Manual);
    }

    #[test]
    fn registers_without_conflict() {
        let mut registry = ModeRegistry::new();
        register_opencode_modes(&mut registry);
        assert!(registry.is_registered(OpencodeMode::mode_id()));
    }
}
