//! The `'<` and `'>` marks: where the last Visual selection began and ended.
//!
//! They are marks in every way a user can tell — `'<` jumps, `d'>` deletes
//! to, `:'<,'>` addresses — but they are not *stored* as marks. The last
//! Visual selection already lives in [`Editor::last_visual`] (it is what `gv`
//! restores), and writing its two ends into the named-mark table as well
//! would be a second copy to keep in step at every place a selection ends.
//! So the table the grammar reads is a view: the named marks, plus these two
//! answered from the selection.

use std::borrow::Borrow;
use std::collections::HashMap;

use lattice_grammar::{MarkResolver, VisualKind};
use lattice_protocol::position::Position;

use crate::editor::Editor;

/// The mark table the grammar resolves `'x` / `` `x `` / `:'x` against.
///
/// Generic over how the named marks are held so the two dispatch paths can
/// each keep their shape: the read-only path borrows the editor's map, the
/// actor path needs an owned one to put behind an `Arc`.
pub(crate) struct HostMarks<M> {
    pub(crate) named: M,
    /// `(start, end)` of the last Visual selection, in buffer order.
    pub(crate) visual: Option<(Position, Position)>,
}

impl<M: Borrow<HashMap<char, Position>>> MarkResolver for HostMarks<M> {
    fn mark(&self, name: char) -> Option<Position> {
        match name {
            '<' => self.visual.map(|(start, _)| start),
            '>' => self.visual.map(|(_, end)| end),
            _ => self.named.borrow().get(&name).copied(),
        }
    }
}

impl Editor {
    /// Where `'<` and `'>` are, or `None` before any Visual selection.
    ///
    /// In buffer order whichever way the selection was drawn. A linewise
    /// selection marks whole lines, as in vim: `'<` at the start of its
    /// first line and `'>` past the end of its last (the reader clamps a
    /// mark to the line, so "past the end" lands on the last column).
    pub(crate) fn visual_marks(&self) -> Option<(Position, Position)> {
        let last = self.last_visual.as_ref()?;
        let (start, end) =
            if (last.anchor.line, last.anchor.byte) <= (last.head.line, last.head.byte) {
                (last.anchor, last.head)
            } else {
                (last.head, last.anchor)
            };
        Some(match last.kind {
            VisualKind::Linewise => (
                Position::new(start.line, 0),
                Position::new(end.line, u32::MAX),
            ),
            VisualKind::Charwise | VisualKind::Blockwise => (start, end),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_visual_marks_are_answered_beside_the_named_ones() {
        let named = HashMap::from([('a', Position::new(1, 1))]);
        let marks = HostMarks {
            named: &named,
            visual: Some((Position::new(2, 3), Position::new(5, 0))),
        };
        assert_eq!(marks.mark('a'), Some(Position::new(1, 1)));
        assert_eq!(marks.mark('<'), Some(Position::new(2, 3)));
        assert_eq!(marks.mark('>'), Some(Position::new(5, 0)));
        assert_eq!(marks.mark('b'), None);
    }

    #[test]
    fn with_no_selection_yet_they_are_unset() {
        let marks = HostMarks {
            named: HashMap::new(),
            visual: None,
        };
        assert_eq!(marks.mark('<'), None);
        assert_eq!(marks.mark('>'), None);
    }
}
