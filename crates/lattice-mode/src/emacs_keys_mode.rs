//! `emacs-keys-mode` — a default-on builtin minor mode contributing an
//! emacs-style `<C-x>` leader (a tribute layer). Design:
//! `docs/dev/architecture/emacs-keys.md`; sequencing:
//! `docs/dev/operations/slice-plans/emacs-keys.md`.
//!
//! ## Home (BC.5, 2026-06-23)
//!
//! Moved here from `lattice-host` and reclassified as a **builtin** mode: it
//! is default-on + `Universal`, has no owning feature crate, and is
//! renderer-agnostic — so it belongs with the foundation modes in
//! `lattice-mode` (registered via [`register_foundation_modes`]). All the
//! keymap-trie types its layer builder needs (`KeymapTrie` / `BoundCommand` /
//! `KeymapLayer` from `lattice-keymap`, `ChordPattern` from `lattice-protocol`,
//! `CommandRegistry` / `CommandInvocation` from `lattice-grammar`) live below
//! `lattice-mode`, so the *whole* module moves. The host retains only the
//! keymap-layer **push** (it owns the live `KeymapHandle` + reads `config` for
//! the prefix / enable flag), calling [`emacs_keys_layer_bindings`] here.
//!
//! ## Shape
//!
//! A marker minor mode (`Guard = ()`) with `ActivationPolicy::Universal`, so
//! it auto-activates on every buffer. Its keymap layer is pushed once at boot
//! under `MinorMode(emacs-keys-mode)`; K.1.c's per-keystroke filter gates the
//! chords to buffers where the mode is active — the `diff-mode` pattern.
//!
//! ## Configurable prefix
//!
//! Every binding's chord is `prefix + suffix`, parsed as one key sequence
//! ([`lattice_protocol::parse_chord_sequence`]). The prefix defaults to
//! `"<C-x>"`; rebuilding the layer with a different prefix re-targets the
//! whole tribute. A malformed prefix (user config) or an unknown command
//! name skips that one binding with a warning — never a panic on the boot
//! path (graceful-degradation contract).

use crate::registry::ModeRegistry;
use crate::{ActivationPolicy, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind};

/// The `emacs-keys` minor mode. A marker mode: it owns a keymap layer and
/// an activation policy, but allocates no per-buffer resources.
pub struct EmacsKeysMode;

