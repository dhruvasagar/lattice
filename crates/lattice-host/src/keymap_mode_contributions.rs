//! K.2.4 — host translation pass for mode-contributed keymaps.
//!
//! Walks the [`ModeRegistry`] (boot path) or a single newly-
//! registered mode (dynamic path) and merges each mode's
//! [`Keymap`] contribution into the host's [`KeymapHandle`]
//! at `KeymapLayer::MinorMode(mode.id())`.
//!
//! See [`keymap-architecture.md` §11.3] for the design
//! contract. The K.2 substrate (K.2.1 chord primitives, K.2.2
//! `BindingMode`, K.2.3 real `Keymap`, K.2.4.A.0.1 catalog
//! relocation, K.2.4.A.0.2 entry-form `Keymap::from_entries`)
//! is in place; this slice is the plumbing that calls
//! `Mode::keymap()`, resolves any table-form entries against
//! the [`CommandRegistry`], and inserts the result into the
//! matcher trie. Once K.2.5 promotes the multibuffer +
//! project-search bindings into their owning mode crates, this
//! pass is what makes those bindings reachable; until then it
//! runs over the registry without finding any non-default
//! `Keymap`s (every mode still returns `Keymap::default()`).
//!
//! ## Resolution model
//!
//! Two declaration paths share the same insertion path:
//!
//! - **Chain form** (`Keymap::bindings`): already typed
//!   [`KeymapBinding`]s carrying a [`CommandInvocation`]. Used
//!   directly.
//! - **Table form** (`Keymap::entries`): static-catalog rows
//!   built via the [`keymap_entry!`] macro. Each entry carries
//!   a canonical command-name string (`"motion:line-down"`,
//!   …). The pass resolves the name against the
//!   [`CommandRegistry`] at registration time to mint a
//!   [`CommandInvocation`]; the entry's `doc`, `source`, and
//!   parsed chord flow into the resulting [`KeymapBinding`].
//!   Entries whose name doesn't resolve log a
//!   `tracing::warn!` and skip (matches the existing
//!   catalog-drift convention); entries with
//!   `command = None` (synthetic actions like `PushDigit`)
//!   skip silently — they're informational catalog rows for
//!   `:describe-key` and `:keymap`, not dispatchable bindings.
//!
//! [`keymap-architecture.md` §11.3]: docs/dev/architecture/keymap-architecture.md

use std::collections::HashMap;
use std::sync::Arc;

