//! OM.2 — opening a `.org` file activates `org-mode`, a major mode that ships
//! inside a plugin.
//!
//! This is the slice's headline claim and the one that failed before it. The
//! host resolves a document's major through
//! `lattice_syntax::major_mode_id_for_lang`, a hand-written `match` over the
//! `Lang` enum whose `Lang::Plugin(_)` arm returns `None` **by design** — the
//! host cannot have an arm for a language it has never heard of. So however
//! good the org plugin's grammar was, a `.org` buffer landed in `text-mode`.
//!
//! The route was already designated (that arm's comment says a plugin
//! language's major "is contributed through the `modes` seam by the plugin that
//! owns it"); OM.1 built the registry's language index and OM.2 opened the seam
//! to majors. This test walks the whole path with the REAL reference plugin:
//! discover → load → `language` + `modes` seams drain → open a file → the
//! editor's ordinary activation resolves org-mode.
//!
//! ## Mode-ownership acid test
//!
//! Nothing in `lattice-host` knows what org is. No `Editor::` method, no
//! `Action` variant, no `BufferKind`, no `Lang` arm. The plugin contributed a
//! language and the major that owns it, and the generic resolver did the rest.
//!
//! Skips when `examples/org-plugin` was not built — it is deliberately not a
//! workspace member (its grammar is 2.2 MB of generated C fetched on demand),
//! so `cargo build` never produces it. The `org_folds.rs` precedent.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_mode::{ModeId, ModeKind};
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader, discover};

/// The reference org component, if it was built. Built out-of-workspace with
/// `cd examples/org-plugin && cargo build --release --target wasm32-wasip2`.
fn org_plugin_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/org-plugin/target/wasm32-wasip2/release/org_plugin.wasm"
    );
    std::fs::read(path).ok()
}

/// Lay the plugin out on disk the way the manager would. Note the `provides`
/// order is deliberately NOT dependency order — OM.0 made the loader sort, and
/// org is exactly the plugin that would have suffered from it.
fn write_org_plugin_dir(root: &std::path::Path, wasm: &[u8]) {
    let dir = root.join("org");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        "id = \"org\"\nprovides = [\"modes\", \"language\", \"help\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

/// A loader wired to the booted editor's LIVE registries, so the plugin's mode
/// lands in the same `mode_registry` the editor's activation reads.
fn loader_over_editor(editor: &Editor, base: &std::path::Path) -> PluginLoader {
    let host = Arc::new(
        PluginHost::with_dirs(base.join("cache"), base.join("data")).expect("host builds"),
    );
    PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(editor.event_bus.clone()),
            command_registry: Some(editor.registry.clone()),
            mode_registry: Some(editor.mode_registry.clone()),
            keymap: Some(editor.keymap.clone()),
            help_topics: Some(editor.help_topics.clone()),
            ..Default::default()
        },
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opening_an_org_file_activates_the_plugins_major_mode() {
    let Some(wasm) = org_plugin_wasm() else {
        eprintln!(
            "skipping: examples/org-plugin not built \
             (cd examples/org-plugin && cargo build --release --target wasm32-wasip2)"
        );
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_org_plugin_dir(&plugins_dir, &wasm);

    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));

    // --- Before load: nothing in the editor knows what org is.
    assert!(
        !editor
            .mode_registry
            .load()
            .is_registered(ModeId::new("org-mode")),
        "org-mode is not a built-in"
    );

    assert_eq!(discover(&plugins_dir).len(), 1, "discovery finds org");
    let loaded = loader_over_editor(&editor, base.path())
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(loaded, 1, "the org component loads");

    // --- The mode registered, AS A MAJOR, and claimed the language its own
    // `language` seam registered in the same load.
    let org_mode = ModeId::new("org-mode");
    let registry = editor.mode_registry.load();
    let entry = registry
        .get(org_mode)
        .expect("org-mode registered from the component");
    assert_eq!(
        entry.kind(),
        ModeKind::Major,
        "it registered as a major, not downgraded to a minor"
    );
    assert_eq!(
        registry.find_major_for_lang("org"),
        Some(org_mode),
        "and owns `org` in the registry's language index"
    );
    drop(registry);

    // --- Open a real `.org` file through the ordinary `:e` path.
    let file = base.path().join("notes.org");
    std::fs::write(&file, "* Top level\n** Second\nbody text\n").unwrap();
    editor.do_edit(Some(file.clone()), false);

    // The language seam's extension mapping resolved `.org`...
    let detected = lattice_syntax::Lang::detect_from_path(Some(file.as_path()));
    assert_eq!(
        detected.name(),
        "org",
        "the plugin's `language` seam claimed the .org extension"
    );

    // ...and the editor's GENERIC activation put the buffer in the plugin's
    // major. No org-specific code ran anywhere in the host to make this true.
    let buffer = editor.document_buffer_id;
    let major = editor
        .active_modes
        .get(&buffer)
        .and_then(|m| m.major())
        .expect("the buffer has an active major");
    assert_eq!(
        major, org_mode,
        "a .org buffer activates the plugin's major (it was text-mode before OM.2)"
    );
}

/// Graceful degradation, and the shape a partially-working install takes: the
/// `language` seam can land without the `modes` seam — a plugin may declare
/// only `language`, or its mode declaration may be rejected. An org buffer then
/// highlights and folds perfectly well in `text-mode`, which is a good outcome,
/// not an error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugin_language_without_its_major_still_opens_in_text_mode() {
    let Some(wasm) = org_plugin_wasm() else {
        eprintln!("skipping: examples/org-plugin not built");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    let dir = plugins_dir.join("org-lang-only");
    std::fs::create_dir_all(&dir).unwrap();
    // `provides` names the language seam ONLY — the component still exports
    // `register-modes`, but a seam the manifest does not declare is never
    // driven.
    std::fs::write(
        dir.join("plugin.toml"),
        "id = \"org-lang-only\"\nprovides = [\"language\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), &wasm).unwrap();

    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let loaded = loader_over_editor(&editor, base.path())
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(loaded, 1, "the language-only plugin loads");

    assert!(
        !editor
            .mode_registry
            .load()
            .is_registered(ModeId::new("org-mode")),
        "no modes seam declared, so no org-mode"
    );

    let file = base.path().join("notes.org");
    std::fs::write(&file, "* Top level\n").unwrap();
    editor.do_edit(Some(file), false);

    let buffer = editor.document_buffer_id;
    let major = editor
        .active_modes
        .get(&buffer)
        .and_then(|m| m.major())
        .expect("the buffer has an active major");
    assert_eq!(
        major,
        ModeId::new("text-mode"),
        "an unclaimed plugin language falls back to text-mode, not to nothing"
    );
}
