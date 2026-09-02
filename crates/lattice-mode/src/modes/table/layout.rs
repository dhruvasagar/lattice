//! HP.1: lay out markdown pipe tables so their columns line up.
//!
//! Help pages are markdown, and nothing between the `.md` file and the
//! help buffer used to touch tables — so a reader saw the raw source:
//! `|---|---|` rows rendered literally, and cells whose widths had
//! nothing to do with each other.
//!
//! **Why display width and not `char` count.** The docs that *were*
//! hand-padded were padded by counting characters, which is a different
//! number from the columns a terminal advances. `✓`, `─`, `↑`, `▸` and
//! every CJK glyph break that assumption, so a table looked aligned in
//! the source file and ragged on screen — the specific complaint this
//! module answers. [`unicode_width`] measures what the terminal will
//! actually do.
//!
//! **Why here and not in a renderer.** Two reasons, and the second is
//! the load-bearing one:
//!
//! 1. A renderer-side pass is two implementations (TUI and GPUI) of one
//!    piece of text layout.
//! 2. The buffer's text would then differ from what is on screen, so
//!    `/` search, `w` motions, visual selection and yank would all
//!    operate on columns the reader cannot see. Formatting the text
//!    keeps "what you see" and "what the buffer holds" the same string.
//!
//! **Ordering constraint.** This runs BEFORE
//! [`crate::extract_links_and_clean`], never after. Link ranges are
//! recorded against the cleaned text, so inserting padding afterwards
//! would slide every link on a padded row and `<CR>` would follow the
//! wrong one. Running first means extraction sees the final bytes and
//! the ranges come out right with no offset bookkeeping — which is why
//! [`visible_width`] strips link markup for *measurement only*: the
//! column has to be as wide as the `label` the reader sees, not as wide
//! as `[label](help:some-page)`.

use unicode_width::UnicodeWidthStr;

/// Column alignment, from the separator row's `:` markers.
///
/// Public from TB.1: [`super::model`] renders the same rows under the caret
/// that this module renders at content-build time, and it carries them in a
/// `pub` row type — a second copy of the alignment vocabulary is the
/// duplication the whole `table/` directory exists to avoid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// No marker — the default, and what an unmarked `---` column means.
    Left,
    /// `:---` — left, said out loud.
    ///
    /// Distinct from [`Align::Left`] only so the marker ROUND-TRIPS. Both
    /// render the same text; collapsing them would mean an interactive
    /// realign silently deletes a `:` the author typed, which shows up in a
    /// git diff of their notes as a change they did not make. TB.1 found
    /// this; the unattended help pass gets the same fidelity for free.
    LeftMarked,
    Right,
    Center,
}

/// Reformat every pipe table in `lines`, leaving everything else byte-
/// identical.
///
/// Fenced code blocks are skipped wholesale. Help pages draw menu
/// mock-ups inside ``` fences, and a fence containing `|` is ASCII art
/// whose alignment the author already chose — re-laying it out would
/// corrupt a picture to satisfy a rule about tables.
pub fn format_tables(lines: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    let mut in_fence = false;
    while i < lines.len() {
        if is_fence_delimiter(&lines[i]) {
            in_fence = !in_fence;
            out.push(lines[i].clone());
            i += 1;
            continue;
        }
        if !in_fence && let Some(end) = table_extent(&lines, i) {
            out.extend(layout(&lines[i..end]));
            i = end;
            continue;
        }
        out.push(lines[i].clone());
        i += 1;
    }
    out
}

fn is_fence_delimiter(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("```") || t.starts_with("~~~")
}

/// Is `lines[start..]` a table, and where does it end?
///
/// A table is a header row, a separator row, and zero or more body
/// rows. The separator is what makes it a table rather than a
/// paragraph that happens to contain `|` — requiring it is what keeps
/// prose like "use `a | b`" from being mangled into a one-column
/// table.
fn table_extent(lines: &[String], start: usize) -> Option<usize> {
    if !is_row(lines.get(start)?) || !is_separator(lines.get(start + 1)?) {
        return None;
    }
    let mut end = start + 2;
    while end < lines.len() && is_row(&lines[end]) && !is_fence_delimiter(&lines[end]) {
        end += 1;
    }
    Some(end)
}

