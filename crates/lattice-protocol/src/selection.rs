//! Selections.
//!
//! A `Selection` is an `(anchor, head)` pair plus a visual mode hint. The
//! `head` is the active cursor end; the `anchor` is the other end of any
//! visual extent. When `anchor == head` and `visual` is `None`, the selection
//! is a degenerate cursor.
//!
//! `SelectionSet` is a non-empty set with one designated *primary* selection.
//! v1 invariants assume exactly one selection; the set form is preserved so
//! multi-cursor (post-1.0 per §5.2) is a clean extension.

use serde::{Deserialize, Serialize};

use crate::position::Position;

/// One cursor or visual extent in a buffer.
///
/// `anchor` and `head` are *not* ordered: moving backwards in Visual mode puts
/// `head` before `anchor`, and consumers normalise (`min`/`max`) when they need
/// a span. With [`visual`](Self::visual) set, the extent follows vim's
/// inclusive convention — Charwise covers the character *at* `head` too (a
/// renderer converts to a half-open [`Range`](crate::Range) by extending
/// `end` one character), Linewise covers whole lines regardless of the byte
/// columns, and Blockwise covers the rectangle the two corners span.
///
/// # Examples
///
/// ```
/// use lattice_protocol::{Position, Selection, VisualMode};
///
/// let caret = Selection::cursor(Position::new(4, 2));
/// assert!(caret.is_cursor());
///
/// // A backwards charwise selection: head precedes anchor.
/// let backwards = Selection {
///     anchor: Position::new(0, 8),
///     head: Position::new(0, 3),
///     visual: Some(VisualMode::Charwise),
/// };
/// assert!(!backwards.is_cursor());
/// assert!(backwards.head < backwards.anchor);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    /// The fixed end — where Visual mode was entered. Equals `head` for a
    /// plain cursor.
    pub anchor: Position,
    /// The moving end: the cursor the user sees and motions move.
    pub head: Position,
    /// The visual-mode shape of the extent, or `None` outside Visual mode.
    pub visual: Option<VisualMode>,
}

/// How a visual selection's two ends are interpreted — vim's `v`, `V` and
/// `<C-v>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VisualMode {
    /// `v`: every character from one end to the other, both ends included.
    Charwise,
    /// `V`: every line from one end's line to the other's; columns ignored.
    Linewise,
    /// `<C-v>`: the rectangle whose opposite corners are the two ends.
    Blockwise,
}

impl Selection {
    /// A plain cursor at `at`: `anchor == head`, no visual extent.
    pub const fn cursor(at: Position) -> Self {
        Self {
            anchor: at,
            head: at,
            visual: None,
        }
    }

    /// `true` for a plain cursor: collapsed *and* not in Visual mode. A
    /// one-character visual selection (`anchor == head`, `visual` set) is not
    /// a cursor — it selects that character.
    pub fn is_cursor(&self) -> bool {
        self.anchor == self.head && self.visual.is_none()
    }
}

/// A selection set. Always non-empty. Index `primary` points at the primary
/// selection; in v1 the set has exactly one entry and `primary == 0`.
///
/// The fields are private so the invariant cannot be broken: every
/// constructor yields at least one selection and an in-range primary, so
/// [`Self::primary`] never panics.
///
/// # Examples
///
/// ```
/// use lattice_protocol::{Position, Selection, SelectionSet};
///
/// let mut set = SelectionSet::default(); // one cursor at the origin
/// assert_eq!(set.all().len(), 1);
/// assert_eq!(set.primary().head, Position::ZERO);
///
/// set.replace_primary(Selection::cursor(Position::new(3, 0)));
/// assert_eq!(set.primary().head.line, 3);
///
/// // Rebuilding from parts repairs what would break the invariant.
/// let repaired = SelectionSet::from_parts(vec![], 5);
/// assert_eq!(repaired, SelectionSet::cursor_at_origin());
/// let clamped = SelectionSet::from_parts(vec![Selection::cursor(Position::ZERO)], 5);
/// assert_eq!(clamped.primary_index(), 0);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionSet {
    selections: Vec<Selection>,
    primary: usize,
}

impl SelectionSet {
    /// A set holding just `selection`, which is primary.
    pub fn single(selection: Selection) -> Self {
        Self {
            selections: vec![selection],
            primary: 0,
        }
    }

    /// One plain cursor at [`Position::ZERO`] — also the [`Default`].
    pub fn cursor_at_origin() -> Self {
        Self::single(Selection::cursor(Position::ZERO))
    }

