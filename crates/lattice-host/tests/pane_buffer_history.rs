//! Pane buffer history (PBH series) — host integration tests.
//!
//! Design: `docs/dev/architecture/pane-buffer-history.md`.
//! Slice plan: `docs/dev/operations/slice-plans/pane-buffer-history.md`.
//!
//! The pure walk semantics are unit-tested in
//! `lattice_host::pane_history`. These cover the parts that need a real
//! `Editor` + pane tree: the side table's lifecycle across splits and
//! closes, which is where the headline requirement lives.

use lattice_core::Document as CoreDocument;
use lattice_core::ui::pane::SplitOrientation;
use lattice_host::editor::Editor;
use lattice_host::pane_history::{PaneBufferHistory, PaneHistoryEntry};

fn boot() -> Editor {
    Editor::boot(CoreDocument::from_text("line-0\nline-1\nline-2\n"))
}

fn buffers_of(h: &PaneBufferHistory) -> Vec<u32> {
    h.entries().iter().map(|e| e.buffer.0).collect()
}

/// THE headline requirement: splitting a pane starts a fresh trail.
///
/// A split is a new place to work, not a copy of where you have been.
/// This holds by construction rather than by a reset step —
/// `PaneId::next()` never reuses ids, so the new leaf has no side-table
/// entry at all. The test pins the behaviour so a future move of the
/// history into `PaneState` (which is `Copy`, and whose `..new_state`
/// split would silently inherit it) fails loudly here.
#[test]
fn splitting_a_pane_does_not_inherit_its_history() {
    let mut e = boot();

    // Give the original pane a non-trivial trail.
    let original_id = e.pane_tree.active().id;
    {
        let h = e.active_pane_history_mut();
        h.push(PaneHistoryEntry::at_origin(lattice_core::BufferId(2)), 100);
        h.push(PaneHistoryEntry::at_origin(lattice_core::BufferId(3)), 100);
    }
    let before = buffers_of(&e.pane_buffer_history[&original_id]);
    assert_eq!(before.len(), 3, "sanity: original has a real trail");

    e.pane_tree.split_active(SplitOrientation::Vertical);
    let new_id = e
        .pane_tree
        .leaves()
        .iter()
        .map(|l| l.id)
        .find(|id| *id != original_id)
        .expect("split created a second pane");

    assert!(
        !e.pane_buffer_history.contains_key(&new_id),
        "a freshly split pane must not inherit history; it has none until it navigates",
    );
    assert_eq!(
        buffers_of(&e.pane_buffer_history[&original_id]),
        before,
        "the original pane's trail must be untouched by the split",
    );
}

/// The new pane's history, once it exists, holds exactly one entry —
/// the buffer it is showing — not a copy of the source pane's trail.
#[test]
fn a_split_pane_starts_with_only_its_current_buffer() {
    let mut e = boot();
    {
        let h = e.active_pane_history_mut();
        h.push(PaneHistoryEntry::at_origin(lattice_core::BufferId(2)), 100);
        h.push(PaneHistoryEntry::at_origin(lattice_core::BufferId(3)), 100);
    }

    let new_idx = e.pane_tree.split_active(SplitOrientation::Vertical);
    e.pane_tree.set_active(new_idx);

    let h = e.active_pane_history_mut();
    assert_eq!(
        h.len(),
        1,
        "the new pane seeds with its current buffer only, not the source trail",
    );
    assert_eq!(h.cursor(), 0);
}

/// Seeding uses the pane's committed buffer, so the first `<C-6>` after
/// one switch has an origin to go back *from*.
#[test]
fn seeding_uses_the_panes_current_buffer() {
    let mut e = boot();
    let committed = e.pane_tree.active().committed_id();
    let h = e.active_pane_history_mut();
    assert_eq!(h.current().map(|c| c.buffer), Some(committed));
}

/// Reconciliation reaps closed panes. Keyed reaping (retain what the
/// tree still has) rather than hooking each removal path — `close_active`
/// and `collapse_to_active` both drop leaves today.
#[test]
fn reconcile_reaps_a_closed_panes_history() {
    let mut e = boot();
    let original_id = e.pane_tree.active().id;
    let _ = e.active_pane_history_mut();

    let new_idx = e.pane_tree.split_active(SplitOrientation::Vertical);
    e.pane_tree.set_active(new_idx);
    let new_id = e.pane_tree.active().id;
    let _ = e.active_pane_history_mut();
    assert_eq!(e.pane_buffer_history.len(), 2, "both panes have history");

    // Close the pane we're in.
    assert!(e.pane_tree.close_active());
    e.reconcile_pane_history();

    assert!(
        !e.pane_buffer_history.contains_key(&new_id),
        "the closed pane's history must be reaped",
    );
    assert!(
        e.pane_buffer_history.contains_key(&original_id),
        "the surviving pane's history must be kept",
    );
}

