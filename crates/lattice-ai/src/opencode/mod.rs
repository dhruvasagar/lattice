//! `opencode` — lattice drives the opencode agent's **native TUI** in a
//! terminal buffer (the v1 opencode integration).
//!
//! `:opencode` spawns the `opencode` TUI (via [`Effect::SpawnTerminal`]) and
//! activates [`OpencodeMode`] — a minor mode over `terminal-mode` that is the
//! seam for future lattice-native integration. The full opencode UX (readline,
//! `/` commands, model switching, history, and its own diff review) comes from
//! opencode's real TUI running in the PTY, so lattice reimplements none of it.
//!
//! This is the **terminal topology** (like the MCP / Claude Code peer): the
//! agent runs in a terminal buffer and lattice layers integration via a minor
//! mode. Contrast [`super::acp`], which drives `opencode acp` *headlessly* and
//! owns the conversation as a buffer — kept for the future IDE-native-review
//! direction, reachable via `:opencode-acp`. See
//! `docs/dev/architecture/agent-integration.md`.
//!
//! [`Effect::SpawnTerminal`]: lattice_grammar::effect::Effect::SpawnTerminal

pub mod commands;
pub mod modes;

use lattice_mode::SubsystemBoot;

/// Wire the opencode terminal integration into editor boot: the `:opencode`
/// ex-command + the `opencode-mode` minor. One line in the crate-root install.
pub fn install(boot: &mut impl SubsystemBoot) {
    commands::register_opencode_ex_commands(boot.commands_mut());
    modes::register_opencode_modes(boot.modes_mut());
}

pub use modes::{OpencodeMode, register_opencode_modes};