fn is_row(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

/// A separator row is pipes, dashes, colons and spaces — nothing else.
fn is_separator(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|')
        && t.len() > 1
        && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
        && t.contains('-')
}

/// Split a row into its cells.
///
/// `\|` is an escaped pipe and stays inside the cell it belongs to —
/// several help tables document alternatives that way (`` `:reg\|:registers` ``),
/// and splitting on it would invent a column.
pub(super) fn cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let inner = t
        .strip_prefix('|')
        .unwrap_or(t)
        .strip_suffix('|')
        .unwrap_or_else(|| t.strip_prefix('|').unwrap_or(t));
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut escaped = false;
    for ch in inner.chars() {
        if escaped {
            cur.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' => {
                cur.push(ch);
                escaped = true;
            }
            '|' => out.push(std::mem::take(&mut cur)),
            _ => cur.push(ch),
        }
    }
    out.push(cur);
    out.into_iter().map(|c| c.trim().to_string()).collect()
}

/// Columns the reader will see this cell occupy.
///
/// `[label](url)` measures as `label`, because that is all the link
/// stripper leaves behind. Getting this wrong in the other direction —
/// measuring the markup — would pad every column holding a cross-link
/// out by the length of a URL nobody sees.
pub(super) fn visible_width(cell: &str) -> usize {
    UnicodeWidthStr::width(strip_link_markup(cell).as_str())
}

fn strip_link_markup(cell: &str) -> String {
    let bytes = cell.as_bytes();
    let mut out = String::with_capacity(cell.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'['
            && let Some(close) = bytes[i + 1..].iter().position(|&b| b == b']')
        {
            let label_end = i + 1 + close;
            if bytes.get(label_end + 1) == Some(&b'(')
                && let Some(paren) = bytes[label_end + 2..].iter().position(|&b| b == b')')
            {
                out.push_str(&cell[i + 1..label_end]);
                i = label_end + 2 + paren + 1;
                continue;
            }
        }
        let ch_len = cell[i..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&cell[i..i + ch_len]);
        i += ch_len;
    }
    out
}

pub(super) fn alignments(separator: &str) -> Vec<Align> {
    cells(separator)
        .into_iter()
        .map(|c| {
            let c = c.trim();
            match (c.starts_with(':'), c.ends_with(':')) {
                (true, true) => Align::Center,
                (false, true) => Align::Right,
                (true, false) => Align::LeftMarked,
                (false, false) => Align::Left,
            }
        })
        .collect()
}

fn layout(table: &[String]) -> Vec<String> {
    let aligns = alignments(&table[1]);
    let rows: Vec<Vec<String>> = table
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 1)
        .map(|(_, l)| cells(l))
        .collect();

    // Ragged rows are common in hand-written markdown. Take the widest
    // row's column count so a row with a missing trailing cell is padded
    // out rather than truncating the table.
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    if columns == 0 {
        return table.to_vec();
    }
    let mut widths = vec![0usize; columns];
    for row in &rows {
        for (c, cell) in row.iter().enumerate() {
            widths[c] = widths[c].max(visible_width(cell));
        }
    }
    // A column of empty cells still needs to be visible as a column.
    for w in &mut widths {
        *w = (*w).max(1);
    }

    let align_of = |c: usize| aligns.get(c).copied().unwrap_or(Align::Left);
    let mut out = Vec::with_capacity(table.len());
    let mut rows = rows.into_iter();

    if let Some(header) = rows.next() {
        out.push(render_row(&header, &widths, align_of));
    }
    out.push(render_separator(&widths, align_of, '|'));
    for row in rows {
        out.push(render_row(&row, &widths, align_of));
    }
    out
}

