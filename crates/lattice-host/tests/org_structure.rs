//! OM.3 — org promotes and demotes headlines, from inside a plugin.
//!
//! The first slice where the org plugin EDITS. Each chord runs the whole
//! route: `<leader>oh` → the org-mode major's own keymap layer → `Action::Invoke`
//! → the sync grammar trampoline → the guest's `apply-action`, which reads the
//! buffer through its `borrow<document>` handle and returns
//! `Effect::ApplyEdit` → the host's ordinary edit path.
//!
//! Nothing in `lattice-host` knows what a headline is.
//!
//! ## What is asserted, and why through dispatch
//!
//! The plugin's own unit tests (`examples/org-plugin/src/headline.rs`) cover
//! the line logic and run on the host target, but `examples/org-plugin` is not
//! a workspace member so CI never sees them. These tests are the CI gate, and
//! they are the better test anyway: they exercise the seam, the keymap layer,
//! the leader expansion and the edit path, not just the arithmetic.
//!
//! Skips when `examples/org-plugin` was not built (the `org_folds.rs`
//! precedent) — its grammar is fetched on demand and never built by `cargo
//! build`.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_mode::ModeId;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_protocol::{KeyChord, parse_chord_sequence};

fn org_plugin_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/org-plugin/target/wasm32-wasip2/release/org_plugin.wasm"
    );
    std::fs::read(path).ok()
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
            help_topics: Some(editor.help_topics.clone()),
            ..Default::default()
        },
    )
}

/// Boot an editor, load the org plugin into its live registries, and open a
/// `.org` file holding `text`. Returns the editor and the file path.
async fn org_editor(base: &std::path::Path, text: &str) -> Editor {
    let plugins_dir = base.join("plugins");
    let dir = plugins_dir.join("org");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        "id = \"org\"\nprovides = [\"modes\", \"grammar\", \"language\", \"help\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("component.wasm"),
        org_plugin_wasm().expect("caller checked"),
    )
    .unwrap();

    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let loaded = loader_over_editor(&editor, base)
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(loaded, 1, "the org component loads");

    // OM.4b: boot subscribes to `PluginLoaded` and expands a plugin mode's
    // motion / text-object bindings into operator rows on a spawned task. That
    // is right in production and a race in a test that dispatches immediately,
    // so drive the same function directly here. The event-driven wiring has its
    // own test below rather than every test racing it.
    {
        let commands = editor.registry.load();
        for (mode_id, kind) in editor.mode_registry.load().iter_meta() {
            let layer = match kind {
                lattice_mode::ModeKind::Major => lattice_keymap::KeymapLayer::MajorMode(mode_id),
                lattice_mode::ModeKind::Minor => lattice_keymap::KeymapLayer::MinorMode(mode_id),
            };
            lattice_host::keymap_normal::expand_plugin_mode_grammar_rows(
                &editor.keymap,
                &commands,
                &editor.builtins,
                layer,
            );
        }
    }

    let file = base.join("notes.org");
    std::fs::write(&file, text).unwrap();
    editor.do_edit(Some(file), false);
    assert_eq!(
        editor
            .active_modes
            .get(&editor.document_buffer_id)
            .and_then(|m| m.major()),
        Some(ModeId::new("org-mode")),
        "the buffer is in org-mode before any chord is dispatched"
    );
    editor
}

fn chord(s: &str) -> KeyChord {
    parse_chord_sequence(s)
        .expect("parseable chord")
        .into_iter()
        .next()
        .expect("one chord")
}

/// Dispatch a multi-key sequence written with `<leader>`, expanding it the same
/// way the binding did — so the test types what the user types.
fn press(editor: &mut Editor, keys: &str) {
    let expanded = editor.keymap.expand_leader(keys);
    let seq = parse_chord_sequence(&expanded).expect("parses");
    let mut partial: Vec<KeyChord> = Vec::new();
    for c in seq {
        let _ = editor.dispatch_chord(c, &mut partial);
    }
}

/// Put the caret on `line`, column 0.
fn goto_line(editor: &mut Editor, line: u32) {
    editor.cursor.line = line;
    editor.cursor.byte = 0;
}

