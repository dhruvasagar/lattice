//! `repl-mode` — a builtin minor mode contributing the REPL **input surface**.
//!
//! The Normal-mode insert-entry keys (`i`/`a`/`o`/`A`/`I`/`O`) don't insert in
//! place: they move the caret to the buffer's prompt (its last line) and enter
//! Insert. That is the affordance a REPL-like buffer wants — the transcript
//! above the prompt is read-only, so "start typing" should always land you at
//! the prompt.
//!
//! ## Why a minor mode, not a major-mode keymap
//!
//! This behaviour previously lived on `ai-conversation-mode`, a **major** mode,
//! whose keymap overrides vim's universal insert keys. A major-mode keymap is
//! gated by the buffer's active major (K.1.c), but binding the single most
//! common Normal-mode keys there is fragile — any gap in that gating resurfaces
//! them everywhere (the `:describe-key i` / dashboard-jumps-to-EOF saga).
//!
//! Modelling the affordance as a `Manual` **minor** mode that each REPL major
//! (`ai-conversation`, and later terminal / claude) pulls in via
//! [`Mode::implies`](crate::Mode::implies) keeps it OFF every ordinary buffer
//! (a regular `i` stays vim-native), reusable across REPLs, and rides the
//! well-tested minor-mode keymap gating. The user is free to `:set` it on any
//! buffer where it makes sense.

use std::sync::Arc;

use lattice_grammar::ModalState;
use lattice_grammar::effect::Effect;

use crate::registry::ModeRegistry;
use crate::{
    ActionContext, ActionHandler, ActionHandlerContribution, ActivationPolicy, BufferStoreHandle,
    Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, keymap_entry,
};

/// `repl-mode` minor. A marker mode: it owns a keymap layer + one action
/// handler, but allocates no per-buffer resources (`Guard = ()`).
pub struct ReplMode;

impl ReplMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("repl-mode")
    }
}

impl Mode for ReplMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    /// Explicit-only: `repl-mode` is activated by a REPL major's
    /// [`implies`](crate::Mode::implies) (or a user `:set`), never
    /// auto-activated on ordinary buffers. That isolation is the whole point —
    /// a regular buffer's `i` must stay vim-native.
    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Manual
    }

    /// The insert-entry chords. Pushed once at boot under
    /// `MinorMode(repl-mode)`; K.1.c's per-keystroke filter gates them to
    /// buffers where `repl-mode` is active — the `diff-mode` / `snippet-mode`
    /// pattern.
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(repl_mode_keymap_entries())
    }

    /// The generic focus-the-prompt handler, mode-owned. Bound globally at boot
    /// by the host's `register_mode_action_handlers` walk (keyed on the
    /// `CommandId`, active on many buffers at once — the `snippet-expand`
    /// precedent), and gated to `repl-mode`-active buffers by K.1.c.
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![ActionHandlerContribution {
            action_name: "action:repl-focus-input",
            handler: focus_input_handler(),
        }]
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// The `i`/`a`/`o`/`A`/`I`/`O` → `action:repl-focus-input` entries. All six
/// insert-entry chords route to the same handler so entering Insert always
/// relocates the caret to the prompt (the transcript is read-only, so there is
/// nothing to insert-at in place).
fn repl_mode_keymap_entries() -> &'static [KeymapEntry] {
    use std::sync::OnceLock;
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        let focus = |chord: &'static str| {
            keymap_entry! {
                mode: Normal, chord: chord,
                doc: "repl: move the cursor to the prompt and enter Insert",
                cmd: "action:repl-focus-input"
            }
        };
        vec![
            focus("i"),
            focus("a"),
            focus("o"),
            focus("A"),
            focus("I"),
            focus("O"),
        ]
    })
}

/// Place the cursor at the end of the buffer's last line (the prompt) and enter
/// Insert. Generic — reads the buffer through the `BufferStoreHandle` service
/// (the `ActionContext` carries no buffer text), so any REPL buffer whose
/// prompt is its trailing line works without mode-specific wiring.
fn focus_input_handler() -> ActionHandler {
    Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
        let store = ctx.services.get::<BufferStoreHandle>()?;
        let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
        let handle = store.handle_for(buffer_id)?;
        let snap = handle.snapshot();
        let last_line = snap.buffer.line_count().saturating_sub(1);
        let end_byte = snap
            .buffer
            .line(last_line)
            .unwrap_or_default()
            .trim_end_matches('\n')
            .len() as u32;
        let pos = lattice_protocol::position::Position::new(last_line, end_byte);
        Some(Effect::Many(vec![
            Effect::CursorMove(pos),
            Effect::EnterMode(ModalState::Insert),
        ]))
    })
}

/// Register `repl-mode` against `registry`. Called from
/// [`register_foundation_modes`](crate::register_foundation_modes) — it is a
/// builtin, so there is no separate boot call.
pub fn register_repl_mode(registry: &mut ModeRegistry) {
    registry
        .register(ReplMode)
        .expect("repl-mode must register without conflict");
}

/// Register the `action:repl-focus-input` command so the mode's keymap `cmd`
/// name resolves at boot (the `register_ai_conversation_actions` pattern). The
/// `apply` body is a dead `Effect::None`: the mode's `action_handlers` closure
/// intercepts before the grammar Action gate, so this never runs. It exists so
/// the `CommandId` resolves for the chord binding + handler registration.
pub fn register_repl_mode_actions(registry: &mut lattice_grammar::CommandRegistry) {
    use lattice_grammar::registry::ActionSpec;
    registry.register_action(
        "action:repl-focus-input",
        "repl: move the cursor to the prompt and enter Insert (mode-owned).",
        ActionSpec {
            apply: Arc::new(|_| Ok(Effect::None)),
            args_schema: vec![],
        },
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn mode_id_uses_the_mode_suffix() {
        assert_eq!(ReplMode::mode_id().as_str(), "repl-mode");
        assert!(ReplMode::mode_id().as_str().ends_with("-mode"));
    }

    #[test]
    fn is_a_manual_minor_mode() {
        // Manual so it never auto-activates on ordinary buffers — a regular
        // `i` stays vim-native; only a REPL major's `implies` (or `:set`)
        // turns it on.
        assert_eq!(<ReplMode as Mode>::kind(&ReplMode), ModeKind::Minor);
        assert!(matches!(
            <ReplMode as Mode>::activation_policy(&ReplMode),
            ActivationPolicy::Manual
        ));
    }

    #[test]
    fn binds_every_insert_entry_key_to_the_focus_action() {
        let pairs: Vec<(&str, Option<&str>)> = repl_mode_keymap_entries()
            .iter()
            .map(|e| (e.chord, e.command))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("i", Some("action:repl-focus-input")),
                ("a", Some("action:repl-focus-input")),
                ("o", Some("action:repl-focus-input")),
                ("A", Some("action:repl-focus-input")),
                ("I", Some("action:repl-focus-input")),
                ("O", Some("action:repl-focus-input")),
            ],
        );
    }

    #[test]
    fn contributes_the_focus_input_handler() {
        let names: Vec<&str> = ReplMode
            .action_handlers()
            .iter()
            .map(|c| c.action_name)
            .collect();
        assert_eq!(names, vec!["action:repl-focus-input"]);
    }

    #[test]
    fn register_action_makes_the_command_resolvable() {
        let mut r = lattice_grammar::CommandRegistry::new();
        register_repl_mode_actions(&mut r);
        assert!(r.id_by_name("action:repl-focus-input").is_some());
    }
}
