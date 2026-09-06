//! OS.0 — verification slice: does an Insert-mode plugin chord reach its
//! guest action, and does `Effect::Declined` from such an action fall through
//! to the builtin binding underneath?
//!
//! `keymap_insert.rs`'s `action_from_bound` builds `Action::Invoke(inv)`,
//! which resolves through the unified dispatcher against the grammar
//! registry, where a plugin's `register_action` entries live — so it
//! *should* work, the same way `fall_through.rs` already proves for NORMAL
//! mode. But `plugin-actions-need-a-dispatch-fallback` records this class
//! biting twice for other seams, and eight later org bindings (OS.4, OS.8)
//! depend on both facts holding for INSERT specifically. This is a pinning
//! test, not a feature: if either fact does not hold, the fix belongs in
//! generic Insert-mode dispatch, not here.
//!
//! Extends the existing `multiseam-guest` fixture (shared with
//! `fall_through.rs`, `multiseam.rs`, and a dozen other seam tests) with a
//! THIRD mode, `multiseam-insert-mode`, rather than minting a new fixture —
//! it owns two Insert-mode bindings: `<M-CR>` -> an action that fires
//! (`multiseam-insert-fires`), and `<C-t>` -> the same declining action
//! `multiseam-mode` binds in Normal (`multiseam-declines`). Skips when the
//! fixture wasn't built (no `wasm32-wasip2` target — the established
//! convention every multiseam-guest test in this repo follows).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_core::Document as CoreDocument;
use lattice_host::action::EchoMessage;
use lattice_host::editor::Editor;
use lattice_mode::ModeId;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};

/// The `multiseam-guest` fixture component, or `None` when it wasn't built —
/// the same path `fall_through.rs` reads.
fn multiseam_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/multiseam-guest/target/wasm32-wasip2/release/multiseam_guest.wasm"
    );
    std::fs::read(path).ok()
}

fn write_plugin_dir(root: &std::path::Path, id: &str, provides: &str, wasm: &[u8]) {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        format!("id = \"{id}\"\nprovides = [{provides}]\n"),
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

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
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            ..Default::default()
        },
    )
}

/// Bring the buffer's minors live (publish MajorEntered + drain the
/// resolver) — the `fall_through.rs` precedent, needed because a `Global`
/// activation policy is still available-but-off (CI.3) until enabled AND the
/// buffer's major-entered signal fires.
fn activate_modes(editor: &mut Editor) {
    let _ = editor.drain_minor_activation();
    let proto = lattice_protocol::ids::BufferId::new(editor.document_buffer_id.0 as u64);
    editor
        .event_bus
        .publish(lattice_protocol::Event::MajorEntered {
            buffer: proto,
            major: "text-mode".into(),
        });
    let _ = editor.drain_minor_activation();
}

/// Boot a sealed editor, load the `multiseam-guest` fixture into it, and
/// enable + activate `multiseam-insert-mode` — the mode owning the two
/// Insert-mode bindings this slice pins. `None` when the fixture wasn't
/// built, so callers skip the same way every other multiseam-guest test
/// does.
async fn boot_sealed_editor_with_fixture() -> Option<Editor> {
    let wasm = multiseam_guest_wasm()?;
    lattice_plugin_loader::disable_autoload();

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(
        &plugins_dir,
        "multiseam",
        "\"grammar\", \"modes\", \"config\"",
        &wasm,
    );

    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    editor.cursor = lattice_protocol::position::Position::new(0, 0);

    let loaded = loader_over_editor(&editor, base.path())
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(loaded, 1, "the multiseam plugin loads");

    {
        let mut next = (**editor.mode_registry.load()).clone();
        next.set_minor_enabled(ModeId::new("multiseam-insert-mode"), true);
        editor.mode_registry.store(std::sync::Arc::new(next));
    }
    activate_modes(&mut editor);

    // The plugin is loaded and its registries (event bus, command registry,
    // keymap, mode registry) are all Arc-backed handles the editor keeps its
    // own clones of — nothing after this point reads the plugin's on-disk
    // `plugin.toml` / `component.wasm` again, so the tempdir can be leaked
    // rather than kept alive for the rest of the test body (this helper
    // returns an owned `Editor`, not a borrow tied to `base`'s lifetime).
    std::mem::forget(base);

    Some(editor)
}

