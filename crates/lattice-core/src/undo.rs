//! Undo stack.
//!
//! Phase 0 ships a linear undo stack: each entry is the inverse of one applied
//! edit (or one batch of edits applied as a single command). The branching
//! undo *tree* per §5.1 is a later refinement; the linear stack is forward
//! compatible because branching is built by retaining alternative redo paths
//! when a new edit is applied while redo entries exist.

use lattice_protocol::edit::Edit;

/// One entry on the undo stack: an ordered list of edits whose application
/// inverts the user-visible operation. Storing a list (not a single Edit) lets
/// a batch of edits applied atomically be undone atomically.
#[derive(Debug, Clone)]
pub struct UndoEntry {
    pub inverse_edits: Vec<Edit>,
    /// Description for status messages / dot-repeat. Empty for unnamed batches.
    pub label: String,
}

#[derive(Debug, Default, Clone)]
pub struct UndoStack {
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
}

impl UndoStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a new undo entry. Any pending redo history is dropped.
    pub fn push(&mut self, entry: UndoEntry) {
        self.undo.push(entry);
        self.redo.clear();
    }

    /// Take the most recent undo entry and move it onto the redo stack as
    /// `redo_entry`. The caller is expected to apply `entry.inverse_edits` to
    /// the buffer and pass the resulting "inverse-of-the-inverse" back via
    /// `record_redo`.
    pub fn pop_for_undo(&mut self) -> Option<UndoEntry> {
        self.undo.pop()
    }

    /// Reciprocal of `pop_for_undo`. Stores the edit set that would replay the
    /// undone operation onto the redo stack.
    pub fn record_redo(&mut self, redo_entry: UndoEntry) {
        self.redo.push(redo_entry);
    }

    /// Take the most recent redo entry and move it back onto the undo side.
    pub fn pop_for_redo(&mut self) -> Option<UndoEntry> {
        self.redo.pop()
    }

    pub fn record_undo(&mut self, undo_entry: UndoEntry) {
        self.undo.push(undo_entry);
    }

    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use lattice_protocol::edit::Edit;
    use lattice_protocol::position::Position;

    fn entry(label: &str) -> UndoEntry {
        UndoEntry {
            inverse_edits: vec![Edit::insert(Position::ZERO, "x")],
            label: label.into(),
        }
    }

    #[test]
    fn new_stack_is_empty() {
        let s = UndoStack::new();
        assert_eq!(s.undo_depth(), 0);
        assert_eq!(s.redo_depth(), 0);
    }

    #[test]
    fn push_increments_undo_depth() {
        let mut s = UndoStack::new();
        s.push(entry("a"));
        s.push(entry("b"));
        assert_eq!(s.undo_depth(), 2);
        assert_eq!(s.redo_depth(), 0);
    }

    #[test]
    fn pop_for_undo_returns_in_lifo_order() {
        let mut s = UndoStack::new();
        s.push(entry("a"));
        s.push(entry("b"));
        let top = s.pop_for_undo().unwrap();
        assert_eq!(top.label, "b");
        let next = s.pop_for_undo().unwrap();
        assert_eq!(next.label, "a");
        assert!(s.pop_for_undo().is_none());
    }

    #[test]
    fn record_redo_pushes_to_redo_stack() {
        let mut s = UndoStack::new();
        s.push(entry("a"));
        let popped = s.pop_for_undo().unwrap();
        s.record_redo(popped);
        assert_eq!(s.undo_depth(), 0);
        assert_eq!(s.redo_depth(), 1);
    }

    #[test]
    fn pop_for_redo_returns_in_lifo_order() {
        let mut s = UndoStack::new();
        s.record_redo(entry("first"));
        s.record_redo(entry("second"));
        assert_eq!(s.pop_for_redo().unwrap().label, "second");
        assert_eq!(s.pop_for_redo().unwrap().label, "first");
        assert!(s.pop_for_redo().is_none());
    }

    #[test]
    fn push_clears_pending_redo() {
        // Standard undo invariant: making a new edit while there is a redo
        // history must drop that history (the user has diverged onto a new
        // branch). The branching tree variant in §5.1 will preserve it; the
        // linear stack does not.
        let mut s = UndoStack::new();
        s.push(entry("a"));
        let popped = s.pop_for_undo().unwrap();
        s.record_redo(popped);
        assert_eq!(s.redo_depth(), 1);

        s.push(entry("b"));
        assert_eq!(s.redo_depth(), 0);
    }

    #[test]
    fn record_undo_does_not_clear_redo() {
        // record_undo is the bookkeeping primitive used by `Document::redo`
        // -- it should NOT clear the redo stack the way `push` does.
        let mut s = UndoStack::new();
        s.record_redo(entry("r"));
        s.record_undo(entry("u"));
        assert_eq!(s.undo_depth(), 1);
        assert_eq!(s.redo_depth(), 1);
    }

    #[test]
    fn full_undo_redo_dance() {
        let mut s = UndoStack::new();
        s.push(entry("op"));
        let popped = s.pop_for_undo().unwrap();
        // pretend we re-applied the inverse and computed an inverse-of-inverse:
        s.record_redo(entry("inv-of-inv"));
        let redone = s.pop_for_redo().unwrap();
        s.record_undo(entry("re-recorded"));
        assert_eq!(s.undo_depth(), 1);
        assert_eq!(s.redo_depth(), 0);
        let _ = (popped, redone);
    }
}
