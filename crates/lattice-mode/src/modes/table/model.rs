//! TB.1 — a pipe table as `table-mode` sees it: where it starts and ends,
//! which cell the caret is in, and how to render it back.
//!
//! [`layout`](super::layout) is the unattended half of this — it reformats
//! every table in a document at content-build time, for `lattice-help`. This
//! is the *interactive* half, and the two differ in three ways that all come
//! from the same fact: **the user pointed at this table**.
//!
//! 1. **A separator row is not required.** `layout`'s recogniser demands one,
//!    and is right to: it walks whole documents unattended, and prose like
//!    ``use `a | b` `` must not become a one-column table. Nothing here runs
//!    unattended — you put the caret on the line and pressed a key — so
//!    demanding a separator would refuse to align exactly the org tables that
//!    do not have one, which is most of them.
//!
//! 2. **The separator style is preserved, not chosen.** Org writes
//!    `|---+---|` and markdown writes `|---|---|`; both are tables, and an
//!    align that rewrote one into the other would edit a file's dialect
//!    because you asked it to line up some columns.
//!
//!    That is why there is no `table.dialect` option and no seam for a major
//!    to declare one: **the table says which dialect it is.** An option would
//!    be a second source for a fact already in the buffer, and the two can
//!    disagree — a `+`-joined table in a markdown file would be rewritten by
//!    a correct-looking option. Reading the file cannot be wrong about the
//!    file.
//!
//! 3. **The caret has to land somewhere.** Alignment rewrites every row, so
//!    the byte offset the caret sat at is meaningless afterwards; the mode
//!    tracks the *cell* and re-derives an offset in the rendered line.
//!
//! Width is measured by `unicode-width`, through [`layout`]'s own helpers —
//! shared rather than re-derived, because a second measurement that disagreed
//! would align tables one way in help pages and another way under the caret.

use super::layout;

/// One line of a table, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// `| a | b |` — the trimmed cell texts.
    Cells(Vec<String>),
    /// A rule between sections, and **how it was written**: the character
    /// that joins its columns (`|` in markdown, `+` in org) and its
    /// per-column alignment markers. Both are kept so re-rendering reproduces
    /// the dialect it found rather than imposing one.
    Separator {
        join: char,
        aligns: Vec<layout::Align>,
    },
}

/// A table lifted out of a buffer: its line span, its rows, and the
/// indentation its first line carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// First line of the table, inclusive.
    pub first: u32,
    /// Last line of the table, inclusive.
    pub last: u32,
    pub rows: Vec<Row>,
    /// Leading whitespace of the first row, reproduced on every rendered
    /// line. An indented table stays where the author put it — org tables
    /// under a headline routinely are.
    pub indent: String,
}