/// Dispatch a single chord through the real editor path — the org test
/// suite's `press` helper (`lattice-org-plugin/tests/org_structure.rs`),
/// reused in shape here: a name-dispatch test would pass against a mode-
/// scoping bug, a missing binding, or a dead prefix alike, so every
/// assertion in this file goes through a pressed chord, never a dispatched
/// action id.
fn press(editor: &mut Editor, keys: &str) {
    let expanded = editor.keymap.expand_leader(keys);
    let seq = lattice_protocol::parse_chord_sequence(&expanded).expect("parses");
    let mut partial = Vec::new();
    for c in seq {
        let _ = editor.dispatch_chord(c, &mut partial);
    }
}

/// Did the fixture's `multiseam-insert-fires` action (callback 40) run? It
/// echoes a fixed marker string rather than anything computed, so this is a
/// strong assertion: it is true only if `apply-action` actually executed
/// that specific guest callback, not merely if SOME echo happened to be set.
fn fixture_action_fired(editor: &Editor) -> bool {
    matches!(
        &editor.last_message,
        Some(EchoMessage { text, .. }) if text == "multiseam-insert-fired"
    )
}

/// Replace the WHOLE active buffer's text via the ordinary edit path
/// (`Editor::apply_edit_blocking`) — the same path a keystroke-originated
/// edit takes, so this is a real edit application rather than a bypass of
/// the document actor.
fn set_buffer_text(editor: &mut Editor, text: &str) {
    let buffer = editor.active_text();
    // `byte_to_position` is rope-aware about the trailing-newline "phantom
    // last line" ropey reports — unlike hand-computing an end line/byte from
    // `content_line_count`, which undercounts the range by one line for text
    // ending in `\n` and leaves the old trailing newline behind.
    let end = buffer
        .byte_to_position(buffer.byte_len() as usize)
        .expect("end-of-buffer position resolves");
    let range = lattice_protocol::position::Range::new(
        lattice_protocol::position::Position::new(0, 0),
        end,
    );
    editor
        .apply_edit_blocking(lattice_protocol::edit::Edit::replace(
            range,
            text.to_string(),
        ))
        .expect("full-buffer replace applies");
    editor.cursor = lattice_protocol::position::Position::new(0, 0);
}

fn buffer_text(editor: &Editor) -> String {
    editor.active_text().as_string()
}

