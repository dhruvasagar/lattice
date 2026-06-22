//! `emacs-keys` — a default-on minor mode contributing an emacs-style
//! `<C-x>` leader (a tribute layer). Design:
//! `docs/dev/architecture/emacs-keys.md`; sequencing:
//! `docs/dev/operations/slice-plans/emacs-keys.md`.
//!
//! ## Shape
//!
//! A marker minor mode (`Guard = ()`) with `ActivationPolicy::Global`, so
//! it auto-activates on every document buffer. Its keymap layer is pushed
//! once at boot under `MinorMode(emacs-keys)`; K.1.c's per-keystroke
//! filter gates the chords to buffers where the mode is active — exactly
//! the `diff-mode` pattern (`crate::diff::mode`).
//!
//! ## Configurable prefix
//!
//! Every binding's chord is `prefix + suffix`, parsed as one key sequence
//! ([`lattice_protocol::parse_chord_sequence`]). The prefix defaults to
//! `"<C-x>"`; rebuilding the layer with a different prefix re-targets the
//! whole tribute. A malformed prefix (user config) or an unknown command
//! name skips that one binding with a warning — never a panic on the boot
//! path (graceful-degradation contract).
//!
//! S1 ships Tier-1 (buffer / file / save), all of which resolve to
//! existing ex-commands by name. Tier-2 (panes) and the new `quit-all` /
//! `only` actions land in S2 / S3.

use lattice_mode::registry::ModeRegistry;
use lattice_mode::{ActivationPolicy, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind};

/// The `emacs-keys` minor mode. A marker mode: it owns a keymap layer and
/// an activation policy, but allocates no per-buffer resources.
pub struct EmacsKeysMode;

impl EmacsKeysMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("emacs-keys")
    }
}

impl Mode for EmacsKeysMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    /// Default-on in every document buffer — the tribute is universal.
    /// `:set noemacs-keys` (the enable option, S1b) will gate this; until
    /// the option lands the mode is unconditionally global. `Global`
    /// admits document buffers but not Help / oil / file-tree kinds, so
    /// the leader never shadows those buffers' own chords.
    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Global
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// Register `emacs-keys` against `registry`. Called from the editor boot
/// path alongside the other `register_*_modes` helpers.
pub fn register_emacs_keys_modes(registry: &mut ModeRegistry) {
    registry
        .register(EmacsKeysMode)
        .expect("emacs-keys mode must register without conflict");
}

/// The default leader-map. `(suffix, ex-command canonical name)`; the
/// chord bound is `prefix + suffix`. Tier-1 (S1): every target is an
/// existing ex-command resolved by name at build time.
///
/// emacs convention: `b` switches buffers (the picker), `<C-b>` lists
/// them — distinct chords, mirroring `switch-to-buffer` vs `list-buffers`.
const TIER1_BINDINGS: &[(&str, &str)] = &[
    ("<C-f>", "ex:files"),     // C-x C-f — find file (picker)
    ("<C-s>", "ex:write"),     // C-x C-s — save buffer
    ("b", "ex:buffer-picker"), // C-x b   — switch buffer
    ("<C-b>", "ex:buffers"),   // C-x C-b — list buffers
    ("k", "ex:bdelete"),       // C-x k   — kill buffer
];

