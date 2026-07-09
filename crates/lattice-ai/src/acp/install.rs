//! ACP transport boot wiring.
//!
//! Spawns the supervisor (owns the provider child process, the ACP connection,
//! and the active session for the program's lifetime), wires the
//! `:opencode` / `:ai-prompt` / `:ai-stop` ex-commands, and registers the
//! `AiClientHandle` service. Called by the crate-root `install` (behind
//! `#[cfg(feature = "acp")]`), which owns the transport-neutral log substrate.

use std::sync::Arc;

use lattice_agent::AiLogger;
use lattice_mode::SubsystemBoot;

use crate::acp::commands::register_ai_ex_commands;
use crate::acp::conversation::ConversationStore;
use crate::acp::handle::AiClientHandle;

/// Wire the ACP (Agent Client Protocol) transport into the editor at boot. The
/// `logger` is the port-level `AiLogger` the supervisor streams *trace* records
/// into; the crate-root `install` already registered it as a service.
pub fn install(boot: &mut impl SubsystemBoot, logger: &AiLogger) {
    // The structured conversation store: the supervisor folds agent *conversation*
    // output into it and publishes `ConversationUpdated` on the event bus so the
    // `ai-conversation` mode (AU-2) can live-tail. Registered as a service so the
    // mode can read snapshots.
    let bus = boot.event_bus().clone();
    let conv_store = ConversationStore::new(Arc::new(move |event| bus.publish_typed(event)));

    // Spawn the supervisor with a logger clone (trace) + the conversation store.
    let handle = AiClientHandle::spawn(boot.runtime_handle(), logger.clone(), conv_store.clone());

    // Crate-owned ex-commands: `:opencode` / `:ai-prompt` / `:ai-stop`.
    register_ai_ex_commands(boot.commands_mut(), handle.clone());

    // Services: `AiClientHandle` for a future modeline/UI; `ConversationStore`
    // for the `ai-conversation` mode's projection.
    boot.register_service::<AiClientHandle>(handle);
    boot.register_service::<ConversationStore>(conv_store);
}
