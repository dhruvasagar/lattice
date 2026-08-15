//! Indentation units and methods.
//!
//! [`IndentUnit`] is the resolved answer to "what is one level of
//! indent, in this buffer, right now" — `shiftwidth` + `expandtab` +
//! `tabstop` collapsed into one `Copy` value. It lives here rather than
//! in `lattice-indent` (the tree-sitter engine, IN.2) because the `>` /
//! `<` operators in `lattice-grammar` consume it, and
//! `lattice-syntax` → `lattice-grammar` means an engine-side home would
//! be a dependency cycle. This crate is the shared floor both sides
//! already stand on, and it is where the sibling [`crate::FoldMethod`]
//! lives for the same reason.
//!
//! Nothing here reads config. The host resolves the options (including
//! any buffer-local `:setlocal` override) and hands the resulting value
//! down; the grammar layer stays config-agnostic.
//!
//! See `docs/dev/architecture/auto-indent.md` §3.

crate::labeled_enum! {
    /// `:set indentmethod=...`. Which source decides where a newly
    /// created line starts.
    ///
    /// A cascade with a named floor, in the shape of
    /// [`crate::FoldMethod`] — and for the same reason, which is a
    /// property rather than a resemblance: both name a *structural*
    /// source that can be unavailable at the moment it is asked (no
    /// query for this language, a parse that has not landed, a grammar
    /// that failed to load). Degrading to documented vim behaviour
    /// beats a silent wrong answer, and it gives a user whose language
    /// has a bad query a one-word escape hatch.
    ///
    /// `Keep` is not merely `Syntax`'s fallback — it is a setting some
    /// users prefer outright, so the fallback path is exercised by
    /// choice rather than only by failure.
    pub enum IndentMethod {
        /// New lines start at column 0.
        None = "none"
            => "New lines start at column 0",
        /// Copy the previous non-blank line's indent (vim's
        /// `autoindent`).
        Keep = "keep"
            => "Copy the previous line's indent (vim autoindent)",
        /// Tree-sitter `indents.scm`, falling back to `Keep` when the
        /// language has no query or the parse is unavailable.
        #[default]
        Syntax = "syntax"
            => "Indent from the tree-sitter syntax tree",
    }
}

/// One level of indentation, resolved for a specific buffer.
///
/// `width` is `shiftwidth` — **columns per indent level**. `tabstop` is
/// the display width of a literal tab byte. They are deliberately
/// separate: conflating them means "change my indent size" silently
/// reflows every file containing a hard tab, including content the user
/// never edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndentUnit {
    /// `shiftwidth` — columns added or removed per indent level.
    /// Clamped to at least 1 at use; a zero-width level would make
    /// `>` a no-op and `columns → levels` a division by zero.
    pub width: u8,
    /// `expandtab` — render indentation as spaces rather than tabs.
    pub expand_tabs: bool,
    /// `tabstop` — display columns a literal tab advances to.
    /// Clamped to at least 1 at use.
    pub tabstop: u8,
}

impl Default for IndentUnit {
    /// Matches the registered option defaults (`shiftwidth=4`,
    /// `expandtab`, `tabstop=4`) so a context that never resolved
    /// config behaves like an unconfigured buffer rather than like
    /// something arbitrary.
    fn default() -> Self {
        Self {
            width: 4,
            expand_tabs: true,
            tabstop: 4,
        }
    }
}

impl IndentUnit {
    pub fn new(width: u8, expand_tabs: bool, tabstop: u8) -> Self {
        Self {
            width,
            expand_tabs,
            tabstop,
        }
    }

    /// `shiftwidth`, floored at 1. See [`Self::width`].
    #[inline]
    pub fn step(&self) -> u16 {
        self.width.max(1) as u16
    }

    /// `tabstop`, floored at 1. See [`Self::tabstop`].
    #[inline]
    pub fn tab_width(&self) -> u16 {
        self.tabstop.max(1) as u16
    }

    /// Byte length of `line`'s leading whitespace run.
    ///
    /// Whitespace here is space and tab only — not `\r`, and not
    /// Unicode space separators. A line that is entirely whitespace
    /// returns its full length.
    pub fn indent_len(line: &str) -> usize {
        line.bytes()
            .take_while(|b| *b == b' ' || *b == b'\t')
            .count()
    }

    /// Display columns occupied by `line`'s leading whitespace.
    ///
    /// A tab advances to the next multiple of `tabstop`, which is why
    /// this cannot be a byte count: `"\t"` and `"    "` are the same
    /// indent at `tabstop=4` and must compare equal.
    pub fn columns_of(&self, line: &str) -> u16 {
        let tab = self.tab_width();
        let mut col: u16 = 0;
        for b in line.bytes() {
            match b {
                b' ' => col = col.saturating_add(1),
                b'\t' => col = (col / tab).saturating_add(1).saturating_mul(tab),
                _ => break,
            }
        }
        col
    }

    /// The whitespace string that renders as `columns` display columns.
    ///
    /// With `expandtab` off this is tabs plus a space remainder, which
    /// is what vim produces and what keeps `shiftwidth` values that are
    /// not multiples of `tabstop` representable.
    pub fn render(&self, columns: u16) -> String {
        if columns == 0 {
            return String::new();
        }
        if self.expand_tabs {
            return " ".repeat(columns as usize);
        }
        let tab = self.tab_width();
        let tabs = (columns / tab) as usize;
        let spaces = (columns % tab) as usize;
        let mut out = String::with_capacity(tabs + spaces);
        out.extend(std::iter::repeat_n('\t', tabs));
        out.extend(std::iter::repeat_n(' ', spaces));
        out
    }

