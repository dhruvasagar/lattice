//! `:w` on a focused multibuffer view — it must persist its sources, and it
//! must RETURN.
//!
//! Written while chasing a reported freeze: on the agenda, toggle a headline
//! state, then `<C-x><C-s>`, and the editor hangs indefinitely. It did NOT
//! reproduce here, and this file is what establishes that — a hand-built
//! agenda-shaped view, with a real file on disk and a dirty source, saves and
//! returns in milliseconds.
//!
//! It is kept because there was no test at all for this path, and because the
//! path has teeth. `<C-x><C-s>` is `:write` → `Editor::do_write`, which has a
//! special case for Oil and none for a multibuffer, so it falls through to
//! `save_blocking` → `block_on(document.save())`. The multibuffer's `save()`
//! then contains two waits with **no timeout**: a `Flush` barrier awaiting the
//! source-forwarder task, and `source.save().await` per source. Both run
//! under `block_on` on the dispatch thread, so if either party never answers
//! the editor freezes with no recovery — which is exactly the reported
//! symptom, whatever stalls it.
//!
//! So the deadline below is not ceremony. A hang here must be a FAILING test
//! rather than a suite that never returns, and the assertion that the source
//! actually reached disk is what stops "it returned" from passing on a `:w`
//! that quietly did nothing.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use lattice_core::{BufferFlags, BufferId, Document as CoreDocument};
use lattice_host::editor::Editor;
use lattice_multibuffer::{Excerpt, create_multibuffer_view};
use lattice_runtime::spawn_document;

const SOURCE: BufferId = BufferId(211);

fn boot_with_an_agenda_shaped_view() -> (Editor, BufferId, std::path::PathBuf) {
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let cmd_registry: lattice_grammar::CommandRegistryHandle = editor.registry.clone();

    // A REAL file on disk, which is what an agenda source is. Without a path
    // `save()` short-circuits and the write half is never exercised.
    let dir = std::env::temp_dir().join(format!(
        "lattice-save-hang-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("notes.org");
    std::fs::write(&file, "* TODO write it\n* TODO and it\n").unwrap();
    let source = spawn_document(
        SOURCE,
        lattice_core::DocumentBuilder::default()
            .with_path(&file)
            .with_text("* TODO write it\n* TODO and it\n")
            .build(),
        cmd_registry.clone(),
    );
    let mut sources: HashMap<BufferId, Arc<dyn lattice_runtime::Document>> = HashMap::new();
    sources.insert(
        SOURCE,
        Arc::new(source) as Arc<dyn lattice_runtime::Document>,
    );

    let view_id = create_multibuffer_view(
        &mut editor,
        sources,
        vec![Excerpt::new(SOURCE, 0, 0)],
        Some("*test:agenda*".into()),
        BufferFlags::default(),
        cmd_registry,
        None,
        lattice_multibuffer::FoldGrouping::SourceFile,
    );
    (editor, view_id, file)
}

/// Run `f` with a deadline. Panics with a clear message if it does not
/// return in time — which is the whole point: a hang must FAIL, not wedge
/// the suite.
fn with_deadline<F: FnOnce() + Send + 'static>(what: &str, secs: u64, f: F) {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        // The editor thread runs inside the shared runtime; without entering
        // it here, `Pending::spawn` panics with "no reactor running" and we
        // measure the harness rather than the bug.
        let _guard = lattice_runtime::shared_runtime().enter();
        f();
        let _ = tx.send(());
    });
    if rx.recv_timeout(Duration::from_secs(secs)).is_err() {
        panic!("HUNG: {what} did not return within {secs}s");
    }
}