use lattice_grammar::{CommandInvocation, CommandRegistry};
use lattice_mode::{BindingMode, DynMode, KeymapBinding, ModeId, ModeRegistry};
use lattice_protocol::ChordPattern;

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
/// `command_registry` is needed to resolve any table-form
/// entries (`Keymap::entries`) into typed
/// [`CommandInvocation`]s; chain-form bindings
/// (`Keymap::bindings`) skip the registry walk because they
/// already carry an invocation.
///
/// Re-running the pass is safe (`push_layer` for a
/// `MinorMode(mode_id)` is idempotent-on-identity per K.1.b:
/// re-pushing the same `mode_id` replaces the layer's
/// bindings rather than minting a sibling).
pub fn translate_mode_keymaps(
    handle: &KeymapHandle,
    registry: &ModeRegistry,
    command_registry: &CommandRegistry,
) {
    for (mode_id, mode) in registry.iter() {
        push_mode_keymap(handle, mode_id, &mode, command_registry);
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
pub fn translate_mode_keymap(
    handle: &KeymapHandle,
    mode_id: ModeId,
    mode: &Arc<dyn DynMode>,
    command_registry: &CommandRegistry,
) {
    push_mode_keymap(handle, mode_id, mode, command_registry);
}

fn push_mode_keymap(
    handle: &KeymapHandle,
    mode_id: ModeId,
    mode: &Arc<dyn DynMode>,
    command_registry: &CommandRegistry,
) {
    let keymap = mode.keymap();
    if keymap.bindings.is_empty() && keymap.entries.is_empty() {
        return;
    }
    // Concatenate chain-form bindings with resolved table-form
    // entries so a single grouping pass writes one trie per
    // BindingMode regardless of which path each binding came
    // through.
    let mut all_bindings: Vec<KeymapBinding> = keymap.bindings.clone();
    all_bindings.extend(resolve_entries_into_bindings(
        &keymap.entries,
        command_registry,
    ));
    if all_bindings.is_empty() {
        // Every entry was either synthetic (`command = None`)
        // or unresolvable against the registry. Don't push an
        // empty layer.
        return;
    }
    let bindings_by_mode = group_bindings_into_tries(&all_bindings, mode_id);
    handle.push_layer(
        PushLayerKind::MinorMode(mode_id),
        format!("{mode_id}"),
        bindings_by_mode,
    );
}

/// Group a slice of bindings into one [`KeymapTrie`] per
/// [`BindingMode`], wrapping each `KeymapBinding` in a
/// [`BoundCommand`] at `KeymapLayer::MinorMode(mode_id)`.
///
/// Factored out so unit tests can exercise the grouping +
/// `BoundCommand` construction without going through the
/// registry mutex. Operates on a flat `&[KeymapBinding]`
/// (not `&Keymap`) so [`push_mode_keymap`] can pre-flatten the
/// chain form + resolved table form into one list.
fn group_bindings_into_tries(
    bindings: &[KeymapBinding],
    mode_id: ModeId,
) -> HashMap<BindingMode, KeymapTrie> {
    let layer = KeymapLayer::MinorMode(mode_id);
    let mut by_mode: HashMap<BindingMode, KeymapTrie> = HashMap::new();
    for binding in bindings {
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

/// Walk a mode's table-form entries, resolve each against the
/// [`CommandRegistry`], and emit one [`KeymapBinding`] per
/// resolvable entry.
///
/// Per-entry behaviour:
///
/// - `entry.command == None` → silent skip (synthetic catalog
///   row, not a dispatchable binding).
/// - `entry.command == Some(name)` and the registry has no
///   `name` → `tracing::warn!` and skip (catalog-drift; the
///   binding declares an invocation no command implements).
/// - chord string fails [`lattice_protocol::parse_chord_sequence`]
///   → `tracing::warn!` and skip (declaratively invalid;
///   shouldn't happen since the `keymap_entry!` macro doesn't
///   validate, but defensive).
/// - Otherwise → build a [`KeymapBinding`] whose `mode` /
///   `chords` / `command` come from the entry + registry
///   resolution, and whose `source` and `doc` are carried
///   through from the entry's macro-captured fields.
fn resolve_entries_into_bindings(
    entries: &[&'static lattice_mode::KeymapEntry],
    command_registry: &CommandRegistry,
) -> Vec<KeymapBinding> {
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(name) = entry.command else {
            // Synthetic action (`PushDigit`, `SetPending`, …) —
            // informational catalog row, no dispatchable
            // binding. Skip silently.
            continue;
        };
        let Some(cmd_id) = command_registry.id_by_name(name) else {
            tracing::warn!(
                chord = entry.chord,
                command = name,
                mode = ?entry.mode,
                "keymap_entry: command name not registered in CommandRegistry; skipping binding",
            );
            continue;
        };
        let chords: Vec<ChordPattern> = match lattice_protocol::parse_chord_sequence(entry.chord) {
            Ok(parsed) => parsed.into_iter().map(ChordPattern::Literal).collect(),
            Err(err) => {
                tracing::warn!(
                    chord = entry.chord,
                    error = %err,
                    mode = ?entry.mode,
                    "keymap_entry: chord string failed to parse; skipping binding",
                );
                continue;
            }
        };
        let binding = KeymapBinding::new(
            entry.mode,
            chords,
            CommandInvocation::of(cmd_id),
            entry.source().clone(),
        )
        .with_doc(entry.doc);
        out.push(binding);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::OnceLock;

    use lattice_grammar::{CommandInvocation, CommandRegistry, SourceLocation};
    use lattice_mode::{
        Keymap, KeymapBinding, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeKind,
    };
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

    /// Fresh empty `CommandRegistry`. Sufficient for chain-form
    /// tests that don't go through name resolution.
    fn empty_command_registry() -> CommandRegistry {
        CommandRegistry::new()
    }

    /// `CommandRegistry` populated with the standard vim grammar
    /// builtins (motions / operators / text-objects). Used by
    /// table-form resolution tests that point entries at known
    /// command names like `motion:line-down`.
    fn registry_with_builtins() -> CommandRegistry {
        let mut r = CommandRegistry::new();
        let _ = lattice_grammar::builtins::populate(&mut r);
        r
    }

    /// K.2.4.A.0.3 fixture — table-form entry whose canonical
    /// command name resolves against the builtin registry.
    fn fixture_table_form_entries() -> &'static [KeymapEntry] {
        static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
        ENTRIES.get_or_init(|| {
            vec![lattice_mode::keymap_entry! {
                mode: Normal, chord: "z", doc: "Move down (table-form fixture)",
                cmd: "motion:line-down"
            }]
        })
    }

    /// K.2.4.A.0.3 fixture — synthetic catalog row with no
    /// command (informational; not dispatchable).
    fn fixture_synthetic_entries() -> &'static [KeymapEntry] {
        static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
        ENTRIES.get_or_init(|| {
            vec![lattice_mode::keymap_entry! {
                mode: Normal, chord: "z", doc: "Synthetic entry (no cmd)"
            }]
        })
    }

    /// K.2.4.A.0.3 fixture — entry pointing at a command name
    /// the registry doesn't know. Resolver should warn+skip.
    fn fixture_unresolvable_entries() -> &'static [KeymapEntry] {
        static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
        ENTRIES.get_or_init(|| {
            vec![lattice_mode::keymap_entry! {
                mode: Normal, chord: "z", doc: "Points at nonexistent command",
                cmd: "test:nonexistent-command-xyz"
            }]
        })
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

        translate_mode_keymaps(&h, &registry, &empty_command_registry());

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

        translate_mode_keymaps(&h, &registry, &empty_command_registry());

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

        translate_mode_keymaps(&h, &registry, &empty_command_registry());

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

        translate_mode_keymaps(&h, &registry, &empty_command_registry());

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
        assert!(
            matches!(still_partial, LookupResult::Partial),
            "after <C-x>p"
        );
        // Third chord terminates.
        let result = lookup(
            &h,
            BindingMode::Normal,
            &[mode_id],
            &[
                KeyChord::ctrl('x'),
                KeyChord::char('p'),
                KeyChord::char('p'),
            ],
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

        translate_mode_keymaps(&bulk_handle, &registry, &empty_command_registry());
        let mode_arc = registry.get(mode_id).expect("registered mode");
        translate_mode_keymap(
            &single_handle,
            mode_id,
            &mode_arc,
            &empty_command_registry(),
        );

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

    // ---- K.2.4.A.0.3: table-form entry resolution ----

    #[test]
    fn translate_resolves_table_form_entries_via_command_registry() {
        // Happy path: an entry pointing at a builtin command
        // name resolves through the registry, parses its chord
        // string, and lands as a `Bound` lookup. The CommandId
        // on the resolved binding matches the registry's
        // canonical lookup for the same name.
        let h = KeymapHandle::new();
        let cmd_registry = registry_with_builtins();
        let expected_id = cmd_registry
            .id_by_name("motion:line-down")
            .expect("motion:line-down should be registered by builtins");

        let keymap = Keymap::from_entries(fixture_table_form_entries());
        let mut registry = ModeRegistry::new();
        let mode_id = registry
            .register(test_mode("test-mode/entries-resolve", keymap))
            .expect("register");

        translate_mode_keymaps(&h, &registry, &cmd_registry);

        match lookup(&h, BindingMode::Normal, &[mode_id], &[KeyChord::char('z')]) {
            LookupResult::Bound { command, .. } => {
                assert_eq!(command.command.command, expected_id);
                assert_eq!(command.layer, KeymapLayer::MinorMode(mode_id));
            }
            other => panic!("expected Bound, got {other:?}"),
        }
    }

    #[test]
    fn translate_skips_synthetic_entries_with_no_command() {
        // Entry with `command = None` is informational
        // (PushDigit / SetPending / etc.); the resolver skips
        // silently. Mode contributes only synthetic entries →
        // no dispatchable binding → no layer push → Unbound
        // lookup.
        let h = KeymapHandle::new();
        let cmd_registry = registry_with_builtins();
        let keymap = Keymap::from_entries(fixture_synthetic_entries());
        let mut registry = ModeRegistry::new();
        let mode_id = registry
            .register(test_mode("test-mode/synthetic", keymap))
            .expect("register");

        translate_mode_keymaps(&h, &registry, &cmd_registry);

        let result = lookup(&h, BindingMode::Normal, &[mode_id], &[KeyChord::char('z')]);
        assert!(matches!(result, LookupResult::Unbound));
    }

    #[test]
    fn translate_warns_and_skips_unresolvable_entry_names() {
        // Entry's canonical command name isn't in the registry
        // (catalog drift). Resolver logs a `tracing::warn!` and
        // skips; nothing dispatchable contributes, layer skipped.
        let h = KeymapHandle::new();
        let cmd_registry = empty_command_registry();
        let keymap = Keymap::from_entries(fixture_unresolvable_entries());
        let mut registry = ModeRegistry::new();
        let mode_id = registry
            .register(test_mode("test-mode/unresolvable", keymap))
            .expect("register");

        translate_mode_keymaps(&h, &registry, &cmd_registry);

        let result = lookup(&h, BindingMode::Normal, &[mode_id], &[KeyChord::char('z')]);
        assert!(matches!(result, LookupResult::Unbound));
    }

    // ---- K.2.4.A.0.4: chain form + table form composability ----

    #[test]
    fn translate_combines_chain_form_with_table_form_entries() {
        // A mode whose `Mode::keymap()` returns a Keymap built
        // from BOTH paths in one chain — typical real-world
        // shape K.2.5 will adopt for multibuffer-mode (static
        // table for the bulk of the bindings + a few
        // dynamically-named bindings from the mode's own
        // CommandInvocations). After translation, BOTH the
        // entry-resolved 'z' chord AND the chain-form '<C-r>'
        // chord must lookup as Bound.
        let h = KeymapHandle::new();
        let cmd_registry = registry_with_builtins();
        let chain_cmd = synthetic_invocation(123);

        let keymap = Keymap::from_entries(fixture_table_form_entries()).bind_chord(
            BindingMode::Normal,
            "<C-r>",
            chain_cmd.clone(),
        );

        let mut registry = ModeRegistry::new();
        let mode_id = registry
            .register(test_mode("test-mode/combined", keymap))
            .expect("register");

        translate_mode_keymaps(&h, &registry, &cmd_registry);

        // Entry-form chord 'z' fires (resolved through registry).
        let expected_id = cmd_registry
            .id_by_name("motion:line-down")
            .expect("motion:line-down should be registered by builtins");
        match lookup(&h, BindingMode::Normal, &[mode_id], &[KeyChord::char('z')]) {
            LookupResult::Bound { command, .. } => {
                assert_eq!(command.command.command, expected_id, "entry-form");
                assert_eq!(command.layer, KeymapLayer::MinorMode(mode_id));
            }
            other => panic!("expected Bound for entry-form 'z', got {other:?}"),
        }

        // Chain-form chord '<C-r>' fires (typed invocation
        // direct, no resolution needed).
        match lookup(&h, BindingMode::Normal, &[mode_id], &[KeyChord::ctrl('r')]) {
            LookupResult::Bound { command, .. } => {
                assert_eq!(command.command, chain_cmd, "chain-form");
                assert_eq!(command.layer, KeymapLayer::MinorMode(mode_id));
            }
            other => panic!("expected Bound for chain-form '<C-r>', got {other:?}"),
        }
    }
}