impl EmacsKeysMode {
    pub fn mode_id() -> ModeId {
        // The mode id carries the conventional `-mode` suffix (like
        // `snippet-mode`, `diff-mode`, …) so it reads as `emacs-keys-mode`
        // in `:describe-mode` / mode listings. The user-facing *option*
        // is the bare `emacs-keys` (`:set emacs-keys`) — a distinct name.
        ModeId::new("emacs-keys-mode")
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

    /// Default-on in **every** buffer — the tribute is universal, so the
    /// `<C-x>` navigation chords (switch buffer, switch pane, quit) work
    /// everywhere the user can focus, including synthetic buffers like
    /// `*messages*` / help / file-tree (mirroring emacs, whose `C-x` map
    /// is live in `*Messages*`). Hence `Universal`, not `Global`
    /// (`Global` is document-only — the right scope for content modes
    /// like snippets, not a universal leader).
    ///
    /// The enable toggle (`:set noemacs-keys`) is NOT gated here: the
    /// mode stays unconditionally active and the *layer* carries the
    /// gate — `enabled=false` rebuilds the leader map empty (see
    /// `emacs_keys_layer_bindings`), so disabling reclaims `<C-x>`
    /// without churning the per-buffer mode set. The layer is
    /// Normal-mode-only, so Terminal-Insert keystroke passthrough is
    /// unaffected and the leader never shadows a synthetic buffer's own
    /// Normal-mode chords (Help's Esc/Enter/`-` are distinct keys).
    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Universal
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// Register `emacs-keys-mode` against `registry`. Called from
/// [`register_foundation_modes`](crate::register_foundation_modes) — it is a
/// builtin, so there is no separate boot call.
pub fn register_emacs_keys_mode(registry: &mut ModeRegistry) {
    registry
        .register(EmacsKeysMode)
        .expect("emacs-keys-mode must register without conflict");
}

/// The default leader-map. `(suffix, command canonical name)`; the chord
/// bound is `prefix + suffix`. Every target is an existing command (ex or
/// action) resolved by name at build time — no new command is introduced
/// by S1/S2.
///
/// Tier-1 (S1) — buffer / file / save. emacs convention: `b` switches
/// buffers (the picker), `<C-b>` lists them — distinct chords, mirroring
/// `switch-to-buffer` vs `list-buffers`.
const TIER1_BINDINGS: &[(&str, &str)] = &[
    ("<C-f>", "ex:files"),     // C-x C-f — find file (picker)
    ("<C-s>", "ex:write"),     // C-x C-s — save buffer
    ("b", "ex:buffer-picker"), // C-x b   — switch buffer
    ("<C-b>", "ex:buffers"),   // C-x C-b — list buffers
    ("k", "ex:bdelete"),       // C-x k   — kill buffer
    // S3a: emacs `C-x C-c` = save-buffers-kill-emacs. Targets the
    // dirty-guarded `:qa` (quit every pane + tab), not the brute
    // `<C-c>` quit — so unsaved changes are honored, mirroring emacs.
    ("<C-c>", "ex:quit-all"), // C-x C-c — quit all (dirty-guarded)
];

/// Tier-2 (S2) — pane / window. Targets the pre-registered `action:*`
/// pane commands (`crates/lattice-host/src/actions.rs`), reused verbatim;
/// the leader is just a second entry point to the same `CommandId`s the
/// `<C-w>` family already binds. The digit suffixes are matched as literal
/// second chords after the `<C-x>` partial, so they never enter count
/// accumulation (mirrors emacs `C-x 2` / `C-x 3` / `C-x 0`).
///
/// emacs: `2` splits below, `3` splits right, `0` deletes this window,
/// `1` deletes other windows, `o` cycles focus. Lattice's split axis
/// names are locked per the slice plan (`2`→horizontal, `3`→vertical).
const TIER2_BINDINGS: &[(&str, &str)] = &[
    ("2", "action:split-pane-horizontal"), // C-x 2 — split below
    ("3", "action:split-pane-vertical"),   // C-x 3 — split right
    ("0", "action:close-pane"),            // C-x 0 — delete this pane
    ("1", "action:only-pane"),             // C-x 1 — delete other panes (S3b)
    ("o", "action:next-pane"),             // C-x o — focus other pane
];

/// Build the `emacs-keys` keymap layer for the given `enabled` flag and
/// `prefix`, resolving each binding's command name against `registry`.
/// Returns the per-mode trie map the host pushes under
/// `MinorMode(emacs-keys-mode)`.
///
/// `enabled` is the `:set emacs-keys` toggle: `false` yields an EMPTY
/// Normal trie. The layer is the gate (the mode itself stays a marker),
/// so re-pushing an empty layer on `:set noemacs-keys` clears the leader
/// live — `<C-x>` falls through to plain Normal-mode resolution.
///
/// Graceful degradation: an unparseable `prefix + suffix` chord (bad user
/// config) or an unregistered command name skips that binding with a
/// `warn!` rather than aborting. A wholly-malformed prefix therefore also
/// yields an empty tribute instead of a panic.
pub fn emacs_keys_layer_bindings(
    enabled: bool,
    prefix: &str,
    registry: &lattice_grammar::CommandRegistry,
) -> std::collections::HashMap<crate::BindingMode, lattice_keymap::KeymapTrie> {
    use crate::BindingMode;
    use lattice_grammar::CommandInvocation;
    use lattice_grammar::source::SourceLocation;
    use lattice_keymap::{BoundCommand, KeymapLayer, KeymapTrie};
    use lattice_protocol::chord::ChordPattern;
    use std::collections::HashMap;
    use std::sync::Arc;

    let layer = KeymapLayer::MinorMode(EmacsKeysMode::mode_id());
    let mut trie = KeymapTrie::new();

    // Disabled => publish an empty Normal trie so a re-push clears any
    // prior bindings. `<C-x>` then resolves as plain Normal-mode input.
    let bindings: &[&[(&str, &str)]] = if enabled {
        &[TIER1_BINDINGS, TIER2_BINDINGS]
    } else {
        &[]
    };
    for (suffix, command) in bindings.iter().copied().flatten() {
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
            tracing::warn!(
                command,
                "emacs-keys: skipping binding -- command not registered"
            );
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
    use crate::BindingMode;
    use lattice_keymap::LookupResult;

    fn registry() -> lattice_grammar::CommandRegistry {
        let mut r = lattice_grammar::CommandRegistry::new();
        let _builtins = lattice_grammar::builtins::populate(&mut r);
        let _ = lattice_grammar::ex_commands::populate(&mut r);
        // Tier-2 resolves the host `action:*` pane commands. The host's
        // `actions::populate` is not reachable here (it lives in
        // `lattice-host`), so register the five pane-action names as minimal
        // no-op action commands — `emacs_keys_layer_bindings` only needs
        // `id_by_name` to resolve them.
        for name in [
            "action:split-pane-horizontal",
            "action:split-pane-vertical",
            "action:close-pane",
            "action:only-pane",
            "action:next-pane",
        ] {
            r.register_action(
                name,
                "test pane action",
                lattice_grammar::registry::ActionSpec {
                    apply: Box::new(|_ctx| Ok(lattice_grammar::Effect::None)),
                    args_schema: vec![],
                },
            );
        }
        r
    }

    fn seq(s: &str) -> Vec<lattice_protocol::chord::KeyChord> {
        lattice_protocol::parse_chord_sequence(s).unwrap()
    }

    #[test]
    fn mode_id_uses_the_mode_suffix() {
        // Convention (mode_id.rs): every mode id ends in `-mode`. The
        // emacs-keys mode is `emacs-keys-mode`, distinct from the
        // `emacs-keys` *option* (`:set emacs-keys`). Guards the rename.
        assert_eq!(EmacsKeysMode::mode_id().as_str(), "emacs-keys-mode");
        assert!(EmacsKeysMode::mode_id().as_str().ends_with("-mode"));
    }

    #[test]
    fn default_prefix_binds_every_tier1_chord() {
        let modes = emacs_keys_layer_bindings(true, "<C-x>", &registry());
        let trie = modes.get(&BindingMode::Normal).unwrap();
        // Each full chord resolves to a terminal binding.
        for full in [
            "<C-x><C-f>",
            "<C-x><C-s>",
            "<C-x>b",
            "<C-x><C-b>",
            "<C-x>k",
            "<C-x><C-c>", // S3a: quit-all
        ] {
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
    fn default_prefix_binds_every_tier2_pane_chord() {
        let reg = registry();
        let modes = emacs_keys_layer_bindings(true, "<C-x>", &reg);
        let trie = modes.get(&BindingMode::Normal).unwrap();
        // The digit suffixes (`2` / `3` / `0`) are matched as literal
        // second chords after the `<C-x>` partial -- they never enter
        // count accumulation -- alongside the `o` letter suffix.
        for full in ["<C-x>2", "<C-x>3", "<C-x>0", "<C-x>1", "<C-x>o"] {
            assert!(
                matches!(trie.lookup(&seq(full)), LookupResult::Bound { .. }),
                "expected pane chord `{full}` to be bound"
            );
        }
        // The split-axis wiring is correct (catches a name swap):
        // `<C-x>2` targets the horizontal split specifically.
        let LookupResult::Bound { command, .. } = trie.lookup(&seq("<C-x>2")) else {
            panic!("`<C-x>2` should be bound");
        };
        assert_eq!(
            command.command.command,
            reg.id_by_name("action:split-pane-horizontal").unwrap(),
            "`<C-x>2` must target action:split-pane-horizontal"
        );
    }

    #[test]
    fn alternate_prefix_retargets_the_whole_map() {
        let modes = emacs_keys_layer_bindings(true, "<C-c>", &registry());
        let trie = modes.get(&BindingMode::Normal).unwrap();
        // The new prefix is live...
        assert!(matches!(
            trie.lookup(&seq("<C-c><C-f>")),
            LookupResult::Bound { .. }
        ));
        // ...and the old one is gone.
        assert!(matches!(
            trie.lookup(&seq("<C-x><C-f>")),
            LookupResult::Unbound
        ));
    }

    #[test]
    fn malformed_prefix_degrades_to_empty_no_panic() {
        // A garbage prefix can't parse into a chord; every binding skips,
        // leaving an empty tribute rather than panicking on boot.
        let modes = emacs_keys_layer_bindings(true, "<C-", &registry());
        let trie = modes.get(&BindingMode::Normal).unwrap();
        assert!(matches!(
            trie.lookup(&seq("<C-x><C-f>")),
            LookupResult::Unbound
        ));
    }

    #[test]
    fn disabled_yields_empty_layer() {
        // `:set noemacs-keys` => enabled=false => the Normal trie is
        // present but empty, so `<C-x>` and `<C-x><C-f>` both fall
        // through (Unbound). Re-pushing this empty layer is how the live
        // toggle reclaims `<C-x>`.
        let modes = emacs_keys_layer_bindings(false, "<C-x>", &registry());
        let trie = modes.get(&BindingMode::Normal).unwrap();
        assert!(matches!(
            trie.lookup(&seq("<C-x><C-f>")),
            LookupResult::Unbound
        ));
        assert!(matches!(trie.lookup(&seq("<C-x>")), LookupResult::Unbound));
    }
}
