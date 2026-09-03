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

    // ── TB.3 ────────────────────────────────────────────────────────────

    /// Insert a horizontal rule below `at.row`.
    ///
    /// **The style is copied from a rule the table already has.** Only a table
    /// with none needs `fallback`, which is the single place the dialect is
    /// not readable off the buffer — see `mode::rule_fallback`.
    pub fn insert_rule(&self, at: Cell, fallback: char) -> (Table, Cell) {
        let join = self.rule_join().unwrap_or(fallback);
        let aligns = self.alignments_or_default();
        let mut next = self.clone();
        let row = (at.row + 1).min(next.rows.len());
        next.rows.insert(row, Row::Separator { join, aligns });
        // The caret does NOT stay on the rule: there is nothing to type in
        // one, and leaving it there means the next `<Tab>` is the user's
        // first hint that they are parked on a line with no cells.
        let landing = if row + 1 < next.rows.len() {
            row + 1
        } else {
            at.row
        };
        (
            next,
            Cell {
                row: landing,
                column: at.column,
            },
        )
    }

    /// The join character of the table's first rule, if it has one.
    fn rule_join(&self) -> Option<char> {
        self.rows.iter().find_map(|r| match r {
            Row::Separator { join, .. } => Some(*join),
            Row::Cells(_) => None,
        })
    }

    fn alignments_or_default(&self) -> Vec<super::layout::Align> {
        self.rows
            .iter()
            .find_map(|r| match r {
                Row::Separator { aligns, .. } => Some(aligns.clone()),
                Row::Cells(_) => None,
            })
            .unwrap_or_else(|| vec![super::layout::Align::Left; self.columns()])
    }

    /// The run of content rows `at.row` belongs to — the rows between the
    /// rules either side of it, as an inclusive `(first, last)`.
    ///
    /// Sorting is per SECTION rather than whole-table, and that is the whole
    /// reason this exists: a rule separates a header from a body (or one
    /// group from the next), and a sort that crossed it would drag the header
    /// row into the middle of the data. Emacs sorts the region between rules
    /// for the same reason.
    fn section(&self, at: usize) -> Option<(usize, usize)> {
        if self.is_rule(at) {
            return None;
        }
        let mut first = at;
        while first > 0 && !self.is_rule(first - 1) {
            first -= 1;
        }
        let mut last = at;
        while last + 1 < self.rows.len() && !self.is_rule(last + 1) {
            last += 1;
        }
        Some((first, last))
    }

    /// Sort the caret's section by the column under the caret.
    ///
    /// **The comparator is chosen from the data**, not asked for: numeric when
    /// every non-empty value in the column parses as a number, case-insensitive
    /// lexicographic otherwise. Emacs prompts for `a`/`n`/`t`; a prompt on a
    /// single chord is a question with an obvious answer in nearly every real
    /// table, and getting it wrong is one undo.
    ///
    /// Empty cells sort LAST in both directions. An empty cell is the absence
    /// of a value rather than a small one, so floating it to the top of a
    /// descending sort would bury the rows you asked to see.
    pub fn sort_section(&self, at: Cell, descending: bool) -> Option<(Table, Cell)> {
        let (first, last) = self.section(at.row)?;
        if last <= first {
            return None;
        }
        let value = |row: usize| -> String {
            self.cells_at(row)
                .get(at.column)
                .cloned()
                .unwrap_or_default()
        };
        let numeric = (first..=last)
            .map(value)
            .filter(|v| !v.trim().is_empty())
            .all(|v| v.trim().parse::<f64>().is_ok());

        let mut order: Vec<usize> = (first..=last).collect();
        order.sort_by(|&a, &b| {
            let (va, vb) = (value(a), value(b));
            let (ea, eb) = (va.trim().is_empty(), vb.trim().is_empty());
            // Empties last, before the direction is applied — so they stay
            // last when it flips.
            match (ea, eb) {
                (true, true) => return std::cmp::Ordering::Equal,
                (true, false) => return std::cmp::Ordering::Greater,
                (false, true) => return std::cmp::Ordering::Less,
                (false, false) => {}
            }
            let ord = if numeric {
                let na = va.trim().parse::<f64>().unwrap_or(0.0);
                let nb = vb.trim().parse::<f64>().unwrap_or(0.0);
                na.partial_cmp(&nb).unwrap_or(std::cmp::Ordering::Equal)
            } else {
                va.to_lowercase().cmp(&vb.to_lowercase())
            };
            if descending { ord.reverse() } else { ord }
        });

        let mut next = self.clone();
        let sorted: Vec<Row> = order.iter().map(|&i| self.rows[i].clone()).collect();
        next.rows.splice(first..=last, sorted);
        // The caret follows the ROW it was on, not the position — you sorted
        // to see where your row went.
        let landed = order
            .iter()
            .position(|&i| i == at.row)
            .map(|p| first + p)
            .unwrap_or(at.row);
        Some((
            next,
            Cell {
                row: landed,
                column: at.column,
            },
        ))
    }

    /// Empty the cell under the caret, leaving the table's shape alone.
    pub fn blank_cell(&self, at: Cell) -> Option<(Table, Cell)> {
        let mut next = self.clone();
        match next.rows.get_mut(at.row)? {
            Row::Cells(cells) => {
                let slot = cells.get_mut(at.column)?;
                if slot.is_empty() {
                    return None;
                }
                slot.clear();
            }
            Row::Separator { .. } => return None,
        }
        Some((next, at))
    }

    /// Copy the caret's field into the row below, and move down with it.
    ///
    /// Emacs' `org-table-copy-down` (`S-<CR>`), including the increment: a
    /// value ending in an integer copies down as that integer plus one, which
    /// is what makes it a *series* filler rather than a duplicator. `Q3` →
    /// `Q4`, `1.2` → `1.3` — the trailing run of digits moves, the rest is
    /// carried verbatim.
    ///
    /// Creates the row below when the caret is on the last one, since
    /// stopping at the bottom would make the chord fail exactly when you are
    /// filling a column downwards.
    pub fn copy_down(&self, at: Cell) -> Option<(Table, Cell)> {
        let source = self.cells_at(at.row).get(at.column)?.clone();
        let mut next = self.clone();
        let mut target = at.row + 1;
        // Skip a rule rather than writing into it, and create a row when
        // there is nothing below.
        while target < next.rows.len() && next.is_rule(target) {
            target += 1;
        }
        if target >= next.rows.len() {
            let columns = next.columns().max(at.column + 1);
            next.rows.push(Row::Cells(vec![String::new(); columns]));
            target = next.rows.len() - 1;
        }
        let columns = next.columns().max(at.column + 1);
        match next.rows.get_mut(target)? {
            Row::Cells(cells) => {
                if cells.len() < columns {
                    cells.resize(columns, String::new());
                }
                cells[at.column] = increment_trailing_number(&source);
            }
            Row::Separator { .. } => return None,
        }
        Some((
            next,
            Cell {
                row: target,
                column: at.column,
            },
        ))
    }

    /// The cell directly below `at`, skipping rules — where Insert-mode
    /// `<CR>` goes.
    ///
    /// Distinct from [`Self::next_cell`], which wraps to the start of the next
    /// row: `<CR>` keeps the COLUMN, because it is how you fill one downwards.
    /// `None` at the bottom; the caller decides whether that means a new row.
    pub fn down_cell(&self, at: Cell) -> Option<Cell> {
        let mut row = at.row + 1;
        while row < self.rows.len() {
            if !self.is_rule(row) {
                return Some(Cell {
                    row,
                    column: at.column,
                });
            }
            row += 1;
        }
        None
    }

    /// Swap the table's rows and columns.
    ///
    /// **Rules do not survive**, and cannot: a rule separates row groups, and
    /// after a transpose those groups are columns — there is no horizontal
    /// line that means what it meant. Emacs' `org-table-transpose-table-at-point`
    /// drops them for the same reason. Dropping them is stated here rather
    /// than discovered in a diff.
    pub fn transpose(&self, at: Cell) -> Option<(Table, Cell)> {
        let content: Vec<&[String]> = self
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Cells(c) => Some(c.as_slice()),
                Row::Separator { .. } => None,
            })
            .collect();
        if content.is_empty() {
            return None;
        }
        let columns = content.iter().map(|r| r.len()).max().unwrap_or(0);
        if columns == 0 {
            return None;
        }
        let rows: Vec<Row> = (0..columns)
            .map(|c| {
                Row::Cells(
                    content
                        .iter()
                        .map(|r| r.get(c).cloned().unwrap_or_default())
                        .collect(),
                )
            })
            .collect();
        // The caret's cell transposes with the table: the value that was at
        // (row, column) is now at (column, row), and landing anywhere else
        // would lose the user's place in a table that just changed shape.
        let content_index = self.rows[..at.row.min(self.rows.len())]
            .iter()
            .filter(|r| matches!(r, Row::Cells(_)))
            .count();
        Some((
            Table {
                first: self.first,
                last: self.last,
                rows,
                indent: self.indent.clone(),
            },
            Cell {
                row: at.column.min(columns.saturating_sub(1)),
                column: content_index,
            },
        ))
    }
}

