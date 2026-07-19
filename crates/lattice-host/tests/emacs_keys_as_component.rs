//! PL8.G — modes-as-components, from the guest side.
//!
//! The design's §5.8.3 goal is that major/minor modes can ship as WASM
//! components, not only as native Rust. The mode seam (`spawn_mode_plugin`) and
//! the loader drain (`drain_mode`) already register a *discovered* mode plugin's
//! declarations (proven by `lattice-plugin-loader/tests/mode_drain.rs` at the
//! keymap-lookup level). This test closes the remaining claim: a mode shipped as
//! a **component** reaches the REAL dispatch path and behaves identically to a
//! native mode — the same `Editor::dispatch_chord` → keymap-composite →
//! `Action::Invoke` route the native `emacs-keys-mode` takes in
//! `emacs_keys_dispatch.rs`.
//!
//! The subject is the emacs-keys leader tribute itself, re-expressed as a
//! component (`tests/fixtures/emacs-keys-guest`, mode id `emacs-keys-plugin-mode`,
//! `Universal`). It binds two component-EXCLUSIVE leader chords the native mode
//! does not (`<C-x>e` → split, `<C-x>w` → write), so a resolved dispatch is
//! unambiguously the component's (native `<C-x>` and this component's `<C-x>`
//! layers merge into one composite trie at lookup — the chord native lacks can
//! only come from the component).
//!
//! ## Mode-ownership acid test, from the guest side
//!
//! The component contributes its ENTIRE surface — the mode declaration AND its
//! `MinorMode(emacs-keys-plugin-mode)` keymap layer — through the existing seam.
//! This test adds ZERO `Editor::` methods and ZERO `Action` variants in
//! `lattice-host`: the plugin mode reaches dispatch through the same generic
//! path builtins use. That is the mode-ownership acid test passing from the
//! guest side (CLAUDE.md standing rule).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_core::Document as CoreDocument;
use lattice_host::action::Action;
use lattice_host::editor::Editor;
use lattice_keymap::{BindingMode, LookupResult};
use lattice_mode::ModeId;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader, discover};
use lattice_protocol::{KeyChord, parse_chord_sequence};

/// The emacs-keys component fixture, if the `wasm32-wasip2` build produced it.
/// Resolved by its known path (this crate can't read the plugin-host build
/// script's env var); skip when absent (no `wasm32-wasip2` target).
fn emacs_keys_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/emacs-keys-guest/target/wasm32-wasip2/release/emacs_keys_guest.wasm"
    );
    std::fs::read(path).ok()
}

/// Lay a discovered-plugin dir out on disk: `plugin.toml` (id + provides) plus
/// the component bytes (the `discover_one` shape the loader expects).
fn write_plugin_dir(root: &std::path::Path, id: &str, provides: &str, wasm: &[u8]) {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        format!("id = \"{id}\"\nprovides = [\"{provides}\"]\n"),
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

/// First `KeyChord` of a parsed sequence (each string here is a single chord,
/// e.g. `"<C-x>"` or `"e"`).
fn chord(s: &str) -> KeyChord {
    parse_chord_sequence(s)
        .expect("parseable chord")
        .into_iter()
        .next()
        .expect("one chord")
}

/// A loader wired to the booted editor's LIVE registries — so a mode the plugin
/// registers lands in the same `mode_registry` / `keymap` / command registry the
/// editor's activation + dispatch read. Its `PluginHost` is tempdir-backed (a
/// dev-only wasmtime runtime), separate from whatever the editor's boot-installed
/// loader points at, keeping this test hermetic.
fn loader_over_editor(editor: &Editor, base: &std::path::Path) -> PluginLoader {
    let host =
        Arc::new(PluginHost::with_dirs(base.join("cache"), base.join("data")).expect("host builds"));
    PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(editor.event_bus.clone()),
            command_registry: Some(editor.registry.clone()),
            mode_registry: Some(editor.mode_registry.clone()),
            keymap: Some(editor.keymap.clone()),
            ..Default::default()
        },
    )
}

/// Bring the buffer's `Universal` minor modes live the way the running app does:
/// publish `MajorEntered` and drain the generic minor-activation resolver. After
/// the plugin mode is registered this pulls it into the buffer's active-modes
/// set, so its gated `MinorMode` layer is in dispatch scope.
fn activate_modes(editor: &mut Editor) {
    let _ = editor.drain_minor_activation(); // clear any boot-queued events
    let proto = lattice_protocol::ids::BufferId::new(editor.document_buffer_id.0 as u64);
    editor
        .event_bus
        .publish(lattice_protocol::Event::MajorEntered {
            buffer: proto,
            major: "text-mode".into(),
        });
    let _ = editor.drain_minor_activation();
}