    /// `columns` shifted by `levels` indent steps, clamped at zero.
    pub fn shift(&self, columns: u16, levels: i32) -> u16 {
        let delta = self.step() as i32 * levels;
        let shifted = columns as i32 + delta;
        shifted.clamp(0, u16::MAX as i32) as u16
    }

    /// Whether `line` has no non-whitespace content.
    ///
    /// Blank lines are skipped by `>` / `<`: vim does not indent them,
    /// and doing so leaves trailing whitespace on lines the user never
    /// typed into.
    pub fn is_blank(line: &str) -> bool {
        line.bytes().all(|b| b == b' ' || b == b'\t' || b == b'\r')
    }

    /// The replacement leading-whitespace for `line` shifted by
    /// `levels`, or `None` when nothing should change.
    ///
    /// `None` covers both "blank line" and "the rendered result is
    /// byte-identical to what is already there" — the second matters
    /// because emitting a no-op edit still costs an undo entry and a
    /// render invalidation.
    pub fn reindented_prefix(&self, line: &str, levels: i32) -> Option<(usize, String)> {
        if Self::is_blank(line) {
            return None;
        }
        let len = Self::indent_len(line);
        let target = self.shift(self.columns_of(line), levels);
        let rendered = self.render(target);
        if rendered.as_bytes() == &line.as_bytes()[..len] {
            return None;
        }
        Some((len, rendered))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spaces4() -> IndentUnit {
        IndentUnit::new(4, true, 4)
    }
    fn tabs4() -> IndentUnit {
        IndentUnit::new(4, false, 4)
    }

    #[test]
    fn columns_counts_spaces() {
        assert_eq!(spaces4().columns_of("  hi"), 2);
        assert_eq!(spaces4().columns_of("hi"), 0);
    }

    #[test]
    fn a_tab_advances_to_the_next_tabstop_not_by_one() {
        // The whole reason indent is measured in columns rather than
        // bytes: at tabstop=4 these are the same indent.
        assert_eq!(spaces4().columns_of("\thi"), 4);
        assert_eq!(spaces4().columns_of("    hi"), 4);
        // A partial run then a tab still lands on the stop.
        assert_eq!(spaces4().columns_of("  \thi"), 4);
        assert_eq!(spaces4().columns_of("\t\thi"), 8);
    }

    #[test]
    fn tabstop_is_independent_of_shiftwidth() {
        // shiftwidth=2, tabstop=8: a tab is still 8 columns wide.
        let u = IndentUnit::new(2, true, 8);
        assert_eq!(u.columns_of("\thi"), 8);
        assert_eq!(u.step(), 2);
    }

    #[test]
    fn render_expands_or_tabs_per_expandtab() {
        assert_eq!(spaces4().render(6), "      ");
        // 6 columns at tabstop 4 = one tab + two spaces.
        assert_eq!(tabs4().render(6), "\t  ");
        assert_eq!(tabs4().render(8), "\t\t");
        assert_eq!(tabs4().render(0), "");
    }

    #[test]
    fn render_round_trips_through_columns_of() {
        for cols in 0u16..40 {
            for u in [spaces4(), tabs4(), IndentUnit::new(3, false, 8)] {
                let s = u.render(cols);
                assert_eq!(u.columns_of(&s), cols, "unit {u:?} cols {cols}");
            }
        }
    }

    #[test]
    fn shift_clamps_at_zero() {
        assert_eq!(spaces4().shift(2, -1), 0);
        assert_eq!(spaces4().shift(0, -3), 0);
        assert_eq!(spaces4().shift(4, 1), 8);
    }

    #[test]
    fn zero_width_is_treated_as_one_not_as_a_divide_by_zero() {
        let u = IndentUnit::new(0, true, 0);
        assert_eq!(u.step(), 1);
        assert_eq!(u.tab_width(), 1);
        assert_eq!(u.columns_of("\t\t"), 2);
    }

    #[test]
    fn blank_lines_are_left_alone() {
        assert!(IndentUnit::is_blank(""));
        assert!(IndentUnit::is_blank("   "));
        assert!(IndentUnit::is_blank("\t \t"));
        assert!(!IndentUnit::is_blank("  x"));
        assert_eq!(spaces4().reindented_prefix("   ", 1), None);
        assert_eq!(spaces4().reindented_prefix("", 1), None);
    }

    #[test]
    fn a_no_op_reindent_produces_no_edit() {
        // Already at column 0 and dedenting: nothing to write.
        assert_eq!(spaces4().reindented_prefix("hi", -1), None);
        // Already rendered exactly as the unit would render it.
        assert_eq!(spaces4().reindented_prefix("    hi", 0), None);
    }

    #[test]
    fn reindent_replaces_the_whole_prefix_normalising_style() {
        // Tab-indented line, expandtab on: the prefix is rewritten as
        // spaces, which is what vim's >> does.
        let (len, s) = spaces4().reindented_prefix("\thi", 1).unwrap();
        assert_eq!(len, 1);
        assert_eq!(s, "        ");

        // Space-indented line, expandtab off: rewritten as tabs.
        let (len, s) = tabs4().reindented_prefix("    hi", 1).unwrap();
        assert_eq!(len, 4);
        assert_eq!(s, "\t\t");
    }

    #[test]
    fn indent_method_parses_its_labels() {
        assert_eq!(IndentMethod::parse_label("keep"), Ok(IndentMethod::Keep));
        assert_eq!(
            IndentMethod::parse_label("syntax"),
            Ok(IndentMethod::Syntax)
        );
        assert!(IndentMethod::parse_label("bogus").is_err());
        assert_eq!(IndentMethod::default(), IndentMethod::Syntax);
    }
}
