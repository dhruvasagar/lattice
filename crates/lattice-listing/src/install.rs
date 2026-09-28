//! LM.3: boot-wiring for the listing crate — its modes and the oil chord
//! command declarations.
//!
//! Mirrors `lattice_diff::install` / `lattice_multibuffer::install`: the
//! host calls one `install(boot)` and the crate registers its own modes
//! (keymaps + `action_handlers`, picked up by the K.2.4 +
//! `register_mode_action_handlers` walks) and its `action:*` command
//! declarations. The chord *bodies* live in `OilMode::action_handlers()`;
//! these declarations exist so the keymap `cmd:` names resolve to a
//! `CommandId` and the handlers bind.

use std::sync::Arc;

use lattice_grammar::{ActionSpec, CommandRegistry, Effect};
use lattice_mode::SubsystemBoot;

/// Register the listing crate's modes + the oil chord commands at boot.
pub fn install(boot: &mut impl SubsystemBoot) {
    crate::oil::register_oil_modes(boot.modes_mut());
    crate::file_tree::register_file_tree_modes(boot.modes_mut());
    crate::listing_mode::register_listing_modes(boot.modes_mut());
    register_oil_commands(boot.commands_mut());
}

/// The `action:oil-*` command names the oil-mode chords resolve to. All are
/// mode-owned: the real bodies are `OilMode::action_handlers()`, consulted
/// before this `apply` fallback (which is `Effect::None` — a bare chord in a
/// harness with no handler registered does nothing rather than erroring).
fn register_oil_commands(registry: &mut CommandRegistry) {
    for (name, doc) in [
        (
            "action:oil-follow",
            "oil `<CR>`: open the entry under the cursor — descend into a directory, or open a file in the current pane.",
        ),
        (
            "action:oil-navigate-up",
            "oil `-`: re-list the parent directory, landing on the directory stepped out of.",
        ),
        (
            "action:oil-follow-split",
            "oil `<C-s>`: open the entry under the cursor in a horizontal split.",
        ),
        (
            "action:oil-follow-vsplit",
            "oil `<C-v>`: open the entry under the cursor in a vertical split.",
        ),
        (
            "action:oil-follow-tab",
            "oil `<C-t>`: open the entry under the cursor in a new tab.",
        ),
    ] {
        registry.register_action(
            name,
            doc,
            ActionSpec {
                apply: Arc::new(|_| Ok(Effect::None)),
                args_schema: vec![],
            },
        );
    }
}