/// Assert an `Action::Invoke` targets the command registered under `name`.
fn assert_invokes(editor: &Editor, action: &Action, name: &str) {
    let Action::Invoke(inv) = action else {
        panic!("expected Action::Invoke({name}), got {action:?}");
    };
    let expected = editor
        .registry
        .load()
        .id_by_name(name)
        .unwrap_or_else(|| panic!("command `{name}` is registered"));
    assert_eq!(inv.command, expected, "leader chord must target `{name}`");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn emacs_keys_leader_shipped_as_a_component_dispatches_like_native() {
    let Some(wasm) = emacs_keys_guest_wasm() else {
        eprintln!("skipping: emacs-keys-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "emacs-keys-fixture", "modes", &wasm);

    let mut editor = Editor::boot(CoreDocument::from_text(
        "alpha beta gamma\nsecond line\nthird line\n",
    ));

    // --- Before load: the component-exclusive `<C-x>e` is not a binding. The
    // native `<C-x>` leader has no `e` suffix, so the plugin mode's id isn't
    // even in the registry yet. Guards that the assertion below proves the
    // COMPONENT, not a pre-existing native binding.
    let seq = parse_chord_sequence("<C-x>e").expect("parses");
    assert!(
        matches!(
            editor.keymap.lookup_with_context(
                BindingMode::Normal,
                &seq,
                &[ModeId::new("emacs-keys-plugin-mode")],
            ),
            LookupResult::Unbound
        ),
        "before load, `<C-x>e` is unbound even with the (unregistered) plugin mode listed"
    );

    // --- Load the emacs-keys mode as a component through the loader, into the
    // editor's live registries.
    assert_eq!(discover(&plugins_dir).len(), 1, "discovery finds the plugin");
    let loaded = loader_over_editor(&editor, base.path())
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(loaded, 1, "the emacs-keys component loads");

    // The plugin mode is now in the (republished) registry alongside the native
    // foundation modes — a distinct id, so no collision with native
    // `emacs-keys-mode`.
    assert!(
        editor
            .mode_registry
            .load()
            .is_registered(ModeId::new("emacs-keys-plugin-mode")),
        "the component's minor mode registered into the editor's mode registry"
    );

    // --- Ownership / gating: the `<C-x>e` binding lives in the component's OWN
    // gated `MinorMode(emacs-keys-plugin-mode)` layer — resolves only when that
    // mode is active, never globally.
    assert!(
        matches!(
            editor.keymap.lookup_with_context(
                BindingMode::Normal,
                &seq,
                &[ModeId::new("emacs-keys-plugin-mode")],
            ),
            LookupResult::Bound { .. }
        ),
        "with the plugin mode active, `<C-x>e` resolves in its owned layer"
    );
    assert!(
        matches!(
            editor
                .keymap
                .lookup_with_context(BindingMode::Normal, &seq, &[]),
            LookupResult::Unbound
        ),
        "with no modes active, the gated `<C-x>e` binding does not fire"
    );

    // CI.3: a plugin-declared minor mode is registered available-but-OFF (the
    // user, not the plugin author, owns activation). Enable it — as an init.rs
    // `on-plugin-loaded` handler would — before it can auto-activate.
    {
        let mut next = (**editor.mode_registry.load()).clone();
        next.set_minor_enabled(ModeId::new("emacs-keys-plugin-mode"), true);
        editor.mode_registry.store(std::sync::Arc::new(next));
    }

    // --- The real proof: activate the mode and dispatch the leader chord through
    // the SAME path native modes take. `<C-x>e` must resolve the pane split and
    // actually execute it — a component-shipped mode driving a real editor
    // action, indistinguishable from native at the dispatch layer.
    activate_modes(&mut editor);
    assert_eq!(editor.pane_tree.len(), 1, "one pane before the leader chord");

    let mut partial: Vec<KeyChord> = Vec::new();
    let _ = editor.dispatch_chord(chord("<C-x>"), &mut partial);
    let action = editor.dispatch_chord(chord("e"), &mut partial);
    assert_invokes(&editor, &action, "action:split-pane-horizontal");
    assert_eq!(
        editor.pane_tree.len(),
        2,
        "`<C-x>e` from the component split the pane end-to-end"
    );

    // And the second binding (`<C-x>w` → `ex:write`) resolves to the built-in ex
    // command — asserted at the keymap layer so the test drives no real disk
    // write, only proves the binding is live.
    let write_seq = parse_chord_sequence("<C-x>w").expect("parses");
    let LookupResult::Bound { command, .. } = editor.keymap.lookup_with_context(
        BindingMode::Normal,
        &write_seq,
        &[ModeId::new("emacs-keys-plugin-mode")],
    ) else {
        panic!("`<C-x>w` should be bound in the plugin mode's layer");
    };
    assert_eq!(
        command.command.command,
        editor.registry.load().id_by_name("ex:write").unwrap(),
        "`<C-x>w` must target the built-in `ex:write`"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn emacs_keys_component_without_a_wired_mode_registry_is_skipped_not_fatal() {
    let Some(wasm) = emacs_keys_guest_wasm() else {
        eprintln!("skipping: emacs-keys-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "emacs-keys-fixture", "modes", &wasm);

    let editor = Editor::boot(CoreDocument::from_text("alpha\n"));

    // A loader wired to the editor's command registry + keymap but NO mode
    // registry: the modes drain hits `NotWired("modes")`, which
    // `discover_and_load` logs + skips (graceful degradation — never a panic or
    // a corrupted editor).
    let host = Arc::new(
        PluginHost::with_dirs(base.path().join("c"), base.path().join("d")).expect("host builds"),
    );
    let loader = PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(editor.event_bus.clone()),
            command_registry: Some(editor.registry.clone()),
            keymap: Some(editor.keymap.clone()),
            mode_registry: None,
            ..Default::default()
        },
    );

    let loaded = loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(loaded, 0, "the mode component is skipped, not loaded");
    assert!(
        !editor
            .mode_registry
            .load()
            .is_registered(ModeId::new("emacs-keys-plugin-mode")),
        "nothing registered into the editor's mode registry"
    );
}
