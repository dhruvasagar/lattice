//! AI-1b / AG‑4: the crate-owned `install(boot)` entry point.
//!
//! After the AG‑4 fold, `lattice-ai` owns both agent transports, so the
//! single host-facing `lattice_ai::install(&mut boot)` line wires **both**:
//! the ACP agent client (`install_acp`, below) and the MCP IDE peer
//! (`crate::mcp::install`). The host's Phase-B install list holds one line
//! and zero host internals (no `Editor::` method, no host `Effect`/`Action`
//! variant, no `Editor` field for the logger). AG‑5 relocates `install_acp`
//! under `crate::acp` and puts each call behind its `#[cfg(feature = …)]`.
//!
//! The `:ai-log` picker that OPENS a buffer is a later task; `install_acp`
//! only wires the producer side (`AiLogger`), the supervisor, the ex-commands,
//! and `AiLogMode` (which seeds/streams into a buffer once one is opened by
//! name).

use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_mode::SubsystemBoot;

use lattice_agent::{AiLogLevel, AiLogMode, AiLogger};

use crate::commands::register_ai_ex_commands;
use crate::handle::AiClientHandle;

/// Wire both AI agent transports into the editor at boot: the ACP agent
/// client and the MCP (Claude Code) IDE peer.
pub fn install(boot: &mut impl SubsystemBoot) {
    install_acp(boot);
    crate::mcp::install::install(boot);
}

/// Wire the AI (ACP agent client) subsystem into the editor at boot.
fn install_acp(boot: &mut impl SubsystemBoot) {
    // 1. Construct the logger; seed defaults from the `ai.*` config options
    // (registered Phase-A, before this Phase-B `install` runs -- see
    // `lattice_claude_code::install`'s doc comment for the same ordering
    // guarantee via `boot.service::<Arc<ConfigRegistry>>()`).
    let logger = AiLogger::with_defaults();
    if let Some(config) = boot.service::<Arc<ConfigRegistry>>() {
        // ai.log_level -> default min level.
        if let Some(level_str) = config.get_typed::<lattice_config::core_options::AiLogLevel>()
            && let Some(level) = AiLogLevel::parse(&level_str)
        {
            logger.set_default_level(level);
        }
        // ai.log = false -> disable capture (cap-0 rings drop every record).
        if let Some(enabled) = config.get_typed::<lattice_config::core_options::AiLog>()
            && !*enabled
        {
            logger.set_default_capacity(0);
        }
    }

    // 2. Publisher: every append -> runtime bus (AiLogMode's drain task
    // subscribes to refresh open `*ai:<provider>:<index>*` buffers).
    let bus = boot.event_bus().clone();
    logger.set_event_publisher(Arc::new(move |event| bus.publish_typed(event)));

    // 3. Register the AiLogMode major (mirrors `register_lsp_log_modes`).
    boot.modes_mut()
        .register(AiLogMode)
        .expect("ai-log-mode register");

    // 4. Spawn the supervisor with a logger clone -- it owns the provider
    // child process, the ACP connection, and the active session for the
    // program's lifetime (until every handle clone is dropped).
    let handle = AiClientHandle::spawn(boot.runtime_handle(), logger.clone());

    // 5. Crate-owned ex-commands: `:opencode` / `:ai-prompt` / `:ai-stop`.
    register_ai_ex_commands(boot.commands_mut(), handle.clone());

    // 6. Services. `AiClientHandle` for a future modeline/UI; `AiLogger` for
    // `AiLogMode`'s `on_activate` (`ctx.service::<AiLogger>()`) and the
    // later `:ai-log` picker.
    boot.register_service::<AiClientHandle>(handle);
    boot.register_service::<AiLogger>(logger);
}