/// The reported sequence: change a source through the view, then save.
#[test]
fn saving_a_multibuffer_view_after_editing_a_source_persists_and_returns() {
    with_deadline("do_write on a multibuffer view", 20, || {
        let (mut editor, view, file) = boot_with_an_agenda_shaped_view();
        assert!(editor.activate_buffer(view), "the view must be focusable");
        eprintln!(
            "view = {view:?}, active = {:?}",
            editor.active_pane_buffer_id()
        );

        // The reported sequence: change a headline's state first…
        editor.apply_edit_effect_inline(
            SOURCE,
            lattice_protocol::edit::Edit {
                range: lattice_protocol::position::Range::new(
                    lattice_protocol::position::Position::new(0, 2),
                    lattice_protocol::position::Position::new(0, 6),
                ),
                kind: lattice_protocol::edit::EditKind::Replace {
                    text: "DONE".to_string(),
                },
            },
            None,
        );
        eprintln!("edit applied");

        // …then save.
        editor.do_write(None);
        eprintln!("do_write returned");
        // Did the save actually REACH the multibuffer? If not, "no hang"
        // measures nothing — it measures a `:w` that did nothing.
        let on_disk = std::fs::read_to_string(&file).unwrap();
        eprintln!("on disk after save: {on_disk:?}");
        assert!(
            on_disk.contains("DONE"),
            "`:w` on a focused multibuffer must persist the edited source; \
             got {on_disk:?}"
        );
    });
}

/// The same save with NO preceding edit — the discriminator that would tell
/// "saving a multibuffer hangs" apart from "saving one with a dirty source
/// hangs" if either ever did.
#[test]
fn saving_an_untouched_multibuffer_view_returns() {
    with_deadline("do_write on an untouched multibuffer view", 20, || {
        let (mut editor, view, _file) = boot_with_an_agenda_shaped_view();
        assert!(editor.activate_buffer(view));
        eprintln!("active = {:?}", editor.active_pane_buffer_id());
        editor.do_write(None);
        eprintln!("do_write returned (untouched)");
    });
}

/// **The freeze, reproduced.**
///
/// In production `:w` runs inside the editor actor — a `current_thread`
/// tokio runtime (`editor_actor.rs`) — which is the one ingredient the tests
/// above lack: they enter the multi-thread shared runtime, where the bug
/// cannot appear.
///
/// `MultibufferDocument::save()` builds its future with `Pending::spawn`,
/// which is `tokio::spawn` — **onto whatever runtime is in scope**, i.e. the
/// actor's own single-threaded one. `save_blocking` then blocks that same
/// thread awaiting the result. Nothing is left to drive the task, so the
/// flush barrier never completes, no source is ever written, and the editor
/// is wedged with no recovery.
///
/// The codebase already knows this hazard: `Pending::from_channel`'s doc
/// spells it out — "`spawn` calls `tokio::spawn` and so needs a runtime in
/// scope at the point of construction; this path is reached from the editor
/// actor — a `current_thread` runtime about to `block_on` the result — where
/// spawning the awaiter onto the caller's own runtime is the deadlock". Two
/// mitigations exist for it (`map_ok`, `from_channel`) and `save()` uses
/// neither.
///
/// A normal buffer is unaffected because `RopeDocumentHandle::save` is a
/// mailbox send to an actor already running on the shared runtime — which is
/// exactly the reported asymmetry: editing the org file directly and saving
/// works, saving the agenda view hangs.
#[test]
fn saving_a_multibuffer_from_inside_a_current_thread_runtime_does_not_deadlock() {
    with_deadline("do_write inside a current_thread runtime", 20, || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (mut editor, view, file) = boot_with_an_agenda_shaped_view();
            assert!(editor.activate_buffer(view));
            editor.apply_edit_effect_inline(
                SOURCE,
                lattice_protocol::edit::Edit {
                    range: lattice_protocol::position::Range::new(
                        lattice_protocol::position::Position::new(0, 2),
                        lattice_protocol::position::Position::new(0, 6),
                    ),
                    kind: lattice_protocol::edit::EditKind::Replace {
                        text: "DONE".to_string(),
                    },
                },
                None,
            );
            editor.do_write(None);
            let on_disk = std::fs::read_to_string(&file).unwrap();
            assert!(
                on_disk.contains("DONE"),
                "the source must reach disk, got {on_disk:?}"
            );
        });
    });
}
