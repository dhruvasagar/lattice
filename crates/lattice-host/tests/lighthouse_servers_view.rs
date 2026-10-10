//! LH.2 — a keypress in a real `*lsp-servers*` buffer.
//!
//! Lighthouse is tested at three levels below this one: its logic against a
//! fake host, its seams through the real component, and its manifest through
//! the real loader. Each stops short of the same join. The loader test proves
//! the row chords are bound in `lighthouse-servers-mode`'s layer; the
//! component test proves the actions do the right thing *given* a cursor
//! line. Neither presses a key.
//!
//! Between them sits the editor's generic path, and three places on it where
//! this could be wired end to end and still do nothing:
//!
//! * `:lsp-servers` returns an effect naming a PLUGIN's manual minor as
//!   `activate-minor`. If the editor does not activate it — a plugin minor is
//!   filtered on enablement, and this one has no `default_modes` gate — the
//!   buffer opens, looks right, and its keys are dead.
//! * The buffer's major is `plugin-output-mode`, which implies
//!   `read-only-mode`, whose job is to refuse `x`. The mode's `x` has to win.
//! * The action reads the server off the cursor's line of the REAL buffer,
//!   which the host filled asynchronously from the plugin's output store.
//!
//! So this loads the shipped component with its shipped manifest into a
//! booted editor, runs the command, and presses keys.
//!
//! Skips when the component was not built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use lattice_config::ConfigRegistry;
use lattice_core::Document as CoreDocument;
use lattice_host::dispatch::DispatchOutcome;
use lattice_host::editor::Editor;
use lattice_mode::{BufferStoreHandle, ModeId};
use lattice_plugin_host::output::PluginOutputHandle;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_protocol::position::Position;

const LIST: &str = "*lsp-servers*";
const SERVERS_MODE: &str = "lighthouse-servers-mode";

fn plugin_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../plugins/lighthouse/target/wasm32-wasip2/release/lighthouse.wasm"
    );
    std::fs::read(path).ok()
}

/// The plugin as it ships: the real manifest beside the real component.
fn write_plugin_dir(root: &std::path::Path, wasm: &[u8]) {
    let dir = root.join("lighthouse");
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../plugins/lighthouse/plugin.toml"
    ))
    .unwrap();
    std::fs::write(dir.join("plugin.toml"), manifest).unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

/// Boot an editor and load lighthouse into ITS registries, writing to ITS
/// output store — the store `plugin-output-mode` seeds from.
async fn boot() -> Option<Editor> {
    let wasm = plugin_wasm()?;
    lattice_plugin_loader::disable_autoload();

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, &wasm);

    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    editor.cursor = Position::new(0, 0);

    let host = Arc::new(
        PluginHost::with_dirs(base.path().join("cache"), base.path().join("data"))
            .expect("host builds"),
    );
    let output = editor
        .services
        .get::<PluginOutputHandle>()
        .expect("the editor registers a plugin-output store at boot");
    host.set_plugin_output((*output).clone());
    let loader = PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(editor.event_bus.clone()),
            command_registry: Some(editor.registry.clone()),
            mode_registry: Some(editor.mode_registry.clone()),
            keymap: Some(editor.keymap.clone()),
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            help_topics: Some(lattice_help::topics::builtin_topics().into_handle()),
            ..Default::default()
        },
    );
    let loaded = loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(loaded, 1, "lighthouse loads from its shipped manifest");

    // The registries are Arc-backed handles the editor holds its own clones
    // of, and the plugin's tasks own their instances; keep the loader and the
    // directories alive for the test's duration by leaking them.
    std::mem::forget(loader);
    std::mem::forget(base);
    Some(editor)
}

/// Apply the buffer-opening effects a dispatch deferred to the renderer.
///
/// Opening a buffer is renderer-coupled: the host hands the effect back in
/// the `DispatchOutcome` and each renderer applies it — the TUI through
/// exactly this call (`app/messages.rs`). There is no renderer here, so the
/// test stands in for that one step and nothing else.
fn apply_opens(editor: &mut Editor, out: DispatchOutcome) {
    for effect in out.effects {
        if let lattice_grammar::Effect::OpenSyntheticBuffer {
            name,
            mode_id,
            content,
            cursor,
            activate_minor,
        } = effect
        {
            editor.open_synthetic_buffer_seeded(
                &name,
                &mode_id,
                content.as_deref(),
                cursor,
                activate_minor.as_deref(),
            );
        }
    }
}

fn press(editor: &mut Editor, keys: &str) {
    let expanded = editor.keymap.expand_leader(keys);
    let seq = lattice_protocol::parse_chord_sequence(&expanded).expect("parses");
    let mut partial = Vec::new();
    for c in seq {
        let (_, out) = editor.dispatch_chord_with_outcome(c, &mut partial);
        apply_opens(editor, out);
    }
}

fn echo(editor: &Editor) -> String {
    editor
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default()
}