    /// Reconstruct a set from its parts — the counterpart to [`Self::all`] +
    /// [`Self::primary_index`], used to rebuild a `SelectionSet` that was
    /// projected into an owned form (e.g. the plugin-host WIT boundary). The
    /// non-empty invariant is preserved: an empty `selections` collapses to a
    /// single origin cursor, and `primary` is clamped into range.
    pub fn from_parts(selections: Vec<Selection>, primary: usize) -> Self {
        if selections.is_empty() {
            return Self::cursor_at_origin();
        }
        let primary = primary.min(selections.len() - 1);
        Self {
            selections,
            primary,
        }
    }

    /// The primary selection — the one single-cursor code acts on.
    pub fn primary(&self) -> &Selection {
        // SAFETY-equivalent: every constructor and mutator preserves the
        // non-empty invariant, so primary is always a valid index.
        &self.selections[self.primary]
    }

    /// Mutable access to the primary selection.
    pub fn primary_mut(&mut self) -> &mut Selection {
        &mut self.selections[self.primary]
    }

    /// Every selection, in stored order (never empty).
    pub fn all(&self) -> &[Selection] {
        &self.selections
    }

    /// Index of the primary within [`Self::all`].
    pub fn primary_index(&self) -> usize {
        self.primary
    }

    /// Overwrite the primary selection in place; the others are untouched.
    pub fn replace_primary(&mut self, selection: Selection) {
        self.selections[self.primary] = selection;
    }
}

impl Default for SelectionSet {
    fn default() -> Self {
        Self::cursor_at_origin()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn cursor_constructor_collapses_anchor_and_head() {
        let p = Position::new(2, 4);
        let sel = Selection::cursor(p);
        assert_eq!(sel.anchor, p);
        assert_eq!(sel.head, p);
        assert_eq!(sel.visual, None);
        assert!(sel.is_cursor());
    }

    #[test]
    fn selection_with_distinct_endpoints_is_not_a_cursor() {
        let sel = Selection {
            anchor: Position::new(0, 0),
            head: Position::new(0, 3),
            visual: None,
        };
        assert!(!sel.is_cursor());
    }

    #[test]
    fn selection_with_visual_extent_is_not_a_cursor_even_when_collapsed() {
        let sel = Selection {
            anchor: Position::ZERO,
            head: Position::ZERO,
            visual: Some(VisualMode::Charwise),
        };
        assert!(!sel.is_cursor());
    }

    #[test]
    fn selection_set_default_is_a_single_origin_cursor() {
        let s = SelectionSet::default();
        assert_eq!(s.all().len(), 1);
        assert_eq!(s.primary_index(), 0);
        assert!(s.primary().is_cursor());
        assert_eq!(s.primary().head, Position::ZERO);
    }

    #[test]
    fn from_parts_preserves_selections_and_primary() {
        let a = Selection::cursor(Position::new(0, 0));
        let b = Selection::cursor(Position::new(2, 3));
        let s = SelectionSet::from_parts(vec![a, b], 1);
        assert_eq!(s.all().len(), 2);
        assert_eq!(s.primary_index(), 1);
        assert_eq!(s.primary(), &b);
    }

    #[test]
    fn from_parts_clamps_out_of_range_primary_and_repairs_empty() {
        // Out-of-range primary clamps to the last selection (invariant kept).
        let only = Selection::cursor(Position::new(1, 1));
        let clamped = SelectionSet::from_parts(vec![only], 9);
        assert_eq!(clamped.primary_index(), 0);
        // Empty input collapses to a single origin cursor (never empty).
        let repaired = SelectionSet::from_parts(vec![], 3);
        assert_eq!(repaired.all().len(), 1);
        assert_eq!(repaired.primary_index(), 0);
        assert!(repaired.primary().is_cursor());
    }

    #[test]
    fn selection_set_single_uses_provided_selection() {
        let sel = Selection::cursor(Position::new(5, 6));
        let s = SelectionSet::single(sel);
        assert_eq!(s.primary(), &sel);
        assert_eq!(s.all().len(), 1);
    }

    #[test]
    fn replace_primary_swaps_in_place_without_changing_count() {
        let mut s = SelectionSet::default();
        let new_sel = Selection::cursor(Position::new(7, 0));
        s.replace_primary(new_sel);
        assert_eq!(s.primary(), &new_sel);
        assert_eq!(s.all().len(), 1);
        assert_eq!(s.primary_index(), 0);
    }

    #[test]
    fn primary_mut_allows_in_place_mutation() {
        let mut s = SelectionSet::default();
        s.primary_mut().head = Position::new(0, 4);
        assert_eq!(s.primary().head, Position::new(0, 4));
    }

    #[test]
    fn visual_modes_are_distinct() {
        // Documents the intent: charwise / linewise / blockwise are not equal.
        assert_ne!(Some(VisualMode::Charwise), Some(VisualMode::Linewise));
        assert_ne!(Some(VisualMode::Linewise), Some(VisualMode::Blockwise));
    }
}
