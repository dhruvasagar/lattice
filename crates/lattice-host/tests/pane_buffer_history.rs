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
