//! ACP transport boot wiring.
//!
//! Spawns the supervisor (owns the provider child process, the ACP connection,
//! and the active session for the program's lifetime), wires the
//! `:opencode` / `:ai-prompt` / `:ai-stop` ex-commands, and registers the
//! `AiClientHandle` service. Called by the crate-root `install` (behind
//! `#[cfg(feature = "acp")]`), which owns the transport-neutral log substrate.

use lattice_agent::AiLogger;
use lattice_mode::SubsystemBoot;

use crate::acp::commands::register_ai_ex_commands;
use crate::acp::handle::AiClientHandle;

/// Wire the ACP (Agent Client Protocol) transport into the editor at boot. The
/// `logger` is the port-level `AiLogger` the supervisor streams agent records
/// into; the crate-root `install` already registered it as a service.
pub fn install(boot: &mut impl SubsystemBoot, logger: &AiLogger) {
    // Spawn the supervisor with a logger clone.
    let handle = AiClientHandle::spawn(boot.runtime_handle(), logger.clone());

    // Crate-owned ex-commands: `:opencode` / `:ai-prompt` / `:ai-stop`.
    register_ai_ex_commands(boot.commands_mut(), handle.clone());

    // `AiClientHandle` service for a future modeline/UI.
    boot.register_service::<AiClientHandle>(handle);
}
