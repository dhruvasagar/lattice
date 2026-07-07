//! `claude-code-mode` — the minor mode layered on the IDE terminal buffer.
//!
//! I1 registers it as a Manual-activation marker mode (no per-buffer
//! resources). In I5 it becomes the minor mode `:claude` activates on the
//! `BufferKind::Terminal` buffer running the agent: its `on_activate` will
//! ensure the server is running, contribute the headerline status row, and
//! own the diff affordances (design §3). The `-mode` suffix matches the
//! `ModeRegistry` naming convention (enforced at registration).

use lattice_mode::registry::ModeRegistry;
use lattice_mode::{
    ActivationPolicy, Keymap, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
};

use crate::server::ClaudeCodeServerHandle;

/// The Claude Code IDE minor mode.
pub struct ClaudeCodeMode;

impl ClaudeCodeMode {
    /// The mode id (`claude-code-mode`).
    pub fn mode_id() -> ModeId {
        ModeId::new("claude-code-mode")
    }
}

/// I7: per-activation Guard. While `claude-code-mode` is active on a buffer,
/// that buffer shows the `claude-code` modeline status segment; the Guard's
/// `Drop` (on deactivate / buffer close) unregisters it so the segment clears.
/// `None` when the server handle wasn't available at activation (e.g. tests
/// without the service) — then the Guard is an inert no-op.
pub struct ClaudeCodeStatusGuard {
    inner: Option<(ClaudeCodeServerHandle, lattice_core::BufferId)>,
}

impl Drop for ClaudeCodeStatusGuard {
    fn drop(&mut self) {
        if let Some((handle, buffer)) = &self.inner {
            handle.unregister_status_buffer(*buffer);
        }
    }
}

impl Mode for ClaudeCodeMode {
    type Guard = ClaudeCodeStatusGuard;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    /// Manual — activated explicitly by `:claude` (I5) on the agent terminal.
    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Manual
    }

    /// K.2.4 path: the `<C-c>` interrupt chord, contributed by the mode
    /// itself and picked up by `translate_mode_keymaps` at boot. Resolves
    /// `"ex:claude-interrupt"` against the `CommandRegistry` (registered in
    /// `crate::commands`). K.1.c gates it to `claude-code-mode` buffers, and —
    /// because it binds in `Normal` binding-mode — it only fires in
    /// Normal-in-terminal; in Terminal-Insert `<C-c>` still reaches the shell
    /// as SIGINT. This is the one-key complement to the `:claude-interrupt`
    /// ex-command once you've dropped into Normal-in-terminal with `<Esc>`.
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(claude_code_keymap_entries())
    }

    /// I7: register this buffer to show the IDE status segment. The server
    /// handle is a boot service; absent it (tests), the mode degrades to a
    /// no-op Guard. The status content itself is published off-thread by the
    /// crate's status publisher (`crate::status`).
    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, ClaudeCodeStatusGuard> {
        Box::pin(async move {
            // Register the `claude-code` modeline descriptor (idempotent,
            // last-write-wins). Done here, not at install, because the host
            // registers the `ModelineServiceHandle` after the Phase-B install
            // list runs — by activation time (runtime) it is present.
            if let Some(svc) = ctx.service::<lattice_mode::ModelineServiceHandle>() {
                crate::status::register_status_descriptor(&svc);
            }
            let buffer = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let inner = ctx.service::<ClaudeCodeServerHandle>().map(|handle| {
                handle.register_status_buffer(buffer);
                ((*handle).clone(), buffer)
            });
            Ok(ClaudeCodeStatusGuard { inner })
        })
    }
}

/// The `claude-code-mode` keymap entries. Contributed through
/// [`ClaudeCodeMode::keymap`] and resolved against the `CommandRegistry` by
/// the host's K.2.4 translate pass, which pushes them under
/// `MinorMode(claude-code-mode)`. K.1.c gates the chords to buffers where
/// `claude-code-mode` is active; the `Normal` binding-mode scopes `<C-c>` to
/// Normal-in-terminal (in Terminal-Insert `<C-c>` still reaches the shell as
/// SIGINT). An unregistered `cmd` name is dropped by the translate pass with
/// a `warn!` — never a boot panic (graceful degradation).
fn claude_code_keymap_entries() -> &'static [lattice_mode::KeymapEntry] {
    use std::sync::OnceLock;
    static ENTRIES: OnceLock<Vec<lattice_mode::KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![lattice_mode::keymap_entry! {
            mode: Normal, chord: "<C-c>",
            doc: "Interrupt the running Claude agent (sends <Esc> to the terminal)",
            cmd: "ex:claude-interrupt"
        }]
    })
}

/// Register `claude-code-mode` against `registry`. Called from editor boot.
pub fn register_claude_code_modes(registry: &mut ModeRegistry) {
    registry
        .register(ClaudeCodeMode)
        .expect("claude-code-mode register");
}

#[cfg(test)]
mod keymap_tests {
    use super::claude_code_keymap_entries;

    /// The mode binds exactly `<C-c>` (Normal) → the `claude-interrupt`
    /// ex-command. Catches a chord swap, a name drift, or an extra/dropped
    /// binding. The end-to-end resolution on the `MinorMode(claude-code-mode)`
    /// layer is the host's K.2.4 translate pass (shared with tutor / diff);
    /// this pins the mode's contribution.
    #[test]
    fn ctrl_c_binds_to_claude_interrupt_ex_command() {
        let pairs: Vec<(&str, Option<&str>)> = claude_code_keymap_entries()
            .iter()
            .map(|e| (e.chord, e.command))
            .collect();
        assert_eq!(pairs, vec![("<C-c>", Some("ex:claude-interrupt"))]);
    }
}
