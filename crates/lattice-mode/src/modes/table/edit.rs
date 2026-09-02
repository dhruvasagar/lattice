//! TB.1 — what the chords do to a table: move between cells, and move,
//! insert and delete rows and columns.
//!
//! Every operation here is a pure function from a [`Table`] and a caret to a
//! new table and a new caret. Nothing touches a buffer; the mode turns the
//! result into one `Effect::ApplyEdit` over the table's line span.
//!
//! **One edit, not one per row.** A column insert changes every line, and a
//! half-applied column is a corrupt table — worse than either end state. It
//! also means `u` undoes the operation rather than the last row of it.
//!
//! **The caret is tracked as a cell, not an offset.** Alignment rewrites
//! every row, so the byte the caret sat on does not survive; what the user
//! means by "where I was" is the cell. Re-deriving the offset from the
//! rendered line is the only way to land in the same place after the widths
//! move.

use super::model::{Row, Table};

/// A caret inside a table: which row, and which cell in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// Index into [`Table::rows`].
    pub row: usize,
    pub column: usize,
}

/// Which cell contains byte offset `col` on `line`.
///
/// Counts the unescaped `|` to the left of the caret — the same split
/// `layout::cells` makes, so the answer agrees with the parse. A caret before
/// the leading pipe (in the indent) is in cell 0: it is the cell you would
/// reach by pressing `l`, and answering `None` there would make `<Tab>` dead
/// on a line the user is plainly inside.
pub fn column_at(line: &str, col: usize) -> usize {
    let mut seen = 0usize;
    let mut escaped = false;
    for (i, ch) in line.char_indices() {
        if i >= col {
            break;
        }
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '|' => seen += 1,
            _ => {}
        }
    }
    // The leading pipe is a boundary, not a cell — one `|` to the left still
    // means cell 0.
    seen.saturating_sub(1)
}

/// The byte offset of cell `column`'s first content character in `line`.
///
/// Where the caret goes after an operation re-renders the row. Falls back to
/// the end of the line when the column does not exist, which is the honest
/// answer for a ragged row: the caret lands at the row's end rather than at
/// its start, and the user is where they were looking.
pub fn offset_of_column(line: &str, column: usize) -> usize {
    let mut seen = 0usize;
    let mut escaped = false;
    for (i, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '|' => {
                if seen == column + 1 {
                    return i;
                }
                seen += 1;
                if seen == column + 1 {
                    // Just past the opening pipe of the wanted cell: skip the
                    // single padding space so the caret lands ON the text.
                    let start = i + 1;
                    return if line[start..].starts_with(' ') {
                        start + 1
                    } else {
                        start
                    };
                }
            }
            _ => {}
        }
    }
    line.len()
}

impl Table {
    /// Cells in `row`, or an empty slice for a rule.
    fn cells_at(&self, row: usize) -> &[String] {
        match self.rows.get(row) {
            Some(Row::Cells(c)) => c,
            _ => &[],
        }
    }

    /// True when `row` is a rule rather than content.
    fn is_rule(&self, row: usize) -> bool {
        matches!(self.rows.get(row), Some(Row::Separator { .. }))
    }

    /// The next cell after `at`, wrapping to the start of the next content
    /// row and **skipping rules** — a rule has no cells, so stopping on one
    /// would make `<Tab>` appear to do nothing every few presses.
    ///
    /// `None` at the last cell of the last row: the caller decides whether
    /// that means "add a row" or "leave the table", and this function has no
    /// business inventing either.
    pub fn next_cell(&self, at: Cell) -> Option<Cell> {
        let width = self.cells_at(at.row).len();
        if at.column + 1 < width {
            return Some(Cell {
                row: at.row,
                column: at.column + 1,
            });
        }
        let mut row = at.row + 1;
        while row < self.rows.len() {
            if !self.is_rule(row) && !self.cells_at(row).is_empty() {
                return Some(Cell { row, column: 0 });
            }
            row += 1;
        }
        None
    }

    /// The previous cell, wrapping to the END of the previous content row.
    pub fn prev_cell(&self, at: Cell) -> Option<Cell> {
        if at.column > 0 {
            return Some(Cell {
                row: at.row,
                column: at.column - 1,
            });
        }
        let mut row = at.row;
        while row > 0 {
            row -= 1;
            if !self.is_rule(row) {
                let width = self.cells_at(row).len();
                if width > 0 {
                    return Some(Cell {
                        row,
                        column: width - 1,
                    });
                }
            }
        }
        None
    }