pub(super) fn render_row(
    row: &[String],
    widths: &[usize],
    align_of: impl Fn(usize) -> Align,
) -> String {
    let mut s = String::from("|");
    for (c, width) in widths.iter().enumerate() {
        let cell = row.get(c).map(String::as_str).unwrap_or("");
        let pad = width.saturating_sub(visible_width(cell));
        s.push(' ');
        match align_of(c) {
            Align::Left | Align::LeftMarked => {
                s.push_str(cell);
                s.extend(std::iter::repeat_n(' ', pad));
            }
            Align::Right => {
                s.extend(std::iter::repeat_n(' ', pad));
                s.push_str(cell);
            }
            Align::Center => {
                let left = pad / 2;
                s.extend(std::iter::repeat_n(' ', left));
                s.push_str(cell);
                s.extend(std::iter::repeat_n(' ', pad - left));
            }
        }
        s.push_str(" |");
    }
    s
}

/// A rule row, its columns joined by `join`.
///
/// `join` is `|` for markdown and `+` for org (TB.1). The parameter exists
/// so [`super::model`] can reproduce the dialect it found instead of carrying
/// a second copy of this function that differs by one character — which is
/// how the two halves of table support drift apart.
pub(super) fn render_separator(
    widths: &[usize],
    align_of: impl Fn(usize) -> Align,
    join: char,
) -> String {
    let mut s = String::from("|");
    for (c, width) in widths.iter().enumerate() {
        // `width + 2` covers the space either side of the cell, so the
        // rule spans the whole column rather than stopping short of it.
        let span = width + 2;
        match align_of(c) {
            Align::Left => s.extend(std::iter::repeat_n('-', span)),
            Align::LeftMarked => {
                s.push(':');
                s.extend(std::iter::repeat_n('-', span - 1));
            }
            Align::Right => {
                s.extend(std::iter::repeat_n('-', span - 1));
                s.push(':');
            }
            Align::Center => {
                s.push(':');
                s.extend(std::iter::repeat_n('-', span - 2));
                s.push(':');
            }
        }
        s.push(join);
    }
    // The loop wrote a trailing join; a table's right edge is always a pipe,
    // even in org where the interior joins are `+`.
    s.pop();
    s.push('|');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(src: &str) -> String {
        format_tables(src.lines().map(str::to_string).collect()).join("\n")
    }

    /// The base case: ragged source in, aligned columns out.
    #[test]
    fn columns_line_up() {
        let out = fmt("| Chord | Action |\n|---|---|\n| `gr` | Refresh |\n| `]]` | Next section |");
        let widths: Vec<usize> = out.lines().map(str::len).collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "every row is the same width:\n{out}"
        );
        assert!(out.contains("| `gr`  | Refresh      |"), "{out}");
    }

    /// **The bug this module exists for.**
    ///
    /// `✓` is one `char` and one column; `─` is one `char` and one
    /// column; but a CJK glyph is one `char` and TWO columns. Padding by
    /// `char` count leaves the wide row one column short, which is
    /// exactly how a hand-aligned table drifts. Asserted by display
    /// width rather than `len()` because the rows genuinely differ in
    /// bytes.
    #[test]
    fn a_wide_glyph_costs_two_columns_not_one() {
        let out = fmt("| Key | Note |\n|---|---|\n| `a` | plain |\n| 日本 | wide |");
        let cols: Vec<usize> = out.lines().map(UnicodeWidthStr::width).collect();
        assert!(
            cols.windows(2).all(|w| w[0] == w[1]),
            "rows must be the same DISPLAY width, got {cols:?}:\n{out}"
        );
        // And the naive measure would have disagreed — proving the test
        // is not passing by accident on a table where both happen to
        // match.
        let chars: Vec<usize> = out.lines().map(|l| l.chars().count()).collect();
        assert!(
            chars.windows(2).any(|w| w[0] != w[1]),
            "this fixture must be one where char-count and display-width \
             DISAGREE, else it cannot detect the bug: {chars:?}"
        );
    }

    /// A column is padded to the width of the link's LABEL, not its
    /// markup — the markup is gone by the time anyone sees the buffer.
    #[test]
    fn a_link_measures_as_its_label() {
        let out = fmt("| A | B |\n|---|---|\n| [gr](help:magit-core-mode) | x |\n| ab | y |");
        let body: Vec<&str> = out.lines().collect();
        assert!(
            body[2].starts_with("| [gr](help:magit-core-mode) |"),
            "the link's markup is untouched: {:?}",
            body[2]
        );
        assert!(
            body[3].starts_with("| ab |"),
            "`ab` is 2 columns like `gr`, so it needs NO padding — if the \
             URL had been measured this row would be padded out to match \
             it: {:?}",
            body[3]
        );
    }

    /// Alignment markers survive the round trip AND move the text.
    ///
    /// Every column is deliberately wider than its narrow cell, because
    /// with equal widths there is no padding to place and left, right
    /// and centre all render identically — a fixture that cannot tell
    /// the three apart proves nothing about any of them.
    #[test]
    fn colons_still_mean_right_and_centre() {
        let out = fmt("| Left | Centre | Right |\n|:--|:-:|--:|\n| a | b | c |");
        let sep = out.lines().nth(1).unwrap();
        // TB.1 changed this line's expectation, deliberately. It used to
        // assert `|---` — the explicit left marker was DROPPED, since left is
        // the default and the rendering is identical either way. That was
        // fine for a pass over generated help pages and wrong the moment the
        // same engine realigns the user's own file: a `:` they typed vanishing
        // from a git diff is a change they did not make. The marker now
        // round-trips; the alignment it means is unchanged.
        assert!(
            sep.starts_with("|:---"),
            "an explicit left marker survives: {sep}"
        );
        assert!(sep.contains(":------:"), "centre keeps both colons: {sep}");
        assert!(sep.ends_with(":|"), "right keeps its trailing colon: {sep}");

        let row = out.lines().nth(2).unwrap();
        assert!(
            row.starts_with("| a    |"),
            "left-aligned hugs the left: {row}"
        );
        assert!(
            row.contains("|   b    |"),
            "centred is padded both sides: {row}"
        );
        assert!(
            row.ends_with("|     c |"),
            "right-aligned hugs the right: {row}"
        );
    }

    /// ASCII art inside a fence is a picture, not a table.
    #[test]
    fn a_fenced_block_is_left_alone() {
        let src = "```text\n| not | a |\n|--|--|\n| table | here |\n```";
        assert_eq!(fmt(src), src, "fenced content must be byte-identical");
    }

    /// Prose containing a pipe is not a one-column table. Without the
    /// separator-row requirement, every such line would be reformatted.
    #[test]
    fn prose_with_a_pipe_is_not_a_table() {
        let src = "Use `a | b` to alternate.\nAnd another line.";
        assert_eq!(fmt(src), src);
    }

    /// An escaped pipe belongs to its cell.
    #[test]
    fn an_escaped_pipe_does_not_split_a_cell() {
        let out = fmt("| Cmd | Does |\n|---|---|\n| `:reg\\|:registers` | List |");
        let row = out.lines().nth(2).unwrap();
        assert_eq!(
            row.matches('|').count() - row.matches("\\|").count(),
            3,
            "three unescaped pipes = two columns: {row}"
        );
    }

    /// A row missing its last cell is padded out, not truncated — the
    /// table keeps its shape and the reader still sees the column.
    #[test]
    fn a_short_row_keeps_the_table_rectangular() {
        let out = fmt("| A | B |\n|---|---|\n| only |");
        let cols: Vec<usize> = out.lines().map(UnicodeWidthStr::width).collect();
        assert!(
            cols.windows(2).all(|w| w[0] == w[1]),
            "short row is padded to full width, got {cols:?}:\n{out}"
        );
    }

    /// Text around a table is untouched, and two tables in one document
    /// are laid out independently rather than sharing column widths.
    #[test]
    fn tables_are_independent_and_prose_survives() {
        let out = fmt("Intro.\n\n| A | B |\n|---|---|\n| x | y |\n\nMiddle.\n\n\
             | Long header | Q |\n|---|---|\n| a | b |\n\nEnd.");
        assert!(out.starts_with("Intro.\n"), "{out}");
        assert!(out.ends_with("\nEnd."), "{out}");
        assert!(
            out.contains("| x | y |"),
            "narrow table stays narrow:\n{out}"
        );
        assert!(
            out.contains("| Long header | Q |"),
            "wide table keeps its own width:\n{out}"
        );
    }
}
