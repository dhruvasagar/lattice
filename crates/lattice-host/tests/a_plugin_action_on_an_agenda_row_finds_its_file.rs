//! HB.2b — the whole chain at once: a plugin chord fired on a multibuffer row
//! learns which file the row came from.
//!
//! Every link in this chain had a passing test and the chain answered nothing:
//!
//!   - the resolver, against a view built the way a scan view builds one
//!     (`lattice-multibuffer`'s `excerpt_source_tests`),
//!   - the guest reaching whatever resolver the host carries
//!     (`lattice-plugin-host`'s `excerpt_source_seam`),
//!   - the boot wiring (`boot_regression_pins`' `wired_seams`),
//!   - the id the dispatch gate puts in a grammar action's context
//!     (`an_agenda_row_can_write_to_its_source`).
//!
//! Four green tests, and none of them could say whether pressing a key on an
//! agenda row works — because each supplies by hand the thing the next one
//! produces. This file supplies nothing: a real `Editor`, a real multibuffer
//! view, the real dispatch path, a real WASM guest, and the resolver wired the
//! way `lattice_plugin_loader::install` wires it.
//!
//! The fixture's `multiseam-excerpt-source` echoes the seam's answer using the
//! ids from *its own* context, so a `none` is as visible as a hit.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::Arc;

use lattice_core::{BufferFlags, BufferId, Document as CoreDocument};
use lattice_host::editor::Editor;
use lattice_multibuffer::{Excerpt, MultibufferRegistryHandle, create_multibuffer_view};
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_runtime::spawn_document;

const SOURCE: BufferId = BufferId(311);

fn guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/multiseam-guest/target/wasm32-wasip2/release/multiseam_guest.wasm"
    );
    std::fs::read(path).ok()
}

fn write_plugin_dir(root: &std::path::Path, wasm: &[u8]) {
    let dir = root.join("multiseam");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        "id = \"multiseam\"\nprovides = [\"grammar\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

/// A view shaped like an agenda: excerpts covering only the headline rows of a
/// file whose interesting content (the `SCHEDULED:` line) sits between them
/// and is therefore composed nowhere.
fn agenda_shaped_view(editor: &mut Editor) -> BufferId {
    let cmd_registry: lattice_grammar::CommandRegistryHandle = editor.registry.clone();
    // WITH a path. A scan view's sources are files, and the resolver answers
    // `none` for a source that has none — so a pathless fixture would report
    // exactly the failure this file exists to detect, from its own setup.
    let source = spawn_document(
        SOURCE,
        lattice_core::DocumentBuilder::default()
            .with_text(
                "* TODO water the plants\n  SCHEDULED: <2026-09-03 Thu .+2d>\n* TODO and this\n",
            )
            .with_path(std::path::PathBuf::from("/org/habits.org"))
            .build(),
        cmd_registry.clone(),
    );
    let mut sources: HashMap<BufferId, Arc<dyn lattice_runtime::Document>> = HashMap::new();
    sources.insert(
        SOURCE,
        Arc::new(source) as Arc<dyn lattice_runtime::Document>,
    );

    create_multibuffer_view(
        editor,
        sources,
        // Row 0 is source line 0; row 1 is source line 2. The gap is what makes
        // the answer's LINE meaningful — a seam that echoed the composed line
        // back would pass on row 0 and fail here.
        vec![Excerpt::new(SOURCE, 0, 0), Excerpt::new(SOURCE, 2, 2)],
        Some("*test:agenda*".into()),
        BufferFlags::default(),
        cmd_registry,
        None,
        lattice_multibuffer::FoldGrouping::SourceFile,
    )
}

/// The loader `install` builds, reduced to the services this path needs — and
/// with the excerpt-source resolver wired the SAME way, over the editor's own
/// multibuffer registry. That wiring is the point: leave it out and every
/// assertion below still runs, and reports `none`.
async fn load_the_guest(editor: &Editor, base: &std::path::Path, wasm: &[u8]) {
    let plugins = base.join("plugins");
    write_plugin_dir(&plugins, wasm);

    let host = Arc::new(
        PluginHost::with_dirs(base.join("cache"), base.join("data")).expect("host builds"),
    );
    let views = editor
        .services
        .get::<MultibufferRegistryHandle>()
        .expect("the editor registers its multibuffer registry at boot");
    host.set_excerpt_source_resolver(Arc::new(
        lattice_multibuffer::registry::MultibufferExcerptSource::new((*views).clone()),
    ));

    let loader = PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(editor.event_bus.clone()),
            command_registry: Some(editor.registry.clone()),
            ..Default::default()
        },
    );
    let loaded = loader.discover_and_load(&plugins, TrustTier::Bundled).await;
    assert_eq!(loaded, 1, "the fixture plugin loads");
}

/// Fire the probe with the cursor on composed row `row` and return the echo.
fn ask(editor: &mut Editor, row: u32) -> String {
    editor.cursor = lattice_protocol::position::Position::new(row, 0);
    let id = editor
        .registry
        .load()
        .id_by_name("multiseam-excerpt-source")
        .expect("the fixture action registered");
    let mut out = lattice_host::dispatch::DispatchOutcome::default();
    editor.dispatch_invocation(lattice_grammar::CommandInvocation::of(id), &mut out);
    editor
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .expect("the action echoed")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugin_chord_on_a_row_learns_the_file_behind_it() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("skipping: multiseam guest wasm not built");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let view = agenda_shaped_view(&mut editor);
    load_the_guest(&editor, base.path(), &wasm).await;
    editor.activate_buffer(view);

    let path = editor
        .services
        .get::<MultibufferRegistryHandle>()
        .and_then(|r| r.handle(view))
        .and_then(|v| v.source_path(SOURCE))
        .expect("the view knows its source's path");

    assert_eq!(
        ask(&mut editor, 0),
        format!(
            "excerpt-source({},0)={}@0 in {}",
            view.0,
            path.display(),
            SOURCE.0
        ),
        "a chord on the first row must resolve to source line 0"
    );

    // The row that proves the translation rather than an identity: composed
    // row 1 is source line 2, because the planning line between them is
    // composed nowhere.
    assert_eq!(
        ask(&mut editor, 1),
        format!(
            "excerpt-source({},1)={}@2 in {}",
            view.0,
            path.display(),
            SOURCE.0
        ),
        "composed row 1 is source line 2 — an echo of the composed line would \
         also read plausibly, which is why the excerpts are not contiguous"
    );
}

/// The same guest, the same chord, in an ordinary file buffer: `none`. Without
/// this the test above cannot distinguish a working resolver from one that
/// answers the same thing everywhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_chord_in_a_plain_buffer_is_told_none() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("skipping: multiseam guest wasm not built");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let mut editor = Editor::boot(CoreDocument::from_text("* TODO in a real file\n"));
    let _view = agenda_shaped_view(&mut editor);
    load_the_guest(&editor, base.path(), &wasm).await;

    let answer = ask(&mut editor, 0);
    assert!(
        answer.ends_with("=none"),
        "an ordinary buffer composes nothing, got {answer:?}"
    );
}