/// `collapse_to_active` (`<C-w>o` / `:only`) drops *siblings* rather than
/// the active pane — the removal path most likely to be missed by a
/// hook-each-site approach, and the reason reconciliation is keyed off
/// the tree instead.
#[test]
fn reconcile_reaps_siblings_dropped_by_collapse_to_active() {
    let mut e = boot();
    let _ = e.active_pane_history_mut();
    let survivor = e.pane_tree.active().id;

    let a = e.pane_tree.split_active(SplitOrientation::Vertical);
    e.pane_tree.set_active(a);
    let _ = e.active_pane_history_mut();
    let b = e.pane_tree.split_active(SplitOrientation::Horizontal);
    e.pane_tree.set_active(b);
    let _ = e.active_pane_history_mut();
    assert_eq!(e.pane_buffer_history.len(), 3);

    // Keep the ORIGINAL pane, drop the two we made.
    let survivor_idx = e
        .pane_tree
        .leaves()
        .iter()
        .position(|l| l.id == survivor)
        .expect("survivor still in the tree");
    e.pane_tree.set_active(survivor_idx);
    assert!(e.pane_tree.collapse_to_active());
    e.reconcile_pane_history();

    assert_eq!(
        e.pane_buffer_history.len(),
        1,
        "only the surviving pane's history remains",
    );
    assert!(e.pane_buffer_history.contains_key(&survivor));
}

/// Reconciliation is idempotent and safe with nothing to reap — it runs
/// on ordinary pane transitions, not just closes.
#[test]
fn reconcile_is_a_no_op_when_every_pane_is_live() {
    let mut e = boot();
    let _ = e.active_pane_history_mut();
    let before = e.pane_buffer_history.len();
    e.reconcile_pane_history();
    e.reconcile_pane_history();
    assert_eq!(e.pane_buffer_history.len(), before);
}

// ---- PBH.2: recording at the activation chokepoint ----

use lattice_core::{BufferFlags, BufferId};
use lattice_host::buffer_registry::{BufferData, BufferEntry, DocumentEntry};
use std::sync::Arc;

/// Add a real registry Document WITHOUT activating it.
fn add_document(editor: &mut Editor, raw_id: u32, text: &str, name: &str) -> BufferId {
    let bid = BufferId(raw_id);
    let handle =
        lattice_runtime::spawn_document(bid, CoreDocument::from_text(text), editor.registry.clone());
    let arc: Arc<dyn lattice_runtime::Document> = Arc::new(handle);
    editor.buffers.insert(BufferEntry {
        id: bid,
        flags: BufferFlags {
            listed: true,
            hidden: false,
            ephemeral: false,
        },
        data: BufferData::Document(DocumentEntry {
            id: bid,
            handle: Arc::clone(&arc),
        }),
        name: Some(name.to_string()),
    });
    bid
}

fn active_trail(e: &mut Editor) -> Vec<u32> {
    buffers_of(e.active_pane_history_mut())
}

/// A real buffer switch is recorded on the pane's trail.
#[test]
fn activating_a_buffer_records_it() {
    let mut e = boot();
    let origin = e.pane_tree.active().committed_id();
    let b = add_document(&mut e, 900, "b\n", "*B*");

    e.activate_buffer(b);

    assert_eq!(
        active_trail(&mut e),
        vec![origin.0, b.0],
        "the origin buffer and the newly activated one both appear",
    );
}

/// Re-activating the buffer the pane already shows is not a visit.
/// Guarded twice on purpose — at the chokepoint and inside `push` — so
/// neither alone is load-bearing.
#[test]
fn reactivating_the_same_buffer_does_not_record() {
    let mut e = boot();
    let b = add_document(&mut e, 901, "b\n", "*B*");
    e.activate_buffer(b);
    let before = active_trail(&mut e);

    e.activate_buffer(b);
    e.activate_buffer(b);

    assert_eq!(active_trail(&mut e), before, "no duplicate entries");
}

