//! Every chord a mode contributes names a command that exists.
//!
//! A mode's keymap names its commands by string (`cmd: "action:…"`). At boot
//! the host resolves each against the command registry, and one that does not
//! resolve is skipped with a `warn!` — by design, so one bad entry cannot fail
//! boot. The cost of that leniency is that the chord silently does nothing,
//! and the only trace is a log line nobody reads.
//!
//! Two of these had shipped: `*notifications*`'s `<CR>` / `d` named actions
//! that were never registered, and `claude-code-mode`'s `<C-c>` named
//! `ex:claude-interrupt` for a command registered as `claude-interrupt`. Each
//! mode's own test pinned its entry against itself, so both stayed green.
//! This asks the question once, for every mode, against the real registry.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

#[test]
fn every_mode_keymap_command_is_registered() {
    let editor = Editor::boot(CoreDocument::from_text("x\n"));
    let commands = editor.registry.load();
    let modes = editor.mode_registry.load();

    let mut unresolved: Vec<String> = Vec::new();
    for (id, mode) in modes.iter() {
        for entry in &mode.keymap().entries {
            if let Some(cmd) = entry.command
                && commands.id_by_name(cmd).is_none()
            {
                unresolved.push(format!("{id}: `{}` -> `{cmd}`", entry.chord));
            }
        }
    }
    unresolved.sort();

    assert!(
        unresolved.is_empty(),
        "these chords name a command that is not registered, so boot skips \
         them and they do nothing:\n  {}\n\nRegister the command (a mode's \
         `action:*` names need a dead-body `register_action` beside the \
         mode), or fix the name.",
        unresolved.join("\n  ")
    );
}
