//! `claude-code-mode` — the minor mode layered on the IDE terminal buffer.
//!
//! I1 registers it as a Manual-activation marker mode (no per-buffer
//! resources). In I5 it becomes the minor mode `:claude` activates on the
//! `BufferKind::Terminal` buffer running the agent: its `on_activate` will
//! ensure the server is running, contribute the headerline status row, and
//! own the diff affordances (design §3). The `-mode` suffix matches the
//! `ModeRegistry` naming convention (enforced at registration).

use lattice_mode::registry::ModeRegistry;
use lattice_mode::{ActivationPolicy, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind};

/// The Claude Code IDE minor mode.
pub struct ClaudeCodeMode;

impl ClaudeCodeMode {
    /// The mode id (`claude-code-mode`).
    pub fn mode_id() -> ModeId {
        ModeId::new("claude-code-mode")
    }
}

impl Mode for ClaudeCodeMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    /// Manual for I1 — the mode is activated explicitly (by `:claude` in
    /// I5). Until then the IDE server lifecycle is driven directly by the
    /// `:claude-code-start` / `:claude-code-stop` ex-commands.
    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Manual
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        // I1 marker: no per-buffer resources yet. I5 spawns/ensures the
        // server here and returns a Guard holding its registrations.
        Box::pin(async { Ok(()) })
    }
}

/// Register `claude-code-mode` against `registry`. Called from editor boot.
pub fn register_claude_code_modes(registry: &mut ModeRegistry) {
    registry
        .register(ClaudeCodeMode)
        .expect("claude-code-mode register");
}
