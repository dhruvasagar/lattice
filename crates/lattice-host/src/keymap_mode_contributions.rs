//! K.2.4 — host translation pass for mode-contributed keymaps.
//!
//! Walks the [`ModeRegistry`] (boot path) or a single newly-
//! registered mode (dynamic path) and merges each mode's
//! [`Keymap`] contribution into the host's [`KeymapHandle`]
//! at `KeymapLayer::MinorMode(mode.id())`.
//!
//! See [`keymap-architecture.md` §11.3] for the design
//! contract. The K.2 substrate (K.2.1 chord primitives, K.2.2
//! `BindingMode`, K.2.3 real `Keymap`) is in place; this slice
//! is the plumbing that calls `Mode::keymap()` and inserts the
//! result into the matcher trie. Once K.2.5 promotes the
//! multibuffer + project-search bindings into their owning
//! mode crates, this pass is what makes those bindings
//! reachable; until then it runs over the registry without
//! finding any non-default `Keymap`s (every mode still returns
//! the empty `Keymap::default()`).
//!
//! [`keymap-architecture.md` §11.3]: docs/dev/architecture/keymap-architecture.md

use std::collections::HashMap;
use std::sync::Arc;

use lattice_mode::{BindingMode, DynMode, Keymap, ModeId, ModeRegistry};

use crate::keymap_registry::{KeymapHandle, PushLayerKind};
use crate::keymap_trie::{BoundCommand, KeymapLayer, KeymapTrie};

/// Walk every mode in `registry`, call `Mode::keymap()`, and
/// push each non-empty contribution as a `MinorMode(mode_id)`
/// layer on `handle`.
///
/// Boot-path entry. Runs exactly once at editor boot, after
/// the `ModeRegistry` is fully populated (so every mode that
/// registered before boot completion contributes its bindings
/// before the first keystroke). Modes whose `keymap()` returns
/// `Keymap::default()` (the empty contribution) are skipped —
/// no layer is pushed for them, so registry merge cost stays
/// O(modes-with-bindings), not O(all-modes).
///
/// Re-running the pass is safe (`push_layer` for a
/// `MinorMode(mode_id)` is idempotent-on-identity per K.1.b:
/// re-pushing the same `mode_id` replaces the layer's
/// bindings rather than minting a sibling).
pub fn translate_mode_keymaps(handle: &KeymapHandle, registry: &ModeRegistry) {
    for (mode_id, mode) in registry.iter() {
        push_mode_keymap(handle, mode_id, &mode);
    }
}

/// Translate one mode's `Keymap` contribution and push it as
/// a layer. Symmetric with [`translate_mode_keymaps`] but
/// scoped to a single mode — the entry point a future plugin
/// host or dynamic `ModeRegistry::register` call site uses to
/// hot-attach a newly-loaded mode without re-walking the full
/// registry.
///
/// Today no production caller exists for the dynamic path
/// (plugins are post-1.0); the helper is provided here so the
/// translation logic has one shape rather than two
/// drift-prone copies.
pub fn translate_mode_keymap(handle: &KeymapHandle, mode_id: ModeId, mode: &Arc<dyn DynMode>) {
    push_mode_keymap(handle, mode_id, mode);
}

fn push_mode_keymap(handle: &KeymapHandle, mode_id: ModeId, mode: &Arc<dyn DynMode>) {
    let keymap = mode.keymap();
    if keymap.bindings.is_empty() {
        return;
    }
    let bindings_by_mode = group_bindings_into_tries(&keymap, mode_id);
    handle.push_layer(
        PushLayerKind::MinorMode(mode_id),
        format!("{mode_id}"),
        bindings_by_mode,
    );
}

/// Group a [`Keymap`]'s bindings into one [`KeymapTrie`] per
/// [`BindingMode`], wrapping each `KeymapBinding` in a
/// [`BoundCommand`] at `KeymapLayer::MinorMode(mode_id)`.
///
/// Factored out so unit tests can exercise the grouping +
/// `BoundCommand` construction without going through the
/// registry mutex.
fn group_bindings_into_tries(
    keymap: &Keymap,
    mode_id: ModeId,
) -> HashMap<BindingMode, KeymapTrie> {
    let layer = KeymapLayer::MinorMode(mode_id);
    let mut by_mode: HashMap<BindingMode, KeymapTrie> = HashMap::new();
    for binding in &keymap.bindings {
        let bound = Arc::new(BoundCommand::from_invocation(
            binding.command.clone(),
            binding.source.clone(),
            layer,
        ));
        by_mode
            .entry(binding.mode)
            .or_default()
            .insert(&binding.chords, bound);
    }
    by_mode
}