fn text(editor: &Editor) -> String {
    editor.document.snapshot().text().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn demote_and_promote_a_single_headline() {
    if org_plugin_wasm().is_none() {
        eprintln!("skipping: examples/org-plugin not built");
        return;
    }
    let base = tempfile::tempdir().unwrap();
    let mut editor = org_editor(base.path(), "* One\nbody\n** Child\n").await;

    goto_line(&mut editor, 0);
    press(&mut editor, "<leader>ol");
    assert_eq!(
        text(&editor),
        "** One\nbody\n** Child\n",
        "demote moved only the headline — the child is untouched"
    );

    press(&mut editor, "<leader>oh");
    assert_eq!(
        text(&editor),
        "* One\nbody\n** Child\n",
        "promote is the inverse"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn demote_a_subtree_moves_every_headline_under_it() {
    if org_plugin_wasm().is_none() {
        eprintln!("skipping: examples/org-plugin not built");
        return;
    }
    let base = tempfile::tempdir().unwrap();
    let mut editor = org_editor(base.path(), "* One\nbody\n** Child\nkid body\n* Two\n").await;

    goto_line(&mut editor, 0);
    press(&mut editor, "<leader>oL");
    assert_eq!(
        text(&editor),
        "** One\nbody\n*** Child\nkid body\n* Two\n",
        "the subtree moved together and stopped at the next level-1"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_subtree_edit_is_one_undo_step() {
    if org_plugin_wasm().is_none() {
        eprintln!("skipping: examples/org-plugin not built");
        return;
    }
    let base = tempfile::tempdir().unwrap();
    let original = "* One\nbody\n** Child\n*** Grand\n";
    let mut editor = org_editor(base.path(), original).await;

    goto_line(&mut editor, 0);
    press(&mut editor, "<leader>oL");
    assert_eq!(text(&editor), "** One\nbody\n*** Child\n**** Grand\n");

    // The whole span is replaced as ONE edit precisely so this holds: a single
    // `u` puts every star back, not one headline at a time.
    press(&mut editor, "u");
    assert_eq!(
        text(&editor),
        original,
        "one undo reverses the whole subtree demote"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn promoting_works_from_inside_the_subtree_not_just_on_the_headline() {
    if org_plugin_wasm().is_none() {
        eprintln!("skipping: examples/org-plugin not built");
        return;
    }
    let base = tempfile::tempdir().unwrap();
    let mut editor = org_editor(base.path(), "* One\n** Child\nkid body\n").await;

    // Caret in the child's BODY — the enclosing headline is what moves. This is
    // the common case in practice; the caret is rarely on the headline itself.
    goto_line(&mut editor, 2);
    press(&mut editor, "<leader>oh");
    assert_eq!(
        text(&editor),
        "* One\n* Child\nkid body\n",
        "the enclosing headline promoted, not the one under the caret"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn promoting_past_level_one_refuses_rather_than_flattening() {
    if org_plugin_wasm().is_none() {
        eprintln!("skipping: examples/org-plugin not built");
        return;
    }
    let base = tempfile::tempdir().unwrap();
    let original = "* One\n** Child\n";
    let mut editor = org_editor(base.path(), original).await;

    goto_line(&mut editor, 0);
    press(&mut editor, "<leader>oH");
    assert_eq!(
        text(&editor),
        original,
        "a subtree promote whose root is level 1 is refused whole — shifting \
         only the children would make Child a sibling of One"
    );

    // And the single-headline form refuses too, rather than turning `* One`
    // into body text.
    press(&mut editor, "<leader>oh");
    assert_eq!(text(&editor), original);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_buffer_with_no_headline_leaves_the_chord_to_fall_through() {
    if org_plugin_wasm().is_none() {
        eprintln!("skipping: examples/org-plugin not built");
        return;
    }
    let base = tempfile::tempdir().unwrap();
    // A preamble with no headline anywhere above the caret.
    let original = "#+TITLE: Notes\njust prose\n";
    let mut editor = org_editor(base.path(), original).await;

    goto_line(&mut editor, 1);
    press(&mut editor, "<leader>ol");
    assert_eq!(
        text(&editor),
        original,
        "the action declined; no edit, and nothing on the undo stack"
    );
}

/// Correctness at scale. The interesting property here is what the plugin does
/// NOT do: `shift` reads lines one at a time through the `document` handle
/// rather than materialising the buffer, so a headline near the top of a long
/// file costs the handful of reads between the caret and its headline, not one
/// guest→host call per line. (The read-count bound itself is pinned by
/// `headline.rs`'s own unit test, which runs on the host target; this asserts
/// the behaviour that bound has to preserve.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_file_promotes_correctly_at_its_far_end() {
    if org_plugin_wasm().is_none() {
        eprintln!("skipping: examples/org-plugin not built");
        return;
    }
    let base = tempfile::tempdir().unwrap();
    let mut body = String::from("* One\n");
    for i in 0..5_000 {
        body.push_str(&format!("body {i}\n"));
    }
    body.push_str("** Last\ntail\n");
    let mut editor = org_editor(base.path(), &body).await;

    // Caret in the final subtree's body, 5000 lines below the first headline.
    let last = 5_002;
    goto_line(&mut editor, last);
    press(&mut editor, "<leader>ol");

    let out = text(&editor);
    assert!(
        out.starts_with("* One\nbody 0\n"),
        "the top of the file is untouched"
    );
    assert!(
        out.ends_with("* Last\ntail\n"),
        "the enclosing headline at the far end promoted; got tail: {:?}",
        &out[out.len().saturating_sub(40)..]
    );
}

// ── OM.4: headline motions ────────────────────────────────────────
//
// Two things these tests deliberately do NOT cover, both for reasons found
// while writing them rather than assumed:
//
// * **Counts** (`3]]`). Count accumulation lives at the App layer, above
//   `Editor::dispatch_chord` — a control assertion here showed `3j` on a
//   NATIVE motion also resolving as a single step, so the gap is the harness,
//   not the plugin. Covering it means driving `lattice-ui-tui`'s `press`
//   helper; carried to OM.4b.
// * **Operator composition** (`d]]`). Operator+motion paths are bound
//   explicitly at `KeymapLayer::Builtin` from a hardcoded `motion_rows` table
//   (`keymap_normal.rs:1467`), so a plugin motion bound in Normal is not
//   reachable after an operator — the SAME gap that blocks plugin text
//   objects, and the same fix. OM.4b covers both with one mechanism.

fn cursor_line(editor: &Editor) -> u32 {
    editor.cursor.line
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn headline_motions_walk_forward_and_back() {
    if org_plugin_wasm().is_none() {
        eprintln!("skipping: examples/org-plugin not built");
        return;
    }
    let base = tempfile::tempdir().unwrap();
    let mut editor = org_editor(base.path(), "* One\nbody\n** Child\nkid\n* Two\n").await;

    goto_line(&mut editor, 0);
    press(&mut editor, "]]");
    assert_eq!(cursor_line(&editor), 2, "`]]` walks to the next headline");
    press(&mut editor, "]]");
    assert_eq!(cursor_line(&editor), 4, "at any level");

    press(&mut editor, "[[");
    assert_eq!(cursor_line(&editor), 2, "`[[` walks back");
    press(&mut editor, "[[");
    assert_eq!(cursor_line(&editor), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_headline_motion_at_the_edge_stays_put_rather_than_erroring() {
    if org_plugin_wasm().is_none() {
        eprintln!("skipping: examples/org-plugin not built");
        return;
    }
    let base = tempfile::tempdir().unwrap();
    let mut editor = org_editor(base.path(), "* One\nbody\n* Two\n").await;

    // A motion must resolve somewhere. Returning `err` would log and no-op,
    // which is right for a broken motion and wrong for `]]` at the last one —
    // `}` at the end of a buffer stays put, it does not fail.
    goto_line(&mut editor, 2);
    press(&mut editor, "]]");
    assert_eq!(cursor_line(&editor), 2, "`]]` at the last headline stays");

    goto_line(&mut editor, 0);
    press(&mut editor, "[[");
    assert_eq!(cursor_line(&editor), 0, "`[[` at the first headline stays");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parent_skips_siblings_where_prev_headline_would_not() {
    if org_plugin_wasm().is_none() {
        eprintln!("skipping: examples/org-plugin not built");
        return;
    }
    let base = tempfile::tempdir().unwrap();
    let mut editor = org_editor(base.path(), "* One\n** Two\n*** A\n*** B\n").await;

    // From `*** B`, `[[` reaches its level-3 SIBLING; `g{` must skip it to the
    // level-2 parent. This is the whole reason `g{` is not just `[[`.
    goto_line(&mut editor, 3);
    press(&mut editor, "[[");
    assert_eq!(cursor_line(&editor), 2, "`[[` finds the sibling");

    goto_line(&mut editor, 3);
    press(&mut editor, "g{");
    assert_eq!(
        cursor_line(&editor),
        1,
        "g-brace finds the parent, not the sibling"
    );

    // A level-1 headline has no parent; the cursor stays.
    goto_line(&mut editor, 0);
    press(&mut editor, "g{");
    assert_eq!(cursor_line(&editor), 0, "a level-1 headline has no parent");
}

// ── OM.4b: operators compose with org's grammar ───────────────────
//
// These assert at the KEYMAP, not by dispatching `dar` and checking the text,
// and that is a harness limit rather than a preference. Operator-pending is a
// modal state resolved at the App layer (`input::translate`), above
// `Editor::dispatch_chord` — dispatching `d` here returns `Bound` on the
// operator immediately and the following keys arrive as fresh Normal chords,
// so `dar` reads as `d`, then `a` (append), then a literal `r`. The same
// layering that stopped OM.4 covering counts.
//
// What is proven here is the mechanism: the rows exist, in the right layer,
// for the right operators, with text objects losing their Normal binding and
// motions keeping theirs. Driving `dar` end-to-end needs a `lattice-ui-tui`
// test on the App's `press` helper, and is carried as a named follow-up rather
// than left implied.

/// The boot wiring itself, not the function it calls.
///
/// `org_editor` drives `expand_plugin_mode_grammar_rows` directly so the other
/// tests are deterministic — which leaves the `PluginLoaded` subscription that
/// runs it in production untested, and an unrun expansion is exactly the
/// "works, but only after you press something" bug class. So this one loads
/// WITHOUT the direct call and waits for the event path to land the rows.
///
/// Waiting on a one-shot task, not polling a race: under load it takes longer,
/// and it fails only if the subscription is genuinely not wired.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_boot_subscription_expands_rows_without_any_keypress() {
    let Some(wasm) = org_plugin_wasm() else {
        eprintln!("skipping: examples/org-plugin not built");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    let dir = plugins_dir.join("org");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        "id = \"org\"\nprovides = [\"modes\", \"grammar\", \"language\", \"help\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), &wasm).unwrap();

    let editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let loaded = loader_over_editor(&editor, base.path())
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(loaded, 1);

    let org = ModeId::new("org-mode");
    let seq = parse_chord_sequence("dar").expect("parses");
    for _ in 0..200 {
        if matches!(
            editor
                .keymap
                .lookup_with_context(lattice_keymap::BindingMode::Normal, &seq, &[org]),
            lattice_keymap::LookupResult::Bound { .. }
        ) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("`dar` never gained a row — the PluginLoaded subscription is not wired");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_composable_operator_gets_a_row_not_just_delete() {
    if org_plugin_wasm().is_none() {
        eprintln!("skipping: examples/org-plugin not built");
        return;
    }
    let base = tempfile::tempdir().unwrap();
    let editor = org_editor(base.path(), "* One\nbody\n").await;

    let org = ModeId::new("org-mode");
    for prefix in ["d", "c", "y", "="] {
        let seq = parse_chord_sequence(&format!("{prefix}ar")).expect("parses");
        assert!(
            matches!(
                editor.keymap.lookup_with_context(
                    lattice_keymap::BindingMode::Normal,
                    &seq,
                    &[org]
                ),
                lattice_keymap::LookupResult::Bound { .. }
            ),
            "`{prefix}ar` has a row in org-mode's layer"
        );
        // And ONLY in org-mode's layer — org's objects must not exist in a
        // Rust buffer.
        assert!(
            matches!(
                editor
                    .keymap
                    .lookup_with_context(lattice_keymap::BindingMode::Normal, &seq, &[]),
                lattice_keymap::LookupResult::Unbound
            ),
            "`{prefix}ar` is not global"
        );
    }
}

/// Every object org contributes gets its rows, and so do its motions — the two
/// halves of the gap OM.4b closed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_four_text_objects_and_the_motions_are_reachable_after_an_operator() {
    if org_plugin_wasm().is_none() {
        eprintln!("skipping: examples/org-plugin not built");
        return;
    }
    let base = tempfile::tempdir().unwrap();
    let editor = org_editor(base.path(), "* One\nbody\n").await;

    let org = ModeId::new("org-mode");
    for chord in ["ih", "ah", "ir", "ar", "]]", "[["] {
        let seq = parse_chord_sequence(&format!("d{chord}")).expect("parses");
        assert!(
            matches!(
                editor.keymap.lookup_with_context(
                    lattice_keymap::BindingMode::Normal,
                    &seq,
                    &[org]
                ),
                lattice_keymap::LookupResult::Bound { .. }
            ),
            "`d{chord}` resolves in org-mode's layer"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_text_object_loses_its_normal_binding_but_a_motion_keeps_one() {
    if org_plugin_wasm().is_none() {
        eprintln!("skipping: examples/org-plugin not built");
        return;
    }
    let base = tempfile::tempdir().unwrap();
    let editor = org_editor(base.path(), "* One\nbody\n").await;

    let org = ModeId::new("org-mode");

    // `bind_mode_keymap` writes a Normal terminal binding for every declared
    // chord. The expansion REPLACES it for a text object: `ar` alone in Normal
    // is not something a user can mean.
    let tobj = parse_chord_sequence("ar").expect("parses");
    assert!(
        matches!(
            editor
                .keymap
                .lookup_with_context(lattice_keymap::BindingMode::Normal, &tobj, &[org]),
            lattice_keymap::LookupResult::Unbound
        ),
        "`ar` alone is not bound in Normal"
    );

    // Visual is the exception — selecting an object IS meaningful there.
    assert!(
        matches!(
            editor
                .keymap
                .lookup_with_context(lattice_keymap::BindingMode::Visual, &tobj, &[org]),
            lattice_keymap::LookupResult::Bound { .. }
        ),
        "`ar` is bound in Visual, where it extends the selection"
    );

    // A motion keeps its standalone binding; `]]` still moves on its own.
    let motion = parse_chord_sequence("]]").expect("parses");
    assert!(
        matches!(
            editor
                .keymap
                .lookup_with_context(lattice_keymap::BindingMode::Normal, &motion, &[org]),
            lattice_keymap::LookupResult::Bound { .. }
        ),
        "only text objects lose their Normal binding, not motions"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn org_chords_are_scoped_to_org_buffers() {
    if org_plugin_wasm().is_none() {
        eprintln!("skipping: examples/org-plugin not built");
        return;
    }
    let base = tempfile::tempdir().unwrap();
    let editor = org_editor(base.path(), "* One\n").await;

    // The bindings live in `MajorMode(org-mode)`, a GATED layer. With no modes
    // active they resolve to nothing — so a `.rs` buffer never sees them.
    let expanded = editor.keymap.expand_leader("<leader>ol");
    let seq = parse_chord_sequence(&expanded).unwrap();
    assert!(
        matches!(
            editor
                .keymap
                .lookup_with_context(lattice_keymap::BindingMode::Normal, &seq, &[]),
            lattice_keymap::LookupResult::Unbound
        ),
        "org's chords are not global"
    );
    assert!(
        matches!(
            editor.keymap.lookup_with_context(
                lattice_keymap::BindingMode::Normal,
                &seq,
                &[ModeId::new("org-mode")]
            ),
            lattice_keymap::LookupResult::Bound { .. }
        ),
        "and do resolve when org-mode is the active major"
    );

    // Guard against the sequence being reachable by accident: a lone `<Space>`
    // must not be a terminal binding, or `<leader>ol` could never be typed.
    let space = parse_chord_sequence(&editor.keymap.expand_leader("<leader>")).unwrap();
    assert!(
        !matches!(
            editor.keymap.lookup_with_context(
                lattice_keymap::BindingMode::Normal,
                &space,
                &[ModeId::new("org-mode")]
            ),
            lattice_keymap::LookupResult::Bound { .. }
        ),
        "the leader key alone must stay a prefix, never a terminal binding"
    );

    let _ = chord("x"); // keep the helper used if the assertions above change
}