/// Several switches build a trail in visit order, including a return to
/// a buffer already visited (which is a distinct third stop, not a
/// dedup).
#[test]
fn a_trail_records_visit_order_including_returns() {
    let mut e = boot();
    let origin = e.pane_tree.active().committed_id();
    let b = add_document(&mut e, 902, "b\n", "*B*");
    let c = add_document(&mut e, 903, "c\n", "*C*");

    e.activate_buffer(b);
    e.activate_buffer(c);
    e.activate_buffer(b);

    assert_eq!(active_trail(&mut e), vec![origin.0, b.0, c.0, b.0]);
}

/// The cursor is captured onto the entry being LEFT, so walking back
/// returns to where the user actually was rather than the top of file.
#[test]
fn leaving_a_buffer_captures_its_outgoing_cursor() {
    let mut e = boot();
    let b = add_document(&mut e, 904, "b0\nb1\nb2\n", "*B*");

    e.cursor = lattice_protocol::position::Position::new(2, 1);
    e.activate_buffer(b);

    let h = e.active_pane_history_mut();
    let origin_entry = h.entries()[0];
    assert_eq!(
        origin_entry.cursor,
        lattice_protocol::position::Position::new(2, 1),
        "the position we left the origin buffer at is stored on its entry",
    );
}

/// LOAD-BEARING: previews must not pollute the trail.
///
/// Picker previews route through `set_preview_override`, which leaves
/// the pane's committed `buffer_id` untouched and is projected only into
/// the published render state — so they never reach
/// `activate_buffer_only` and cannot be recorded. That is a property of
/// the existing preview-isolation architecture rather than a filter this
/// feature adds, which is exactly why it deserves a pin: a future
/// refactor that routed preview through activation would silently fill
/// every user's history with junk from scrolling a picker.
#[test]
fn previewing_buffers_does_not_record_history() {
    use lattice_host::preview::PreviewOverride;

    let mut e = boot();
    let pane = e.pane_tree.active().id;
    let b = add_document(&mut e, 905, "b\n", "*B*");
    let c = add_document(&mut e, 906, "c\n", "*C*");

    let before = active_trail(&mut e);

    // Scroll a picker across two candidates.
    for id in [b, c] {
        e.set_preview_override(
            pane,
            PreviewOverride {
                buffer_id: id,
                buffer: lattice_core::BufferKind::Document,
                cursor: lattice_protocol::position::Position::ZERO,
                scroll: 0,
            },
        );
    }
    e.clear_preview_override(pane);

    assert_eq!(
        active_trail(&mut e),
        before,
        "previewing must leave the pane's buffer trail untouched",
    );
}

// ---- PBH.3: walking the trail ----

/// Back then forward returns you to where you started.
#[test]
fn walking_back_and_forward_round_trips() {
    let mut e = boot();
    let origin = e.pane_tree.active().committed_id();
    let b = add_document(&mut e, 910, "b\n", "*B*");
    let c = add_document(&mut e, 911, "c\n", "*C*");
    e.activate_buffer(b);
    e.activate_buffer(c);

    e.do_pane_history(-1);
    assert_eq!(e.pane_tree.active().committed_id(), b);
    e.do_pane_history(-1);
    assert_eq!(e.pane_tree.active().committed_id(), origin);
    e.do_pane_history(1);
    assert_eq!(e.pane_tree.active().committed_id(), b);
    e.do_pane_history(1);
    assert_eq!(e.pane_tree.active().committed_id(), c);
}

/// THE invariant that makes forward reachable: a walk must not record.
///
/// If the step back pushed an entry it would truncate the very tail it
/// was moving into, and `<C-7>` could never return anything.
#[test]
fn walking_does_not_record_new_entries() {
    let mut e = boot();
    let b = add_document(&mut e, 912, "b\n", "*B*");
    let c = add_document(&mut e, 913, "c\n", "*C*");
    e.activate_buffer(b);
    e.activate_buffer(c);
    let before = active_trail(&mut e);

    e.do_pane_history(-1);
    e.do_pane_history(-1);
    e.do_pane_history(1);

    assert_eq!(
        active_trail(&mut e),
        before,
        "walking must move the cursor, never append",
    );
}

