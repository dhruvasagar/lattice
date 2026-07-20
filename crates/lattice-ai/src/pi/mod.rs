//! `pi` — lattice drives the pi agent's **native TUI** in a terminal buffer
//! (the v1 pi integration).
//!
//! `:pi` spawns the `pi` TUI (via [`Effect::SpawnTerminal`]) and activates
//! [`PiMode`] — a minor mode over `terminal-mode` that is the seam for future
//! lattice-native integration (RPC conversation buffer, headerline status).
//! The full pi UX (readline, `/` commands, model switching, history, session
//! tree, extension system) comes from pi's real TUI running in the PTY, so
//! lattice reimplements none of it.
//!
//! This is the **terminal topology** (like the MCP / Claude Code peer and
//! opencode): the agent runs in a terminal buffer and lattice layers
//! integration via a minor mode. Pi's RPC mode (`pi --mode rpc`) offers a
//! future path for deeper lattice-native integration, analogous to the ACP
//! adapter for opencode — that is deferred; see
//! `docs/dev/architecture/pi.md` §6.
//!
//! [`Effect::SpawnTerminal`]: lattice_grammar::effect::Effect::SpawnTerminal

pub mod commands;
pub mod modes;

use lattice_mode::SubsystemBoot;

/// Wire the pi terminal integration into editor boot: the `:pi` ex-command +
/// the `pi-mode` minor. One line in the crate-root install.
pub fn install(boot: &mut impl SubsystemBoot) {
    commands::register_pi_ex_commands(boot.commands_mut());
    modes::register_pi_modes(boot.modes_mut());
}

pub use modes::{PiMode, register_pi_modes};