/// `Q3` → `Q4`, `1.2` → `1.3`, `total` → `total`.
///
/// Only the trailing run of digits moves; everything before it is carried
/// verbatim, which is what makes this work on `2026-09` and `item-9` alike.
/// A value with no trailing digits copies unchanged — the honest answer for
/// a label, and the common case for a header being filled down.
fn increment_trailing_number(value: &str) -> String {
    let digits_start = value
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_digit())
        .map(|(i, _)| i)
        .last();
    let Some(start) = digits_start else {
        return value.to_string();
    };
    let Ok(n) = value[start..].parse::<u64>() else {
        return value.to_string();
    };
    // Preserved width, so `09` becomes `10` and not `1O`-looking `10` beside
    // a column of `07`, `08`.
    let width = value.len() - start;
    format!("{}{:0width$}", &value[..start], n + 1, width = width)
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

    // ── TB.3 ────────────────────────────────────────────────────────────

    /// A new rule copies the style of one the table already has, so
    /// inserting into an org table does not quietly plant a markdown rule
    /// that the next align then preserves forever.
    #[test]
    fn a_new_rule_copies_the_style_the_table_already_uses() {
        let t = table("| a | b |\n|--+--|\n| c | d |\n");
        let (next, _) = t.insert_rule(Cell { row: 2, column: 0 }, '|');
        let out = shape(&next);
        let rules: Vec<&String> = out.iter().filter(|l| l.contains('-')).collect();
        assert_eq!(rules.len(), 2);
        assert!(
            rules.iter().all(|l| l.contains('+')),
            "both rules are org's: {rules:?}"
        );
    }

    /// The fallback is used ONLY when there is no rule to copy — the single
    /// case the buffer cannot answer.
    #[test]
    fn the_fallback_style_is_used_only_when_there_is_no_rule() {
        let t = table("| a | b |\n| c | d |\n");
        let (org, _) = t.insert_rule(Cell { row: 0, column: 0 }, '+');
        assert!(shape(&org)[1].contains('+'), "{:?}", shape(&org));
        let (md, _) = t.insert_rule(Cell { row: 0, column: 0 }, '|');
        assert!(!shape(&md)[1].contains('+'), "{:?}", shape(&md));
    }

    /// The caret does not park on the rule. There is nothing to type in one,
    /// and the next `<Tab>` would be the user's first hint that they were on
    /// a line with no cells.
    #[test]
    fn the_caret_moves_off_the_new_rule() {
        let t = table("| a |\n| b |\n");
        let (next, cell) = t.insert_rule(Cell { row: 0, column: 0 }, '|');
        assert!(!next.is_rule(cell.row), "landed on a rule: {:?}", next.rows);
    }

    // ── sorting ─────────────────────────────────────────────────────────

    /// Numeric when the column is numbers — `10` sorts after `9`, which
    /// lexicographic order gets backwards and which is the single most
    /// noticeable way a sort can be wrong.
    #[test]
    fn a_numeric_column_sorts_numerically() {
        let t = table("| 9 |\n| 10 |\n| 2 |\n");
        let (next, _) = t.sort_section(Cell { row: 0, column: 0 }, false).unwrap();
        let got: Vec<String> = shape(&next)
            .iter()
            .map(|l| l.trim_matches(['|', ' ']).to_string())
            .collect();
        assert_eq!(got, vec!["2", "9", "10"]);
    }

    #[test]
    fn a_text_column_sorts_case_insensitively() {
        let t = table("| beta |\n| Alpha |\n| gamma |\n");
        let (next, _) = t.sort_section(Cell { row: 0, column: 0 }, false).unwrap();
        let got: Vec<String> = shape(&next)
            .iter()
            .map(|l| l.trim_matches(['|', ' ']).to_string())
            .collect();
        assert_eq!(got, vec!["Alpha", "beta", "gamma"]);
    }

    /// A sort must NOT cross a rule. The rule separates the header from the
    /// body, and a sort that dragged the header into the middle of the data
    /// would be wrong in the way that costs a `u` and a double-take.
    #[test]
    fn sorting_stays_inside_its_section() {
        let t = table("| header |\n|---|\n| b |\n| a |\n");
        let (next, _) = t.sort_section(Cell { row: 2, column: 0 }, false).unwrap();
        let out = shape(&next);
        assert!(out[0].contains("header"), "the header stayed put: {out:?}");
        assert!(out[2].contains('a') && out[3].contains('b'), "{out:?}");
    }

    /// Empty cells sort last in BOTH directions: an empty cell is the absence
    /// of a value, not a small one, so floating it to the top of a descending
    /// sort would bury the rows you asked to see.
    #[test]
    fn empty_cells_sort_last_whichever_way_you_sort() {
        let t = table("| b |\n|  |\n| a |\n");
        for descending in [false, true] {
            let (next, _) = t
                .sort_section(Cell { row: 0, column: 0 }, descending)
                .unwrap();
            let out = shape(&next);
            assert!(
                out[2].trim_matches(['|', ' ']).is_empty(),
                "descending={descending}: {out:?}"
            );
        }
    }

    /// The caret follows its ROW. You sorted to see where your row went.
    #[test]
    fn the_caret_follows_the_row_it_was_on() {
        let t = table("| 3 |\n| 1 |\n| 2 |\n");
        let (_, cell) = t.sort_section(Cell { row: 0, column: 0 }, false).unwrap();
        assert_eq!(cell.row, 2, "the `3` row is last now, and so is the caret");
    }

    #[test]
    fn a_one_row_section_has_nothing_to_sort() {
        let t = table("| a |\n");
        assert!(t.sort_section(Cell { row: 0, column: 0 }, false).is_none());
    }

    // ── blank / copy-down / transpose ───────────────────────────────────

    #[test]
    fn blanking_empties_the_cell_and_leaves_the_shape() {
        let t = table("| a | b |\n");
        let (next, _) = t.blank_cell(Cell { row: 0, column: 0 }).unwrap();
        assert_eq!(next.columns(), 2, "still two columns");
        assert!(shape(&next)[0].contains('b'));
        assert!(!shape(&next)[0].contains('a'));
        // An already-empty cell is a no-op rather than an edit that changes
        // nothing — an undo step for no change is a papercut.
        assert!(next.blank_cell(Cell { row: 0, column: 0 }).is_none());
    }

    /// Emacs' `org-table-copy-down`: the trailing integer moves, which is
    /// what makes it a series filler rather than a duplicator.
    #[test]
    fn copy_down_increments_a_trailing_number() {
        let t = table("| Q3 | x |\n| | |\n");
        let (next, cell) = t.copy_down(Cell { row: 0, column: 0 }).unwrap();
        assert_eq!(cell.row, 1, "the caret moved down with it");
        assert!(shape(&next)[1].contains("Q4"), "{:?}", shape(&next));
    }

    #[test]
    fn copy_down_carries_a_plain_label_unchanged() {
        let t = table("| total |\n| |\n");
        let (next, _) = t.copy_down(Cell { row: 0, column: 0 }).unwrap();
        assert!(shape(&next)[1].contains("total"), "{:?}", shape(&next));
    }

    /// Filling a column downwards must not fail at the bottom, which is
    /// exactly where you are when you are filling one.
    #[test]
    fn copy_down_creates_the_row_it_needs() {
        let t = table("| 1 |\n");
        let (next, cell) = t.copy_down(Cell { row: 0, column: 0 }).unwrap();
        assert_eq!(next.rows.len(), 2);
        assert_eq!(cell.row, 1);
        assert!(shape(&next)[1].contains('2'));
    }

    /// A leading zero keeps its width — `09` becomes `10`, not `1`, so a
    /// column of `07`, `08`, `09` does not lose its alignment at the tens.
    #[test]
    fn an_incremented_number_keeps_its_width() {
        assert_eq!(increment_trailing_number("09"), "10");
        assert_eq!(increment_trailing_number("item-9"), "item-10");
        assert_eq!(increment_trailing_number("total"), "total");
        assert_eq!(increment_trailing_number(""), "");
    }

    /// `<CR>` keeps the COLUMN — it is how you fill one downwards — where
    /// `<Tab>` wraps to the start of the next row.
    #[test]
    fn down_cell_keeps_the_column_and_skips_a_rule() {
        let t = ruled();
        assert_eq!(
            t.down_cell(Cell { row: 0, column: 1 }).unwrap(),
            Cell { row: 2, column: 1 }
        );
        assert!(t.down_cell(Cell { row: 2, column: 1 }).is_none());
    }

    #[test]
    fn transpose_swaps_rows_and_columns() {
        let t = table("| a | b | c |\n| 1 | 2 | 3 |\n");
        let (next, _) = t.transpose(Cell { row: 0, column: 0 }).unwrap();
        let out = shape(&next);
        assert_eq!(out.len(), 3, "three rows now: {out:?}");
        assert!(out[0].contains('a') && out[0].contains('1'), "{out:?}");
        assert!(out[2].contains('c') && out[2].contains('3'), "{out:?}");
    }

    /// The caret transposes with its cell — landing anywhere else loses the
    /// user's place in a table that just changed shape.
    #[test]
    fn the_caret_transposes_too() {
        let t = table("| a | b | c |\n| 1 | 2 | 3 |\n");
        let (_, cell) = t.transpose(Cell { row: 1, column: 2 }).unwrap();
        assert_eq!(cell, Cell { row: 2, column: 1 }, "(1,2) becomes (2,1)");
    }

    /// Rules cannot survive a transpose: a rule separates row groups, and
    /// after the swap those groups are columns. Emacs drops them for the same
    /// reason; stating it here is what keeps it from reading as a bug.
    #[test]
    fn transpose_drops_the_rules() {
        let t = table("| a | b |\n|---|---|\n| 1 | 2 |\n");
        let (next, _) = t.transpose(Cell { row: 0, column: 0 }).unwrap();
        assert!(
            !next.rows.iter().any(|r| matches!(r, Row::Separator { .. })),
            "{:?}",
            next.rows
        );
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