/// A named buffer's lines, as its document actor has them.
fn lines_of(editor: &Editor, name: &str) -> Vec<String> {
    let Some(id) = editor.buffers.by_name(name) else {
        return Vec::new();
    };
    let store = editor.services.get::<BufferStoreHandle>().unwrap();
    let Some(handle) = store.handle_for(id) else {
        return Vec::new();
    };
    let snap = handle.snapshot();
    (0..snap.buffer.rope_line_count())
        .map(|n| snap.buffer.line(n).unwrap_or_default())
        .collect()
}

/// Wait — on the editor's own wake, with no key pressed — until the list has
/// a row for rust-analyzer. Returns the lines last seen.
async fn list_drawn(editor: &Editor) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let lines = lines_of(editor, LIST);
        if lines.iter().any(|l| l.contains("rust-analyzer"))
            || tokio::time::Instant::now() >= deadline
        {
            return lines;
        }
        let _ =
            tokio::time::timeout(Duration::from_millis(250), editor.async_landed.notified()).await;
    }
}

fn active_buffer_name(editor: &Editor) -> String {
    let store = editor.services.get::<BufferStoreHandle>().unwrap();
    store
        .name_for(editor.active_buffer_id())
        .unwrap_or_default()
}

/// Run `:lsp-servers`, wait for the list, and put the cursor on
/// rust-analyzer's row.
async fn open_list(editor: &mut Editor) -> Vec<String> {
    let mut out = DispatchOutcome::default();
    editor.execute_ex_line("lsp-servers", &mut out);
    apply_opens(editor, out);
    assert_eq!(
        active_buffer_name(editor),
        LIST,
        "the command opened the list and made it the active buffer"
    );
    let lines = list_drawn(editor).await;
    let row = lines
        .iter()
        .position(|l| l.trim_start().starts_with("rust-analyzer"))
        .unwrap_or_else(|| panic!("the list never drew a rust-analyzer row: {lines:?}"));
    editor.cursor = Position::new(row as u32, 0);
    lines
}

/// The list is drawn — by the plugin, through the output store, into a real
/// buffer — and the mode that owns its keys is active on it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lsp_servers_opens_a_drawn_list_with_its_mode_active() {
    let Some(mut editor) = boot().await else {
        eprintln!("skipping: lighthouse wasm not built (no wasm32-wasip2 target)");
        return;
    };
    let lines = open_list(&mut editor).await;

    assert_eq!(lines[0], "  Server         Version     Status");
    assert!(
        lines[1].starts_with("  rust-analyzer  ") && lines[1].ends_with("not installed"),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("i install")),
        "the keys are on the page: {lines:?}"
    );

    let list = editor.active_buffer_id();
    assert!(
        editor.minor_mode_enabled_for(list, ModeId::new(SERVERS_MODE)),
        "`activate-minor` activated the plugin's manual minor on the list — \
         without it the buffer looks right and its keys are dead"
    );
}

/// `x` on a row reaches the plugin's action, which reads that row.
///
/// `x` is the hard one: the buffer is read-only, and `read-only-mode` exists
/// to refuse exactly this key. And outside the list, `x` must still be vim's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn x_on_a_row_is_the_plugins_and_x_elsewhere_is_still_vims() {
    let Some(mut editor) = boot().await else {
        eprintln!("skipping: lighthouse wasm not built (no wasm32-wasip2 target)");
        return;
    };

    // Control, in an ordinary buffer with the plugin loaded: `x` deletes.
    press(&mut editor, "x");
    assert_eq!(
        editor.active_text().as_string(),
        "cratch\n",
        "the plugin's `x` must not exist outside its own buffer"
    );

    let before = open_list(&mut editor).await;
    press(&mut editor, "x");

    assert_eq!(
        echo(&editor),
        "lsp-servers: 'rust-analyzer' is not installed",
        "the chord reached the plugin's action, and the action read the \
         server off the cursor's row"
    );
    assert_eq!(
        lines_of(&editor, LIST),
        before,
        "and nothing was deleted from the list"
    );
}

/// Off a server's row there is nothing to act on, and the action says so
/// rather than guessing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_row_key_on_the_heading_says_there_is_no_server_there() {
    let Some(mut editor) = boot().await else {
        eprintln!("skipping: lighthouse wasm not built (no wasm32-wasip2 target)");
        return;
    };
    open_list(&mut editor).await;
    editor.cursor = Position::new(0, 0);

    press(&mut editor, "u");

    assert_eq!(echo(&editor), "lsp-servers: no server on this line");
}

/// `<CR>` on a row opens that server's install log.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enter_on_a_row_opens_that_servers_log() {
    let Some(mut editor) = boot().await else {
        eprintln!("skipping: lighthouse wasm not built (no wasm32-wasip2 target)");
        return;
    };
    open_list(&mut editor).await;

    press(&mut editor, "<CR>");

    assert_eq!(active_buffer_name(&editor), "*lsp-install:rust-analyzer*");
    let log = editor.active_buffer_id();
    assert!(
        !editor.minor_mode_enabled_for(log, ModeId::new(SERVERS_MODE)),
        "the row keys belong to the list, not to a log"
    );
}