#[cfg(test)]
mod tests {
    use super::*;

    use lattice_grammar::{CommandInvocation, SourceLocation};
    use lattice_mode::{KeymapBinding, LifecycleFuture, Mode, ModeContext, ModeKind};
    use lattice_protocol::ids::CommandId;
    use lattice_protocol::{ChordPattern, KeyChord};

    use crate::keymap_registry::KeymapHandle;
    use crate::keymap_trie::LookupResult;

    fn synthetic_invocation(raw: u64) -> CommandInvocation {
        CommandInvocation::of(CommandId::new(raw))
    }

    fn here() -> SourceLocation {
        SourceLocation::synthetic("keymap_mode_contributions::tests")
    }

    /// Test mode whose only purpose is to return a configurable
    /// `Keymap`. Only `id`, `kind`, `keymap`, and `on_activate`
    /// need real behavior; everything else falls through to the
    /// trait's defaults.
    struct TestMode {
        id: ModeId,
        keymap: Keymap,
    }

    impl Mode for TestMode {
        type Guard = ();
        fn id(&self) -> ModeId {
            self.id
        }
        fn kind(&self) -> ModeKind {
            ModeKind::Minor
        }
        fn keymap(&self) -> Keymap {
            self.keymap.clone()
        }
        fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
            Box::pin(async { Ok(()) })
        }
    }

    fn test_mode(id_str: &str, keymap: Keymap) -> TestMode {
        TestMode {
            id: ModeId::new(id_str),
            keymap,
        }
    }

    fn lookup(
        handle: &KeymapHandle,
        mode: BindingMode,
        active: &[ModeId],
        chords: &[KeyChord],
    ) -> LookupResult {
        handle.lookup_with_context(mode, chords, active)
    }

    #[test]
    fn translate_skips_modes_with_default_keymap() {
        let h = KeymapHandle::new();
        let mut registry = ModeRegistry::new();
        let mode_id = registry
            .register(test_mode("test-mode/empty", Keymap::default()))
            .expect("register");

        translate_mode_keymaps(&h, &registry);

        // Nothing should fire: no layer pushed, no minor binding.
        let result = lookup(&h, BindingMode::Normal, &[mode_id], &[KeyChord::char('a')]);
        assert!(matches!(result, LookupResult::Unbound));
    }

    #[test]
    fn translate_pushes_single_binding_reachable_via_registry() {
        let h = KeymapHandle::new();
        let cmd = synthetic_invocation(42);
        let binding = KeymapBinding::new(
            BindingMode::Normal,
            vec![ChordPattern::Literal(KeyChord::char('z'))],
            cmd.clone(),
            here(),
        );
        let keymap = Keymap::new().bind(binding);
        let mut registry = ModeRegistry::new();
        let mode_id = registry
            .register(test_mode("test-mode/single", keymap))
            .expect("register");

        translate_mode_keymaps(&h, &registry);

        let result = lookup(&h, BindingMode::Normal, &[mode_id], &[KeyChord::char('z')]);
        match result {
            LookupResult::Bound { command, .. } => {
                assert_eq!(command.command, cmd);
                assert_eq!(command.layer, KeymapLayer::MinorMode(mode_id));
            }
            other => panic!("expected Bound, got {other:?}"),
        }
    }

    #[test]
    fn translate_groups_bindings_across_binding_modes() {
        let h = KeymapHandle::new();
        let normal_cmd = synthetic_invocation(1);
        let visual_cmd = synthetic_invocation(2);
        let keymap = Keymap::new()
            .bind(KeymapBinding::new(
                BindingMode::Normal,
                vec![ChordPattern::Literal(KeyChord::char('x'))],
                normal_cmd.clone(),
                here(),
            ))
            .bind(KeymapBinding::new(
                BindingMode::Visual,
                vec![ChordPattern::Literal(KeyChord::char('x'))],
                visual_cmd.clone(),
                here(),
            ));
        let mut registry = ModeRegistry::new();
        let mode_id = registry
            .register(test_mode("test-mode/multi", keymap))
            .expect("register");

        translate_mode_keymaps(&h, &registry);

        match lookup(&h, BindingMode::Normal, &[mode_id], &[KeyChord::char('x')]) {
            LookupResult::Bound { command, .. } => {
                assert_eq!(command.command, normal_cmd, "Normal-mode binding");
            }
            other => panic!("expected Normal Bound, got {other:?}"),
        }
        match lookup(&h, BindingMode::Visual, &[mode_id], &[KeyChord::char('x')]) {
            LookupResult::Bound { command, .. } => {
                assert_eq!(command.command, visual_cmd, "Visual-mode binding");
            }
            other => panic!("expected Visual Bound, got {other:?}"),
        }
    }

    #[test]
    fn translate_emacs_prefix_sequence_via_bind_chord() {
        // End-to-end the <C-x>pp idiom: bind_chord parses the
        // string into three literals, K.2.4 inserts them at
        // MinorMode layer, the registry walks the path.
        let h = KeymapHandle::new();
        let cmd = synthetic_invocation(99);
        let keymap = Keymap::new().bind_chord(BindingMode::Normal, "<C-x>pp", cmd.clone());
        let mut registry = ModeRegistry::new();
        let mode_id = registry
            .register(test_mode("test-mode/emacs", keymap))
            .expect("register");

        translate_mode_keymaps(&h, &registry);

        // First chord is partial.
        let partial = lookup(&h, BindingMode::Normal, &[mode_id], &[KeyChord::ctrl('x')]);
        assert!(matches!(partial, LookupResult::Partial), "after <C-x>");
        // Two chords in is still partial.
        let still_partial = lookup(
            &h,
            BindingMode::Normal,
            &[mode_id],
            &[KeyChord::ctrl('x'), KeyChord::char('p')],
        );
        assert!(matches!(still_partial, LookupResult::Partial), "after <C-x>p");
        // Third chord terminates.
        let result = lookup(
            &h,
            BindingMode::Normal,
            &[mode_id],
            &[KeyChord::ctrl('x'), KeyChord::char('p'), KeyChord::char('p')],
        );
        match result {
            LookupResult::Bound { command, .. } => {
                assert_eq!(command.command, cmd);
                assert_eq!(command.layer, KeymapLayer::MinorMode(mode_id));
            }
            other => panic!("expected Bound on <C-x>pp, got {other:?}"),
        }
    }

    #[test]
    fn single_mode_translation_matches_bulk_pass() {
        // Round-trip the dynamic-path entry point against the
        // bulk pass: both should produce the same observable
        // registry state for the same mode.
        let bulk_handle = KeymapHandle::new();
        let single_handle = KeymapHandle::new();
        let cmd = synthetic_invocation(7);
        let keymap = Keymap::new().bind_chord(BindingMode::Normal, "gd", cmd.clone());
        let mut registry = ModeRegistry::new();
        let mode_id = registry
            .register(test_mode("test-mode/parity", keymap))
            .expect("register");

        translate_mode_keymaps(&bulk_handle, &registry);
        let mode_arc = registry.get(mode_id).expect("registered mode");
        translate_mode_keymap(&single_handle, mode_id, &mode_arc);

        let chord_path = [KeyChord::char('g'), KeyChord::char('d')];
        let bulk_result = lookup(&bulk_handle, BindingMode::Normal, &[mode_id], &chord_path);
        let single_result = lookup(&single_handle, BindingMode::Normal, &[mode_id], &chord_path);
        match (bulk_result, single_result) {
            (
                LookupResult::Bound {
                    command: bulk_cmd, ..
                },
                LookupResult::Bound {
                    command: single_cmd,
                    ..
                },
            ) => {
                assert_eq!(bulk_cmd.command, cmd);
                assert_eq!(single_cmd.command, cmd);
                assert_eq!(bulk_cmd.layer, single_cmd.layer);
            }
            other => panic!("bulk vs single divergence: {other:?}"),
        }
    }
}