/// Step 1 — an Insert-mode plugin chord reaches its guest action.
///
/// `multiseam-insert-mode` binds `<M-CR>` (Insert) to `multiseam-insert-fires`,
/// a grammar action the fixture registers solely for this purpose. Pressing
/// `i` then `<M-CR>` must run that action and leave its marker echo behind.
/// If the chord is silently unbound (a mode-scoping bug, a missing binding,
/// or Insert-mode dispatch simply not resolving plugin-contributed layers),
/// `last_message` stays whatever it was before — `None` on a fresh editor —
/// and the assertion fails loudly rather than passing on an absent effect.
///
/// **OS.0 CONFIRMED this failed, and named why; OS.0b fixed it.** Before
/// OS.0b the chord did nothing — `last_message` stayed `"-- INSERT --"`
/// (the echo `i` itself leaves behind), never `"multiseam-insert-fired"`.
///
/// Root cause, confirmed by a throwaway diagnostic (not committed) that
/// called `KeymapHandle::lookup_with_context` directly with the mode active:
/// the RAW `<M-CR>` chord (ALT preserved) resolves `Bound` against
/// `multiseam-insert-mode`'s layer — so the binding registered correctly and
/// sits in the trie exactly where it should. But `dispatch_insert`'s
/// `normalize_for_insert_lookup` unconditionally stripped ALT and SUPER off
/// EVERY incoming Insert-mode chord before ANY lookup — Builtin, MinorMode,
/// and MajorMode alike — so the real per-keystroke dispatch path looked up
/// plain `<CR>` instead, which resolved to the Builtin binding (insert
/// newline) with no route back to the mode's layer at all. The doc comment
/// at the top of that file stated this as by-design ("no Insert binding
/// (base or overlay) uses [ALT/SUPER]") — true of every BUILTIN binding, but
/// the `modes` WIT seam (`wit/modes.wit`'s `binding-mode: insert`) makes no
/// such promise to plugins, and a plugin that declares an ALT-bearing Insert
/// chord (the org design's `<M-CR>` shape) registered successfully and then
/// could never fire.
///
/// **OS.0b (`crates/lattice-host/src/keymap_insert.rs`'s
/// `lookup_insert_chord`)** made the lookup try the chord AS PRESSED first,
/// falling back to the normalized form only when the raw lookup finds
/// nothing — so a deliberately ALT/SUPER-bearing binding is reachable and a
/// chord that never carried either modifier still costs exactly one lookup.
/// This test is the acceptance criterion for that fix: it must PASS,
/// unmodified, now that the fix has landed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_insert_mode_plugin_chord_reaches_its_guest_action() {
    let Some(mut editor) = boot_sealed_editor_with_fixture().await else {
        eprintln!("skipping: multiseam-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };
    assert!(
        editor.last_message.is_none(),
        "sanity: a fresh editor has no pending echo to false-positive against"
    );
    // Sanity: the binding registered and sits in the trie where it should —
    // isolates "the chord never reached apply-action" (what this test is
    // about) from "the mode never registered the binding at all" (a
    // different bug `boot_sealed_editor_with_fixture` would already be
    // getting wrong).
    let insert_mode = ModeId::new("multiseam-insert-mode");
    let alt_cr = lattice_protocol::parse_chord_sequence("<M-CR>").expect("chord parses");
    assert!(
        matches!(
            editor.keymap.lookup_with_context(
                lattice_keymap::BindingMode::Insert,
                &alt_cr,
                &[insert_mode],
            ),
            lattice_keymap::LookupResult::Bound { .. }
        ),
        "`<M-CR>` binds to the plugin's firing action when the mode is active"
    );

    press(&mut editor, "i"); // Normal -> Insert
    press(&mut editor, "<M-CR>");

    assert!(
        fixture_action_fired(&editor),
        "an Insert-mode plugin binding must reach apply-action; last_message was {:?}",
        editor.last_message
    );
}

/// Step 2 — `Effect::Declined` from an Insert-mode plugin action falls
/// through to the builtin binding underneath.
///
/// `multiseam-insert-mode` ALSO binds `<C-t>` (Insert) to the fixture's
/// `multiseam-declines` action — the same action `multiseam-mode` binds to
/// Normal `x` in `fall_through.rs`, here reached from Insert instead. `<C-t>`
/// is a Builtin Insert binding (indent the current line by one shiftwidth;
/// `keymap_entry.rs:473`), so if decline fall-through works the SAME way in
/// Insert as `fall_through.rs` proved for Normal, the buffer ends up indented
/// by one shiftwidth (4 spaces, the default). If fall-through does not fire
/// for Insert, the buffer is left unchanged (the plugin action ran and did
/// nothing) — a different, distinguishable failure from the chord being
/// unbound (which Step 1 already covers separately).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_declined_insert_chord_falls_through_to_the_builtin() {
    let Some(mut editor) = boot_sealed_editor_with_fixture().await else {
        eprintln!("skipping: multiseam-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };
    set_buffer_text(&mut editor, "hello\n");
    assert_eq!(
        buffer_text(&editor),
        "hello\n",
        "sanity: the buffer holds exactly the seeded text before any chord"
    );
    // Sanity: `<C-t>` really is bound to the plugin's DECLINING action while
    // the mode is active — without this, a pass here would be indistinguishable
    // from "the mode was never active and the builtin ran on its own", which
    // is not what this test claims to prove.
    let insert_mode = ModeId::new("multiseam-insert-mode");
    let ctrl_t = lattice_protocol::parse_chord_sequence("<C-t>").expect("chord parses");
    assert!(
        matches!(
            editor.keymap.lookup_with_context(
                lattice_keymap::BindingMode::Insert,
                &ctrl_t,
                &[insert_mode],
            ),
            lattice_keymap::LookupResult::Bound { .. }
        ),
        "`<C-t>` binds to the plugin's declining action when the mode is active"
    );

    press(&mut editor, "i");
    press(&mut editor, "<C-t>");

    assert_eq!(
        buffer_text(&editor),
        "    hello\n",
        "Declined in Insert must reach the builtin shiftwidth indent"
    );
}