/// True when `line` is part of a table: a `|` after optional indent.
pub fn is_table_line(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

/// True when `line` is a rule row, and which character joins its columns.
///
/// Pipes, dashes, colons, plus signs and spaces — nothing else, and at least
/// one dash. `+` is org's join; `:` are markdown's alignment markers. A row
/// containing both is nonsense nobody writes, and is read as org's.
fn separator_join(line: &str) -> Option<char> {
    let t = line.trim();
    if !t.starts_with('|') || t.len() < 2 || !t.contains('-') {
        return None;
    }
    if !t.chars().all(|c| matches!(c, '|' | '-' | ':' | '+' | ' ')) {
        return None;
    }
    Some(if t.contains('+') { '+' } else { '|' })
}

/// Parse one line into a row.
pub fn parse_row(line: &str) -> Option<Row> {
    if !is_table_line(line) {
        return None;
    }
    if let Some(join) = separator_join(line) {
        return Some(Row::Separator {
            join,
            aligns: layout::alignments(line),
        });
    }
    Some(Row::Cells(layout::cells(line)))
}

impl Table {
    /// The table containing `at`, or `None` when that line is not one.
    ///
    /// `line` answers for any row index; `line_count` bounds the walk. Taking
    /// a closure rather than a `&Buffer` keeps this crate off `lattice-core`'s
    /// rope and makes the whole model testable on a `Vec<&str>`.
    pub fn at(line: impl Fn(u32) -> Option<String>, at: u32, line_count: u32) -> Option<Table> {
        let here = line(at)?;
        if !is_table_line(&here) {
            return None;
        }
        let mut first = at;
        while first > 0 && line(first - 1).is_some_and(|t| is_table_line(&t)) {
            first -= 1;
        }
        let mut last = at;
        while last + 1 < line_count && line(last + 1).is_some_and(|t| is_table_line(&t)) {
            last += 1;
        }
        let head = line(first)?;
        let indent = head[..head.len() - head.trim_start().len()].to_string();
        let rows: Vec<Row> = (first..=last)
            .filter_map(|n| line(n).as_deref().and_then(parse_row))
            .collect();
        Some(Table {
            first,
            last,
            rows,
            indent,
        })
    }

    /// How many columns the widest row has. A ragged table is normal
    /// mid-edit, and the widest row is what every operation sizes against.
    pub fn columns(&self) -> usize {
        self.rows
            .iter()
            .filter_map(|r| match r {
                Row::Cells(c) => Some(c.len()),
                Row::Separator { .. } => None,
            })
            .max()
            .unwrap_or(0)
    }

    /// The row index (into [`Self::rows`]) for buffer line `line`.
    pub fn row_index(&self, line: u32) -> Option<usize> {
        (line >= self.first && line <= self.last).then(|| (line - self.first) as usize)
    }

    /// Render every row, preserving the separator style the table was found
    /// with and the indentation of its first line.
    ///
    /// Column widths come from the widest visible cell, floored at 1: a
    /// column of empty cells still has to be wide enough to put the caret in,
    /// and a zero-width column renders `||` with nowhere to type.
    pub fn render(&self) -> Vec<String> {
        let columns = self.columns();
        if columns == 0 {
            return self
                .rows
                .iter()
                .map(|_| format!("{}|", self.indent))
                .collect();
        }
        let mut widths = vec![1usize; columns];
        for row in &self.rows {
            if let Row::Cells(cells) = row {
                for (c, cell) in cells.iter().enumerate() {
                    widths[c] = widths[c].max(layout::visible_width(cell));
                }
            }
        }
        // Alignment markers come from the separator the table already has.
        // A table with no separator has no markers to honour, and inventing
        // one would add a row the user did not ask for.
        let aligns = self.alignments();
        self.rows
            .iter()
            .map(|row| {
                let body = match row {
                    Row::Separator { join, .. } => layout::render_separator(
                        &widths,
                        |c| aligns.get(c).copied().unwrap_or(layout::Align::Left),
                        *join,
                    ),
                    Row::Cells(cells) => layout::render_row(cells, &widths, |c| {
                        aligns.get(c).copied().unwrap_or(layout::Align::Left)
                    }),
                };
                format!("{}{body}", self.indent)
            })
            .collect()
    }

    /// Per-column alignment, read off the first separator row. All-left when
    /// there is none — a table with no rule has no markers to honour, and
    /// inventing a rule to carry some would add a row nobody asked for.
    fn alignments(&self) -> Vec<layout::Align> {
        self.rows
            .iter()
            .find_map(|r| match r {
                Row::Separator { aligns, .. } => Some(aligns.clone()),
                Row::Cells(_) => None,
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    /// A buffer as a slice of lines, which is all the model needs.
    fn table_at(src: &str, at: u32) -> Option<Table> {
        let lines: Vec<String> = src.lines().map(str::to_string).collect();
        let n = lines.len() as u32;
        Table::at(|i| lines.get(i as usize).cloned(), at, n)
    }

    fn rendered(src: &str, at: u32) -> String {
        table_at(src, at)
            .expect("a table at that line")
            .render()
            .join("\n")
    }

    #[test]
    fn a_line_outside_a_table_is_not_one() {
        assert!(table_at("prose\n| a |\n", 0).is_none());
    }

    /// The interactive recogniser does NOT require a separator row. Org
    /// tables routinely have none, and refusing to align them would refuse
    /// the common case.
    #[test]
    fn a_table_with_no_separator_is_still_a_table() {
        let t = table_at("| a | b |\n| c | d |\n", 1).expect("both lines are one table");
        assert_eq!((t.first, t.last), (0, 1));
        assert_eq!(t.columns(), 2);
    }

    /// …which is exactly where this differs from `layout::format_tables`,
    /// whose stricter rule protects prose it walks unattended.
    #[test]
    fn the_unattended_pass_still_requires_one() {
        let src = vec!["| a | b |".to_string(), "| c | d |".to_string()];
        assert_eq!(
            layout::format_tables(src.clone()),
            src,
            "format_tables must leave a separator-less block alone — it runs \
             over whole documents with nobody pointing at anything"
        );
    }

    /// The bounds are the contiguous run, and stop at the first non-table
    /// line in each direction.
    #[test]
    fn bounds_cover_the_contiguous_run_only() {
        let t = table_at("intro\n| a |\n| b |\nafter\n| c |\n", 2).unwrap();
        assert_eq!((t.first, t.last), (1, 2));
    }

    /// Org's `+`-joined rule survives a render. Rewriting it to `|` would
    /// change the file's dialect because the user asked to line up columns.
    #[test]
    fn an_org_rule_stays_org() {
        let out = rendered("| Name | Qty |\n|--+--|\n| bread | 1 |\n", 0);
        let rule = out.lines().nth(1).unwrap();
        assert!(rule.contains('+'), "org's join is preserved: {out}");
        assert!(
            rule.starts_with('|') && rule.ends_with('|'),
            "…and the edges are still pipes: {rule}"
        );
    }

    /// And markdown's stays markdown — the same assertion in the other
    /// direction, because a dialect-preserving renderer that only ever
    /// preserved one of them would pass the test above.
    #[test]
    fn a_markdown_rule_stays_markdown() {
        let out = rendered("| Name | Qty |\n|---|---|\n| bread | 1 |\n", 0);
        let rule = out.lines().nth(1).unwrap();
        assert!(!rule.contains('+'), "markdown joins with pipes: {out}");
    }

    /// Alignment markers are markdown's, and they have to survive a
    /// realign — losing them silently re-lefts every right-aligned column.
    #[test]
    fn alignment_markers_survive_and_are_honoured() {
        let out = rendered("| a | b |\n|:--|--:|\n| x | y |\n", 0);
        let rule = out.lines().nth(1).unwrap();
        assert!(rule.contains(":-") && rule.contains("-:"), "{out}");
        let body = out.lines().nth(2).unwrap();
        assert!(
            body.trim_end().ends_with("y |"),
            "the right-aligned column is padded on its left: {body:?}"
        );
    }

    /// Columns line up by DISPLAY width. `é` is one column and `世` is two;
    /// counting `char`s calls them both one and the table renders ragged.
    /// This is the measurement the org plugin's own copy admitted it got
    /// wrong, and the reason the engine lives here.
    #[test]
    fn columns_are_measured_by_display_width() {
        let out = rendered("| 世界 | b |\n| xy | c |\n", 0);
        let widths: Vec<usize> = out
            .lines()
            .map(|l| unicode_width::UnicodeWidthStr::width(l))
            .collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "every row occupies the same columns:\n{out}"
        );
    }

    /// A ragged row is the normal mid-edit state — the moment alignment is
    /// most wanted — so it is padded out, never refused or truncated.
    #[test]
    fn a_short_row_is_padded_rather_than_truncating_the_table() {
        let out = rendered("| a | b | c |\n| x |\n", 0);
        assert_eq!(out.lines().count(), 2);
        assert_eq!(
            out.lines().next().unwrap().matches('|').count(),
            out.lines().nth(1).unwrap().matches('|').count(),
            "the short row gains the missing columns:\n{out}"
        );
    }

    /// An indented table stays indented. Org tables under a headline
    /// routinely are, and re-rendering them at column 0 would move the
    /// table as a side effect of aligning it.
    #[test]
    fn indentation_is_preserved() {
        let out = rendered("  | a | b |\n  | c | d |\n", 0);
        assert!(
            out.lines().all(|l| l.starts_with("  |")),
            "every row keeps the first row's indent:\n{out}"
        );
    }

    /// A column of empty cells still has to be wide enough to put the caret
    /// in — `||` renders with nowhere to type.
    #[test]
    fn an_empty_column_is_still_visible() {
        let out = rendered("| a |  | b |\n", 0);
        assert!(!out.contains("||"), "no zero-width column: {out:?}");
    }
}
