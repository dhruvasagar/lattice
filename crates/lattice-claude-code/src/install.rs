//! BC.3b: the crate-owned `install(boot)` entry point.
//!
//! Boot-composition collapses claude-code's five formerly-scattered
//! `editor_boot` sites (server spawn, ex-command registration, mode
//! registration, the `ClaudeCodeServerHandle` service register, and the late
//! `install_services` read/write wiring) into this single call against the
//! generic [`SubsystemBoot`] surface. The host's Phase-B install list holds one
//! line — `lattice_claude_code::install(&mut boot)` — and zero host internals
//! (no `Editor::` method, no host `Effect`/`Action` variant): mode-ownership,
//! and the property that keeps host churn flat as modes scale.

use lattice_mode::SubsystemBoot;

use crate::inbound::{ClaudeCodeInboundRequest, make_handler};
use crate::server::{self, ClaudeCodeServerHandle, ServerConfig};
use crate::{commands, lockfile, modes};

/// Wire the Claude Code IDE peer into the editor at boot.
pub fn install(boot: &mut impl SubsystemBoot) {
    // Spawn the IDE server supervisor (idle until `:claude-code-start`), reusing
    // the shared async runtime + the generic event bus (the read cache
    // subscribes to DocumentOpened/Closed/SelectionsChanged on it).
    let handle = server::spawn(default_config(), boot.event_bus().clone(), boot.runtime_handle());

    // `:claude-code-start` / `:claude-code-stop` (crate-owned ex-commands whose
    // `apply` drives the handle directly) + `claude-code-mode`.
    commands::register_claude_code_ex_commands(boot.commands_mut(), handle.clone());
    modes::register_claude_code_modes(boot.modes_mut());

    // I2 read tools: the generic buffer-store (trait accessor) + the LSP
    // diagnostics handle (reached via the generic service lookup, so this crate
    // needs no lattice-lsp accessor on the trait; the host registered it as a
    // Phase-A service).
    let buffer_store = Some(boot.buffer_store().clone());
    let diagnostics = boot
        .service::<lattice_lsp::modes::DiagnosticsQueryHandle>()
        .map(|h| (*h).clone());

    // I3 write tools: the generic inbound bus. `send` wakes the actor
    // off-keystroke; the per-tick drain runs `make_handler` (maps each request
    // to an `Effect` + resolves its oneshot). The drain's registration token
    // rides `boot.into_registrations()` into the Editor for the program
    // lifetime.
    let writes = boot.inbound::<ClaudeCodeInboundRequest, _>(make_handler(handle.read_cache()));
    handle.install_services(buffer_store, diagnostics, writes);

    // Expose the handle so `claude-code-mode`'s `on_activate` (I5) reaches it.
    boot.register_service::<ClaudeCodeServerHandle>(handle);
}

/// The default server config: workspace = cwd, lockfile dir = `~/.claude/ide`
/// (temp-dir fallback). Built here so the host never names claude's config
/// shape — adding/owning this is the crate's concern, not `editor_boot`'s.
fn default_config() -> ServerConfig {
    ServerConfig {
        workspace_folders: std::env::current_dir()
            .ok()
            .map(|p| vec![p.display().to_string()])
            .unwrap_or_default(),
        lock_dir: lockfile::default_lock_dir().unwrap_or_else(std::env::temp_dir),
    }
}
