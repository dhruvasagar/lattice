//! A closed buffer is announced, under the id its other events carried.
//!
//! Nothing published `Event::DocumentClosed`, so every subscriber that drops
//! per-document state on it (diff sessions, VCS, multibuffer sources and
//! views, LSP reference views, the agent state cache) kept that state for
//! each closed buffer until exit. And the document events disagreed on ids:
//! `DocumentOpened` carried the buffer's id, `DocumentChanged` the core
//! document's own counter. A buffer's document now carries its buffer's id,
//! and every removal from the registry publishes the close.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::time::Duration;

use lattice_core::Document as CoreDocument;
use lattice_diff::DiffAlgorithm;
use lattice_host::action::Action;
use lattice_host::editor::Editor;
use lattice_protocol::event::{Event, EventKind};
use lattice_protocol::ids::DocumentId;
use lattice_runtime::{EventFilter, SubscriptionTarget};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deleted_buffer_is_announced_under_the_id_its_edits_carried() {
    let mut editor = Editor::boot(CoreDocument::from_text("first\n"));
    let first = editor.active_pane_buffer_id();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    editor.event_bus.subscribe(
        EventFilter::kinds(vec![EventKind::DocumentChanged, EventKind::DocumentClosed]),
        SubscriptionTarget::Channel(tx),
    );

    let dir = std::env::temp_dir().join(format!("lattice-doc-closed-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("second.txt");
    std::fs::write(&file, "second\n").unwrap();
    let buf = editor
        .resolve_path_to_buffer(&file)
        .expect("the second buffer opens");
    editor.activate_buffer(buf);
    assert_eq!(editor.active_pane_buffer_id(), buf);
    assert_ne!(buf, first, "a second buffer");
    let doc_id = DocumentId::new(u64::from(buf.0));
    assert_eq!(
        editor.buffers.document_handle(buf).unwrap().snapshot().id,
        doc_id,
        "the document is addressed by its buffer's id"
    );
    let _ = editor.dispatch(Action::Insert("x".to_string()));
    editor
        .diff_subsystem
        .register(buf, DiffAlgorithm::default());
    assert!(editor.diff_subsystem.lookup(buf).is_some());

    assert!(editor.do_buffer_delete(true), "`:bd!` goes through");
    assert_ne!(editor.active_pane_buffer_id(), buf, "the buffer is gone");

    let (mut changed, mut closed) = (None, None);
    tokio::time::timeout(Duration::from_secs(5), async {
        while closed.is_none() {
            match rx.recv().await {
                Some(Event::DocumentChanged { id, .. }) => changed = Some(id),
                Some(Event::DocumentClosed { id }) => closed = Some(id),
                Some(_) => {}
                None => panic!("the bus closed"),
            }
        }
    })
    .await
    .expect("no DocumentClosed was published");
    assert_eq!(changed, Some(doc_id), "the edit carried the buffer's id");
    assert_eq!(closed, Some(doc_id), "and so did the close");

    // The diff subsystem's drain runs after the registry entry is gone, so
    // it must map the id back without looking the buffer up.
    let dropped = tokio::time::timeout(Duration::from_secs(5), async {
        while editor.diff_subsystem.lookup(buf).is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(dropped.is_ok(), "the closed buffer's diff session was kept");
    let _ = std::fs::remove_dir_all(&dir);
}