    /// Swap the row at `at.row` with the one `delta` away, skipping rules.
    ///
    /// Rules are skipped rather than swapped through: a rule marks a boundary
    /// (a header, a section), and dragging a content row across one changes
    /// what the table means, where swapping with the row beyond it does what
    /// the user pictured.
    pub fn move_row(&self, at: Cell, delta: isize) -> Option<(Table, Cell)> {
        if self.is_rule(at.row) {
            return None;
        }
        let mut target = at.row as isize;
        loop {
            target += delta;
            if target < 0 || target as usize >= self.rows.len() {
                return None;
            }
            if !self.is_rule(target as usize) {
                break;
            }
        }
        let target = target as usize;
        let mut next = self.clone();
        next.rows.swap(at.row, target);
        Some((
            next,
            Cell {
                row: target,
                column: at.column,
            },
        ))
    }

    /// Swap column `at.column` with the one `delta` away, in every row.
    pub fn move_column(&self, at: Cell, delta: isize) -> Option<(Table, Cell)> {
        let columns = self.columns();
        let target = at.column as isize + delta;
        if target < 0 || target as usize >= columns {
            return None;
        }
        let target = target as usize;
        let mut next = self.clone();
        for row in &mut next.rows {
            match row {
                Row::Cells(cells) => {
                    // Pad first: a ragged row cannot swap a column it does
                    // not have, and refusing the whole operation because ONE
                    // row is short would fail exactly mid-edit.
                    if cells.len() < columns {
                        cells.resize(columns, String::new());
                    }
                    cells.swap(at.column, target);
                }
                Row::Separator { aligns, .. } => {
                    if aligns.len() >= columns.max(1) && target < aligns.len() {
                        aligns.swap(at.column, target);
                    }
                }
            }
        }
        Some((
            next,
            Cell {
                row: at.row,
                column: target,
            },
        ))
    }

    /// Insert an empty row below `at.row`, and put the caret in it.
    ///
    /// Below rather than above, matching `o` — the row you want is almost
    /// always the next one, and `O`'s peer is a follow-up chord rather than a
    /// guess about which one you meant.
    pub fn insert_row(&self, at: Cell) -> (Table, Cell) {
        let columns = self.columns().max(1);
        let mut next = self.clone();
        let row = (at.row + 1).min(next.rows.len());
        next.rows
            .insert(row, Row::Cells(vec![String::new(); columns]));
        (next, Cell { row, column: 0 })
    }

    /// Insert an empty column to the right of `at.column`, in every row.
    pub fn insert_column(&self, at: Cell) -> (Table, Cell) {
        let columns = self.columns();
        let target = (at.column + 1).min(columns);
        let mut next = self.clone();
        for row in &mut next.rows {
            match row {
                Row::Cells(cells) => {
                    if cells.len() < columns {
                        cells.resize(columns, String::new());
                    }
                    cells.insert(target.min(cells.len()), String::new());
                }
                Row::Separator { aligns, .. } => {
                    if target <= aligns.len() {
                        aligns.insert(target, super::layout::Align::Left);
                    }
                }
            }
        }
        (
            next,
            Cell {
                row: at.row,
                column: target,
            },
        )
    }

    /// Delete the row under the caret.
    ///
    /// `None` when it is the table's last content row: deleting it would
    /// leave a rule floating with nothing to rule, and the user asked to
    /// delete a row, not the table. `dd` is right there for that.
    pub fn delete_row(&self, at: Cell) -> Option<(Table, Cell)> {
        let content = self
            .rows
            .iter()
            .filter(|r| matches!(r, Row::Cells(_)))
            .count();
        if content <= 1 && !self.is_rule(at.row) {
            return None;
        }
        if at.row >= self.rows.len() {
            return None;
        }
        let mut next = self.clone();
        next.rows.remove(at.row);
        let row = at.row.min(next.rows.len().saturating_sub(1));
        Some((
            next,
            Cell {
                row,
                column: at.column,
            },
        ))
    }

