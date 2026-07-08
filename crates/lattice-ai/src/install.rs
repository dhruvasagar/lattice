//! AI-1b: the crate-owned `install(boot)` entry point.
//!
//! Mirrors `lattice_claude_code::install` (the crate collapses everything it
//! owns -- logger construction + config-seed, publisher wiring, mode
//! registration, supervisor spawn, ex-commands, services -- into this single
//! call). The host's Phase-B install list holds one line --
//! `lattice_ai::install(&mut boot)` -- and zero host internals (no
//! `Editor::` method, no host `Effect`/`Action` variant, no `Editor` field
//! for the logger).
//!
//! The `:ai-log` picker that OPENS a buffer is a later task; this `install`
//! only wires the producer side (`AiLogger`), the supervisor, the ex-commands,
//! and `AiLogMode` (which seeds/streams into a buffer once one is opened by
//! name).

use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_mode::SubsystemBoot;

use crate::ai_log::{AiLogLevel, AiLogger};
use crate::commands::register_ai_ex_commands;
use crate::handle::AiClientHandle;
use crate::modes::AiLogMode;

/// Wire the AI (ACP agent client) subsystem into the editor at boot.
pub fn install(boot: &mut impl SubsystemBoot) {
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
