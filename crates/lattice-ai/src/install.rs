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

use crate::commands::register_ai_log_command;

/// Wire the AI subsystem into the editor at boot. The transport-neutral log
/// substrate (`AiLogger` + `AiLogMode` + `:ai-log`) is always installed; each
/// agent transport is wired only when its feature is enabled.
pub fn install(boot: &mut impl SubsystemBoot) {
    let logger = install_ai_log(boot);

    // The v1 opencode integration: `:opencode` runs opencode's TUI in a
    // terminal buffer. Always wired (no transport feature) -- it's the primary
    // opencode experience. The `acp` adapter below is the alternative
    // buffer-conversation path (`:opencode-acp`).
    crate::opencode::install(boot);

    #[cfg(feature = "acp")]
    crate::acp::install::install(boot, &logger);

    #[cfg(feature = "mcp")]
    crate::mcp::install::install(boot);

    // `AiLogger` is a port-level service (`AiLogMode`'s `on_activate` reads it
    // via `ctx.service::<AiLogger>()`, as does the `:ai-log` picker) -- register
    // it regardless of which transport(s) produce records. This consumes the
    // logger; the ACP supervisor above took a clone.
    boot.register_service::<AiLogger>(logger);
}

/// Port-level: construct the `AiLogger` (seeded from the `ai.*` config options,
/// registered Phase-A before this Phase-B `install` runs), wire its event
/// publisher, register the `AiLogMode` major, and register the transport-neutral
/// `:ai-log` command. Returns the logger for the ACP supervisor to clone.
fn install_ai_log(boot: &mut impl SubsystemBoot) -> AiLogger {
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

    // Publisher: every append -> runtime bus (AiLogMode's drain task subscribes
    // to refresh open `*ai:<provider>:<index>*` buffers).
    let bus = boot.event_bus().clone();
    logger.set_event_publisher(Arc::new(move |event| bus.publish_typed(event)));

    // Register the AiLogMode major (mirrors `register_lsp_log_modes`).
    boot.modes_mut()
        .register(AiLogMode)
        .expect("ai-log-mode register");

    // The transport-neutral `:ai-log` command (no ACP handle needed).
    register_ai_log_command(boot.commands_mut());

    logger
}
