//! Regression: a grammar PLUGIN action dispatched through the real
//! `dispatch_invocation` path reads the ACTIVE buffer via its AP.0.1 `document`
//! handle — not an empty scratch document.
//!
//! The `CommandKind::Action` gate historically fed `Document::empty()` to
//! `execute` (native actions don't read the buffer). AP.0.1 gave plugin actions
//! a `document` handle, but this path still handed them an empty document — so
//! auto-pair's close-skip (which peeks the char after the caret) saw nothing and
//! always inserted. The isolated seam tests missed it by calling `execute()` with
//! a real doc directly; this asserts the REAL editor dispatch path.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};

fn grammar_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/grammar-guest/target/wasm32-wasip2/release/grammar_guest.wasm"
    );
    std::fs::read(path).ok()
}

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

fn loader_over_editor(editor: &Editor, base: &std::path::Path) -> PluginLoader {
    let host =
        Arc::new(PluginHost::with_dirs(base.join("cache"), base.join("data")).expect("host builds"));
    PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(editor.event_bus.clone()),
            command_registry: Some(editor.registry.clone()),
            ..Default::default()
        },
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_action_reads_the_real_active_buffer_through_dispatch() {
    let Some(wasm) = grammar_guest_wasm() else {
        eprintln!("skipping: grammar-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "grammar-fixture", "grammar", &wasm);

    // A buffer with real content; caret on the 'e' of "hello" (0,1).
    let mut editor = Editor::boot(CoreDocument::from_text("hello world\n"));
    editor.cursor = lattice_protocol::position::Position::new(0, 1);

    let loaded = loader_over_editor(&editor, base.path())
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(loaded, 1, "the grammar plugin loads");

    // `read-at-cursor` reads the byte at the caret via the `document` handle and
    // echoes "<char>@<line>:<byte>". Dispatch it through the REAL invocation path.
    let id = editor
        .registry
        .load()
        .id_by_name("read-at-cursor")
        .expect("the fixture action registered");
    let mut out = lattice_host::dispatch::DispatchOutcome::default();
    editor.dispatch_invocation(lattice_grammar::CommandInvocation::of(id), &mut out);

    // If the action had read an empty scratch document, get-text-range would have
    // errored (out of range) — no echo, or an error message. The real buffer read
    // yields the 'e' at the caret.
    let msg = editor.last_message.expect("the action echoed a message");
    assert_eq!(
        msg.text, "e@0:1",
        "the plugin action read the REAL active buffer at the caret, not an empty scratch"
    );
}