    /// Delete the column under the caret, in every row.
    ///
    /// `None` on the last column, for [`Self::delete_row`]'s reason: a table
    /// with no columns is not a table, it is a stack of `|`.
    pub fn delete_column(&self, at: Cell) -> Option<(Table, Cell)> {
        let columns = self.columns();
        if columns <= 1 || at.column >= columns {
            return None;
        }
        let mut next = self.clone();
        for row in &mut next.rows {
            match row {
                Row::Cells(cells) => {
                    if at.column < cells.len() {
                        cells.remove(at.column);
                    }
                }
                Row::Separator { aligns, .. } => {
                    if at.column < aligns.len() {
                        aligns.remove(at.column);
                    }
                }
            }
        }
        Some((
            next,
            Cell {
                row: at.row,
                column: at.column.min(columns - 2),
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    fn table(src: &str) -> Table {
        let lines: Vec<String> = src.lines().map(str::to_string).collect();
        let n = lines.len() as u32;
        Table::at(|i| lines.get(i as usize).cloned(), 0, n).expect("a table")
    }

    /// `| a | b |` — three rows, the middle one a rule.
    fn ruled() -> Table {
        table("| a | b |\n|---|---|\n| c | d |\n")
    }

    fn shape(t: &Table) -> Vec<String> {
        t.render()
    }

    // ── where the caret is ──────────────────────────────────────────────

    #[test]
    fn the_caret_column_counts_pipes_to_its_left() {
        let line = "| a | b | c |";
        assert_eq!(column_at(line, 2), 0);
        assert_eq!(column_at(line, 6), 1);
        assert_eq!(column_at(line, 10), 2);
    }

    /// An escaped pipe is content, not a boundary. Counting it would put the
    /// caret one cell to the right of where it looks.
    #[test]
    fn an_escaped_pipe_is_not_a_cell_boundary() {
        let line = r"| a \| b | c |";
        assert_eq!(column_at(line, 8), 0, "still inside the first cell");
    }

    /// A caret in the indent is in cell 0 — answering "nowhere" would make
    /// `<Tab>` dead on a line the user is plainly inside.
    #[test]
    fn a_caret_before_the_first_pipe_is_in_cell_zero() {
        assert_eq!(column_at("  | a | b |", 0), 0);
    }

    /// The round trip that matters: after a re-render the mode asks for the
    /// offset of the cell it was in, and must land back on the same text.
    #[test]
    fn the_offset_of_a_column_lands_on_its_text() {
        let line = "| aa | bb | cc |";
        for c in 0..3 {
            let at = offset_of_column(line, c);
            assert_eq!(
                &line[at..at + 2],
                ["aa", "bb", "cc"][c],
                "column {c} in {line:?}"
            );
        }
    }

    // ── moving between cells ────────────────────────────────────────────

    #[test]
    fn tab_walks_the_row_then_wraps_to_the_next() {
        let t = ruled();
        let at = Cell { row: 0, column: 0 };
        let next = t.next_cell(at).unwrap();
        assert_eq!(next, Cell { row: 0, column: 1 });
        // …and from the last cell of row 0 it must land on row 2, NOT the
        // rule at row 1, which has no cells to sit in.
        assert_eq!(t.next_cell(next).unwrap(), Cell { row: 2, column: 0 });
    }

    #[test]
    fn shift_tab_walks_backwards_and_wraps_to_the_end_of_the_row_above() {
        let t = ruled();
        let at = Cell { row: 2, column: 0 };
        assert_eq!(t.prev_cell(at).unwrap(), Cell { row: 0, column: 1 });
    }

    /// The last cell of the last row has no next. Returning `Some` of
    /// something invented — a new row, the first cell again — is a decision
    /// for the caller, and burying it here would make `<Tab>` mean two
    /// different things depending on data.
    #[test]
    fn the_last_cell_has_no_next() {
        let t = ruled();
        assert!(t.next_cell(Cell { row: 2, column: 1 }).is_none());
        assert!(t.prev_cell(Cell { row: 0, column: 0 }).is_none());
    }

    // ── rows ────────────────────────────────────────────────────────────

    #[test]
    fn a_row_moves_and_the_caret_follows_it() {
        let t = table("| a |\n| b |\n");
        let (moved, cell) = t.move_row(Cell { row: 0, column: 0 }, 1).unwrap();
        assert_eq!(cell.row, 1, "the caret rides the row it moved");
        assert!(shape(&moved)[0].contains('b'));
        assert!(shape(&moved)[1].contains('a'));
    }

    /// A rule marks a boundary. Dragging a content row across one changes
    /// what the table means; swapping with the row BEYOND it is what the
    /// user pictured.
    #[test]
    fn moving_a_row_skips_over_a_rule() {
        let t = ruled();
        let (moved, cell) = t.move_row(Cell { row: 0, column: 0 }, 1).unwrap();
        assert_eq!(cell.row, 2);
        assert!(
            matches!(moved.rows[1], Row::Separator { .. }),
            "the rule stayed put: {:?}",
            moved.rows
        );
    }

    #[test]
    fn a_row_at_the_edge_does_not_move() {
        let t = table("| a |\n| b |\n");
        assert!(t.move_row(Cell { row: 0, column: 0 }, -1).is_none());
        assert!(t.move_row(Cell { row: 1, column: 0 }, 1).is_none());
    }

    #[test]
    fn inserting_a_row_puts_it_below_and_takes_the_caret_there() {
        let t = ruled();
        let (next, cell) = t.insert_row(Cell { row: 2, column: 1 });
        assert_eq!(next.rows.len(), 4);
        assert_eq!(cell, Cell { row: 3, column: 0 });
        assert_eq!(next.columns(), 2, "the new row is full width");
    }

    #[test]
    fn deleting_a_row_removes_it() {
        let t = ruled();
        let (next, _) = t.delete_row(Cell { row: 2, column: 0 }).unwrap();
        assert_eq!(next.rows.len(), 2);
    }

    /// The last content row is not deletable: what would be left is a rule
    /// ruling nothing. `dd` is right there for deleting the table.
    #[test]
    fn the_last_content_row_is_kept() {
        let t = table("| a |\n");
        assert!(t.delete_row(Cell { row: 0, column: 0 }).is_none());
    }

    // ── columns ─────────────────────────────────────────────────────────

    #[test]
    fn a_column_moves_in_every_row_at_once() {
        let t = ruled();
        let (moved, cell) = t.move_column(Cell { row: 0, column: 0 }, 1).unwrap();
        assert_eq!(cell.column, 1);
        let out = shape(&moved);
        assert!(out[0].starts_with("| b"), "{out:?}");
        assert!(out[2].starts_with("| d"), "…and the row below too: {out:?}");
    }

    /// A ragged row is padded rather than aborting the whole operation —
    /// mid-edit is exactly when a row is short and exactly when the key is
    /// wanted.
    #[test]
    fn moving_a_column_pads_a_short_row_instead_of_refusing() {
        let t = table("| a | b |\n| c |\n");
        let (moved, _) = t.move_column(Cell { row: 0, column: 0 }, 1).unwrap();
        let out = shape(&moved);
        assert_eq!(out.len(), 2);
        assert!(
            out[1].contains('c'),
            "the short row kept its content: {out:?}"
        );
    }

    #[test]
    fn a_column_at_the_edge_does_not_move() {
        let t = ruled();
        assert!(t.move_column(Cell { row: 0, column: 0 }, -1).is_none());
        assert!(t.move_column(Cell { row: 0, column: 1 }, 1).is_none());
    }

    #[test]
    fn inserting_a_column_widens_every_row_including_the_rule() {
        let t = ruled();
        let (next, cell) = t.insert_column(Cell { row: 0, column: 0 });
        assert_eq!(cell.column, 1);
        assert_eq!(next.columns(), 3);
        let rule = &shape(&next)[1];
        assert_eq!(
            rule.matches('|').count(),
            4,
            "the rule grew a column too: {rule}"
        );
    }

    #[test]
    fn deleting_a_column_narrows_every_row() {
        let t = ruled();
        let (next, _) = t.delete_column(Cell { row: 0, column: 0 }).unwrap();
        assert_eq!(next.columns(), 1);
        assert!(shape(&next)[0].contains('b'));
    }

    /// A table with no columns is not a table, it is a stack of pipes.
    #[test]
    fn the_last_column_is_kept() {
        let t = table("| a |\n|---|\n");
        assert!(t.delete_column(Cell { row: 0, column: 0 }).is_none());
    }

    /// Every structural operation goes through the same dialect-preserving
    /// renderer, so org's rule survives a column insert exactly as it
    /// survives an align.
    #[test]
    fn structure_operations_preserve_the_dialect() {
        let t = table("| a | b |\n|--+--|\n| c | d |\n");
        let (next, _) = t.insert_column(Cell { row: 0, column: 0 });
        assert!(
            shape(&next)[1].contains('+'),
            "org's join survives a column insert: {:?}",
            shape(&next)
        );
    }
}
