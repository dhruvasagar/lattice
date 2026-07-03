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

    /// Fold `inverses` into the most recent undo entry instead of
    /// pushing a new one -- the primitive behind undo-group coalescing
    /// (a vim insert session collapses to a single undo unit). The
    /// caller passes the just-applied operation's inverse edits in the
    /// same stored order [`push`] would use (reverse-application order);
    /// they are prepended so the combined entry still replays
    /// newest -> oldest during undo (`inv(eN) .. inv(e1)`).
    ///
    /// Redo is intentionally not cleared: an amend never diverges the
    /// history (the initiating `push` that opened the group already
    /// cleared redo, and no redo can accrue mid-group). If there is no
    /// top entry to amend -- which the group bookkeeping is meant to
    /// prevent -- it falls back to a plain push so the edit stays
    /// undoable rather than being silently lost.
    pub fn amend_top(&mut self, mut inverses: Vec<Edit>) {
        match self.undo.last_mut() {
            Some(top) => {
                inverses.append(&mut top.inverse_edits);
                top.inverse_edits = inverses;
            }
            None => self.undo.push(UndoEntry {
                inverse_edits: inverses,
                label: String::new(),
            }),
        }
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
    fn amend_top_prepends_into_the_latest_entry() {
        // Two edits folded into one entry: the second edit's inverse is
        // prepended so undo replays newest -> oldest. Depth stays 1.
        let mut s = UndoStack::new();
        s.push(UndoEntry {
            inverse_edits: vec![Edit::insert(Position::ZERO, "first")],
            label: String::new(),
        });
        s.amend_top(vec![Edit::insert(Position::new(0, 5), "second")]);
        assert_eq!(s.undo_depth(), 1);
        let top = s.pop_for_undo().unwrap();
        assert_eq!(top.inverse_edits.len(), 2);
        // Prepended: the later edit's inverse comes first.
        assert_eq!(
            top.inverse_edits[0],
            Edit::insert(Position::new(0, 5), "second")
        );
        assert_eq!(top.inverse_edits[1], Edit::insert(Position::ZERO, "first"));
    }

    #[test]
    fn amend_top_on_empty_stack_falls_back_to_push() {
        let mut s = UndoStack::new();
        s.amend_top(vec![Edit::insert(Position::ZERO, "x")]);
        assert_eq!(s.undo_depth(), 1);
    }

    #[test]
    fn amend_top_does_not_clear_redo() {
        // Unlike `push`, amending an open group must not drop redo.
        let mut s = UndoStack::new();
        s.record_redo(entry("r"));
        s.push(entry("open")); // clears redo per the push invariant...
        s.record_redo(entry("r2")); // ...re-seed to prove amend leaves it be
        s.amend_top(vec![Edit::insert(Position::ZERO, "y")]);
        assert_eq!(s.redo_depth(), 1);
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
