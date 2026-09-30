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
    /// The edits that invert the operation, in the order they must be
    /// applied — the reverse of the order the original edits were applied.
    /// Each edit's range is in the coordinates left by the edit before it.
    pub inverse_edits: Vec<Edit>,
    /// Description for status messages / dot-repeat. Empty for unnamed batches.
    pub label: String,
}

/// A linear undo / redo stack of [`UndoEntry`]s.
///
/// This is a passive container: it never touches a buffer. The owner (in
/// practice [`Document`](crate::Document)) applies the popped entry's
/// edits and records the resulting inverse on the other side — see
/// [`Self::pop_for_undo`] / [`Self::record_redo`]. Pushing a new entry
/// discards redo history (no undo tree yet).
///
/// # Examples
///
/// ```
/// use lattice_core::{UndoEntry, UndoStack};
///
/// let entry = |label: &str| UndoEntry { inverse_edits: vec![], label: label.into() };
/// let mut stack = UndoStack::new();
/// stack.push(entry("a"));
/// stack.push(entry("b"));
///
/// // Undo "b": pop it, apply its edits (elided), record the redo side.
/// let undone = stack.pop_for_undo().map(|e| e.label);
/// assert_eq!(undone.as_deref(), Some("b"));
/// stack.record_redo(entry("b"));
/// assert_eq!((stack.undo_depth(), stack.redo_depth()), (1, 1));
///
/// // A fresh edit drops the redo history.
/// stack.push(entry("c"));
/// assert_eq!((stack.undo_depth(), stack.redo_depth()), (2, 0));
/// ```
#[derive(Debug, Default, Clone)]
pub struct UndoStack {
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
}

impl UndoStack {
    /// An empty stack.
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
    /// same stored order [`push`](Self::push) would use (reverse-application order);
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

    /// Pop the most recent undo entry, or `None` if there is none.
    ///
    /// This does **not** touch the redo stack: the caller applies
    /// `entry.inverse_edits` to the buffer and passes the resulting
    /// "inverse-of-the-inverse" back via [`Self::record_redo`].
    pub fn pop_for_undo(&mut self) -> Option<UndoEntry> {
        self.undo.pop()
    }

    /// Reciprocal of `pop_for_undo`. Stores the edit set that would replay the
    /// undone operation onto the redo stack.
    pub fn record_redo(&mut self, redo_entry: UndoEntry) {
        self.redo.push(redo_entry);
    }

    /// Pop the most recent redo entry, or `None` if there is none. Like
    /// [`Self::pop_for_undo`], the caller applies it and records the result
    /// via [`Self::record_undo`].
    pub fn pop_for_redo(&mut self) -> Option<UndoEntry> {
        self.redo.pop()
    }

    /// Reciprocal of [`Self::pop_for_redo`]: push the edit set that undoes a
    /// just-redone operation. Unlike [`Self::push`] it leaves the redo stack
    /// intact, so further redos remain available.
    pub fn record_undo(&mut self, undo_entry: UndoEntry) {
        self.undo.push(undo_entry);
    }

    /// Number of entries available to undo. [`Document`](crate::Document)
    /// compares it against the depth recorded at save time to decide
    /// dirtiness.
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    /// Number of entries available to redo.
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