/// Build the `emacs-keys` keymap layer for the given `prefix`, resolving
/// each binding's command name against `registry`. Returns the per-mode
/// trie map the host pushes under `MinorMode(emacs-keys)`.
///
/// Graceful degradation: an unparseable `prefix + suffix` chord (bad user
/// config) or an unregistered command name skips that binding with a
/// `warn!` rather than aborting boot. A wholly-malformed prefix therefore
/// yields an empty tribute — `<C-x>` falls through to plain Normal-mode
/// resolution — instead of a panic.
pub fn emacs_keys_layer_bindings(
    prefix: &str,
    registry: &lattice_grammar::CommandRegistry,
) -> std::collections::HashMap<crate::keymap::BindingMode, crate::keymap_trie::KeymapTrie> {
    use crate::keymap::BindingMode;
    use crate::keymap_trie::{BoundCommand, ChordPattern, KeymapLayer, KeymapTrie};
    use lattice_grammar::source::SourceLocation;
    use lattice_grammar::CommandInvocation;
    use std::collections::HashMap;
    use std::sync::Arc;

    let layer = KeymapLayer::MinorMode(EmacsKeysMode::mode_id());
    let mut trie = KeymapTrie::new();

    for (suffix, command) in TIER1_BINDINGS {
        let chord_str = format!("{prefix}{suffix}");
        let seq = match lattice_protocol::parse_chord_sequence(&chord_str) {
            Ok(seq) => seq,
            Err(err) => {
                tracing::warn!(
                    chord = %chord_str,
                    ?err,
                    "emacs-keys: skipping binding -- unparseable chord (check `emacs-keys-prefix`)"
                );
                continue;
            }
        };
        let Some(id) = registry.id_by_name(command) else {
            tracing::warn!(command, "emacs-keys: skipping binding -- command not registered");
            continue;
        };
        let pattern: Vec<ChordPattern> = seq.into_iter().map(ChordPattern::Literal).collect();
        trie.insert(
            &pattern,
            Arc::new(BoundCommand::from_invocation(
                CommandInvocation::of(id),
                SourceLocation::builtin_file(file!(), line!()),
                layer,
            )),
        );
    }

    let mut modes = HashMap::new();
    modes.insert(BindingMode::Normal, trie);
    modes
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::keymap::BindingMode;
    use crate::keymap_trie::LookupResult;

    fn registry() -> lattice_grammar::CommandRegistry {
        let mut r = lattice_grammar::CommandRegistry::new();
        let _ = lattice_grammar::builtins::populate(&mut r);
        let _ = lattice_grammar::ex_commands::populate(&mut r);
        r
    }

    fn seq(s: &str) -> Vec<lattice_protocol::chord::KeyChord> {
        lattice_protocol::parse_chord_sequence(s).unwrap()
    }

    #[test]
    fn default_prefix_binds_every_tier1_chord() {
        let modes = emacs_keys_layer_bindings("<C-x>", &registry());
        let trie = modes.get(&BindingMode::Normal).unwrap();
        // Each full chord resolves to a terminal binding.
        for full in ["<C-x><C-f>", "<C-x><C-s>", "<C-x>b", "<C-x><C-b>", "<C-x>k"] {
            assert!(
                matches!(trie.lookup(&seq(full)), LookupResult::Bound { .. }),
                "expected `{full}` to be bound"
            );
        }
        // The bare prefix is a pending partial (waits for the suffix).
        assert!(matches!(trie.lookup(&seq("<C-x>")), LookupResult::Partial));
        // An unmapped suffix under the prefix is unbound (falls through).
        assert!(matches!(trie.lookup(&seq("<C-x>z")), LookupResult::Unbound));
    }

    #[test]
    fn alternate_prefix_retargets_the_whole_map() {
        let modes = emacs_keys_layer_bindings("<C-c>", &registry());
        let trie = modes.get(&BindingMode::Normal).unwrap();
        // The new prefix is live...
        assert!(matches!(trie.lookup(&seq("<C-c><C-f>")), LookupResult::Bound { .. }));
        // ...and the old one is gone.
        assert!(matches!(trie.lookup(&seq("<C-x><C-f>")), LookupResult::Unbound));
    }

    #[test]
    fn malformed_prefix_degrades_to_empty_no_panic() {
        // A garbage prefix can't parse into a chord; every binding skips,
        // leaving an empty tribute rather than panicking on boot.
        let modes = emacs_keys_layer_bindings("<C-", &registry());
        let trie = modes.get(&BindingMode::Normal).unwrap();
        assert!(matches!(trie.lookup(&seq("<C-x><C-f>")), LookupResult::Unbound));
    }
}
