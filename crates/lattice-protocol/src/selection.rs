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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub anchor: Position,
    pub head: Position,
    pub visual: Option<VisualMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VisualMode {
    Charwise,
    Linewise,
    Blockwise,
}

impl Selection {
    pub const fn cursor(at: Position) -> Self {
        Self {
            anchor: at,
            head: at,
            visual: None,
        }
    }

    pub fn is_cursor(&self) -> bool {
        self.anchor == self.head && self.visual.is_none()
    }
}

/// A selection set. Always non-empty. Index `primary` points at the primary
/// selection; in v1 the set has exactly one entry and `primary == 0`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionSet {
    selections: Vec<Selection>,
    primary: usize,
}

impl SelectionSet {
    pub fn single(selection: Selection) -> Self {
        Self {
            selections: vec![selection],
            primary: 0,
        }
    }

    pub fn cursor_at_origin() -> Self {
        Self::single(Selection::cursor(Position::ZERO))
    }

    pub fn primary(&self) -> &Selection {
        // SAFETY-equivalent: every constructor and mutator preserves the
        // non-empty invariant, so primary is always a valid index.
        &self.selections[self.primary]
    }

    pub fn primary_mut(&mut self) -> &mut Selection {
        &mut self.selections[self.primary]
    }

    pub fn all(&self) -> &[Selection] {
        &self.selections
    }

    pub fn primary_index(&self) -> usize {
        self.primary
    }

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
        assert_ne!(
            Some(VisualMode::Charwise),
            Some(VisualMode::Linewise)
        );
        assert_ne!(
            Some(VisualMode::Linewise),
            Some(VisualMode::Blockwise)
        );
    }
}