/// Walking back then opening a new buffer drops the forward tail —
/// browser semantics, end to end.
#[test]
fn visiting_after_walking_back_truncates_the_tail() {
    let mut e = boot();
    let origin = e.pane_tree.active().committed_id();
    let b = add_document(&mut e, 914, "b\n", "*B*");
    let c = add_document(&mut e, 915, "c\n", "*C*");
    let d = add_document(&mut e, 916, "d\n", "*D*");
    e.activate_buffer(b);
    e.activate_buffer(c);

    e.do_pane_history(-1); // back to B
    e.activate_buffer(d); // C's tail is dropped

    assert_eq!(active_trail(&mut e), vec![origin.0, b.0, d.0]);
}

/// The cursor you left a buffer at is restored when you walk back to it.
#[test]
fn walking_back_restores_the_cursor_you_left() {
    let mut e = boot();
    let b = add_document(&mut e, 917, "b0\nb1\nb2\n", "*B*");

    e.cursor = lattice_protocol::position::Position::new(2, 0);
    e.activate_buffer(b);
    e.do_pane_history(-1);

    assert_eq!(
        e.cursor,
        lattice_protocol::position::Position::new(2, 0),
        "walking back should land where the user actually was",
    );
}

/// Both ends echo rather than wrapping — a directional key that cycles
/// is a worse key.
#[test]
fn walking_past_either_end_echoes_and_does_not_wrap() {
    let mut e = boot();
    let origin = e.pane_tree.active().committed_id();
    let b = add_document(&mut e, 918, "b\n", "*B*");
    e.activate_buffer(b);

    e.do_pane_history(-1);
    assert_eq!(e.pane_tree.active().committed_id(), origin);
    e.do_pane_history(-1);
    assert_eq!(
        e.pane_tree.active().committed_id(),
        origin,
        "must not wrap around to the newest entry",
    );
    let msg = e.last_message.as_ref().expect("an echo at the end");
    assert!(msg.text.contains("oldest"), "got {:?}", msg.text);

    e.do_pane_history(1);
    e.do_pane_history(1);
    assert_eq!(e.pane_tree.active().committed_id(), b);
    let msg = e.last_message.as_ref().expect("an echo at the end");
    assert!(msg.text.contains("newest"), "got {:?}", msg.text);
}

/// Each pane walks its own trail — the whole point of per-pane history.
#[test]
fn panes_walk_independent_trails() {
    let mut e = boot();
    let origin = e.pane_tree.active().committed_id();
    let b = add_document(&mut e, 919, "b\n", "*B*");
    let c = add_document(&mut e, 920, "c\n", "*C*");

    // Pane 1 visits B.
    e.activate_buffer(b);

    // Split; the new pane visits C and walks back.
    let new_idx = e.pane_tree.split_active(SplitOrientation::Vertical);
    e.pane_tree.set_active(new_idx);
    e.activate_buffer(c);
    e.do_pane_history(-1);
    assert_eq!(
        e.pane_tree.active().committed_id(),
        b,
        "the split pane's trail starts at what it was showing (B), not pane 1's origin",
    );

    // Pane 1's own trail is untouched by any of that.
    let first_idx = e
        .pane_tree
        .leaves()
        .iter()
        .position(|l| l.id != e.pane_tree.active().id)
        .expect("two panes");
    e.pane_tree.set_active(first_idx);
    assert_eq!(active_trail(&mut e), vec![origin.0, b.0]);
}

/// A buffer removed from the registry (`:bd`) is pruned from the trail
/// as the walk passes it, rather than failing the switch.
///
/// Liveness is registry presence, not document-ness: in-pane synthetic
/// buffers (help, oil, file tree) legitimately appear in a trail, so a
/// `document_ids_sorted`-based check here would silently prune all of
/// them.
#[test]
fn walking_skips_a_buffer_that_left_the_registry() {
    let mut e = boot();
    let origin = e.pane_tree.active().committed_id();
    let b = add_document(&mut e, 921, "b\n", "*B*");
    let c = add_document(&mut e, 922, "c\n", "*C*");
    e.activate_buffer(b);
    e.activate_buffer(c);

    // B goes away while we're sitting on C.
    e.buffers.remove(b);

    e.do_pane_history(-1);

    assert_eq!(
        e.pane_tree.active().committed_id(),
        origin,
        "the walk should skip the deleted buffer and land on the one before it",
    );
    assert!(
        !active_trail(&mut e).contains(&b.0),
        "the dead entry should be dropped, not retained",
    );
}
