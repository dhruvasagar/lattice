//! Where should this line start?
//!
//! The indent engine, beside its peers: [`crate::text_objects`] and
//! [`crate::motions`] are the same shape -- computation over the parse
//! tree, driven by `.scm` files this crate already owns. IN.2 adds the
//! `indents.scm` query evaluator here; IN.1 ships the **lexical**
//! half below, which is what runs when no tree is available.
//!
//! Pure and synchronous by construction -- no I/O, no async, no host
//! state -- because every consumer sits on the keystroke path
//! (`docs/dev/architecture/auto-indent.md` §2).
//!
//! The [`IndentUnit`] value itself lives in `lattice-core`, not here:
//! the `>` / `<` operators in `lattice-grammar` consume it, and this
//! crate depends on `lattice-grammar`, so owning it here would be a
//! cycle.
//!
//! Two policies over one mechanism
//! ------------------------------
//!
//! - [`IndentMethod::Keep`] -- copy the previous non-blank line's
//!   indent. Vim's `autoindent`. No scan, no cleverness.
//! - [`IndentMethod::Syntax`]'s **fallback** -- the copy, plus one level
//!   if the previous line leaves a bracket unclosed, minus one if the
//!   target line opens with a closer. Vim's `smartindent`, roughly.
//!
//! Vim keeps these separate for a reason worth preserving: the bracket
//! rule misfires in a language where `{` is not a block opener, and
//! `keep` is what a user picks when they want the dumbest predictable
//! thing. Rather than special-casing that, the bracket sets are
//! **per-language** and languages with no bracket notion (plain text,
//! markdown) have empty sets -- at which point the bridge degrades to
//! `keep` on its own.
//!
//! What this deliberately does not do
//! ----------------------------------
//!
//! The scan is **lexical**, so it cannot tell a brace in code from a
//! brace in a string or a comment. `println!("{")` counts as an opener
//! here. That is a known and accepted wrong answer: the whole point of
//! this half is to be the thing that still works when no parse tree is
//! available, and a scan sophisticated enough to track string state
//! per-language would be a worse, slower duplicate of what IN.2's
//! tree-sitter path does properly. When the tree is available,
//! `syntax` uses it and never reaches here.

use lattice_core::{IndentMethod, IndentUnit};

use crate::Lang;

/// Per-language bracket sets for the opener/closer scan.
///
/// Empty sets are meaningful, not a null case: they are how a language
/// with no bracket-block notion opts out of the scan and gets pure
/// `keep` behaviour from the same code path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BracketSyntax {
    pub openers: &'static [u8],
    pub closers: &'static [u8],
}

impl BracketSyntax {
    /// `{`, `(`, `[` -- the C-family set, correct for every bundled
    /// language whose blocks are brace-delimited.
    pub const BRACES: Self = Self {
        openers: b"{([",
        closers: b"})]",
    };

    /// No brackets: the scan is skipped entirely and the bridge
    /// behaves as `keep`.
    pub const NONE: Self = Self {
        openers: b"",
        closers: b"",
    };

    /// The bracket sets for a language.
    ///
    /// Prose languages get [`Self::NONE`] so a stray brace in a
    /// sentence does not indent the next line. Everything else gets
    /// [`Self::BRACES`] -- including the indent-sensitive languages
    /// (Python, YAML), where brackets still bound continuation lines
    /// even though blocks are not brace-delimited, which is exactly
    /// the case the scan gets right.
    pub fn for_lang(lang: Lang) -> Self {
        match lang {
            Lang::Plain | Lang::Markdown => Self::NONE,
            _ => Self::BRACES,
        }
    }

    fn is_empty(&self) -> bool {
        self.openers.is_empty() && self.closers.is_empty()
    }

    /// Net bracket depth `line` leaves open. Negative when the line
    /// closes more than it opens.
    fn net_depth(&self, line: &str) -> i32 {
        let mut depth = 0i32;
        for b in line.bytes() {
            if self.openers.contains(&b) {
                depth += 1;
            } else if self.closers.contains(&b) {
                depth -= 1;
            }
        }
        depth
    }

    /// Whether `line`'s first non-whitespace byte is a closer.
    fn opens_with_closer(&self, line: &str) -> bool {
        line.bytes()
            .find(|b| *b != b' ' && *b != b'\t')
            .is_some_and(|b| self.closers.contains(&b))
    }
}

/// The whitespace a newly created line should start with.
///
/// `prev` is the text that ends up ABOVE the new line, which differs
/// by the key that created it:
///
/// - `o` / `O` -- the nearest non-blank line. (Vim takes `O`'s indent
///   from the line it pushes down, which is the line the cursor is on,
///   so both use the same source.)
/// - `<CR>` -- the **head**, i.e. the text before the cursor, not the
///   whole line. In `foo(a, |b)` the whole line is bracket-balanced
///   while the head leaves `(` open; only the head gives the right
///   answer.
///
/// `None` means there is nothing above, i.e. the top of the buffer.
///
/// `next` is the text that will follow on the new line, used only for
/// the closer check. Pass `None` for the ordinary
/// create-an-empty-line case; pass the tail for `<CR>` pressed
/// mid-line, where the text after the cursor moves down with it and a
/// leading `}` should dedent.
///
/// Returns the whitespace string, not a column count, because the
/// caller splices it into an edit and the rendering (tabs vs spaces)
/// is the unit's business.
pub fn indent_for_new_line(
    method: IndentMethod,
    prev: Option<&str>,
    next: Option<&str>,
    unit: IndentUnit,
    brackets: BracketSyntax,
) -> String {
    let columns = indent_columns_for_new_line(method, prev, next, unit, brackets);
    unit.render(columns)
}

/// [`indent_for_new_line`] in display columns, before rendering.
/// Separate so IN.2's engine can compare its own answer against the
/// fallback's without allocating.
pub fn indent_columns_for_new_line(
    method: IndentMethod,
    prev: Option<&str>,
    next: Option<&str>,
    unit: IndentUnit,
    brackets: BracketSyntax,
) -> u16 {
    if matches!(method, IndentMethod::None) {
        return 0;
    }
    let Some(prev) = prev else { return 0 };
    let base = unit.columns_of(prev);

    // `Keep` is a pure copy -- see the module doc. `Syntax` reaching
    // here means the tree was unavailable, and the bracket scan is the
    // best guess left.
    if matches!(method, IndentMethod::Keep) || brackets.is_empty() {
        return base;
    }

    let mut columns = base;
    if brackets.net_depth(prev) > 0 {
        columns = unit.shift(columns, 1);
    }
    if next.is_some_and(|n| brackets.opens_with_closer(n)) {
        columns = unit.shift(columns, -1);
    }
    columns
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit() -> IndentUnit {
        IndentUnit::new(4, true, 4)
    }

    fn syntax_indent(prev: Option<&str>, next: Option<&str>) -> String {
        indent_for_new_line(
            IndentMethod::Syntax,
            prev,
            next,
            unit(),
            BracketSyntax::BRACES,
        )
    }

    fn keep_indent(prev: Option<&str>) -> String {
        indent_for_new_line(
            IndentMethod::Keep,
            prev,
            None,
            unit(),
            BracketSyntax::BRACES,
        )
    }

    #[test]
    fn none_is_always_column_zero() {
        assert_eq!(
            indent_for_new_line(
                IndentMethod::None,
                Some("        deeply indented"),
                None,
                unit(),
                BracketSyntax::BRACES,
            ),
            ""
        );
    }

    #[test]
    fn keep_copies_and_does_not_scan() {
        assert_eq!(keep_indent(Some("    x();")), "    ");
        // The distinguishing case: an unclosed opener. `keep` must NOT
        // add a level -- that is `smartindent`, not `autoindent`.
        assert_eq!(keep_indent(Some("    if x {")), "    ");
        assert_eq!(keep_indent(Some("no indent")), "");
        assert_eq!(keep_indent(None), "");
    }

    #[test]
    fn syntax_fallback_adds_a_level_after_an_unclosed_opener() {
        assert_eq!(syntax_indent(Some("    if x {"), None), "        ");
        assert_eq!(syntax_indent(Some("fn f() {"), None), "    ");
        // Balanced on the line: no extra level.
        assert_eq!(syntax_indent(Some("    f(a);"), None), "    ");
        assert_eq!(syntax_indent(Some("    if x { y() }"), None), "    ");
    }

    #[test]
    fn syntax_fallback_dedents_when_the_moved_tail_opens_with_a_closer() {
        // `<CR>` pressed just before a `}` that moves down with it.
        assert_eq!(syntax_indent(Some("        x();"), Some("}")), "    ");
        // Opener and closer together: they cancel.
        assert_eq!(syntax_indent(Some("    if x {"), Some("}")), "    ");
    }

    #[test]
    fn a_language_with_no_brackets_degrades_to_keep() {
        // Prose: a brace in a sentence must not indent the next line.
        let prose = indent_for_new_line(
            IndentMethod::Syntax,
            Some("  a sentence with a { in it"),
            None,
            unit(),
            BracketSyntax::NONE,
        );
        assert_eq!(prose, "  ");
        assert_eq!(BracketSyntax::for_lang(Lang::Markdown), BracketSyntax::NONE);
        assert_eq!(BracketSyntax::for_lang(Lang::Plain), BracketSyntax::NONE);
        assert_eq!(BracketSyntax::for_lang(Lang::Rust), BracketSyntax::BRACES);
    }

    #[test]
    fn indent_is_rendered_through_the_unit() {
        // noexpandtab: the copied indent comes back as a tab.
        let tabs = IndentUnit::new(4, false, 4);
        let out = indent_for_new_line(
            IndentMethod::Keep,
            Some("    x"),
            None,
            tabs,
            BracketSyntax::BRACES,
        );
        assert_eq!(out, "\t");
    }

    #[test]
    fn a_tab_indented_previous_line_is_measured_in_columns() {
        // The previous line uses a tab; expandtab is on, so the new
        // line gets the equivalent in spaces.
        assert_eq!(keep_indent(Some("\tx();")), "    ");
    }

    #[test]
    fn closing_more_than_opening_does_not_go_negative() {
        // A line that only closes: depth is negative, so no extra
        // level, and the copy is clamped at zero by `shift`.
        assert_eq!(syntax_indent(Some("}"), None), "");
        assert_eq!(syntax_indent(Some("    }"), Some("}")), "");
    }

    #[test]
    fn a_brace_inside_a_string_is_a_known_wrong_answer() {
        // Documented in the module header: the lexical scan cannot see
        // string state. Asserted so the limitation is visible in the
        // suite rather than discovered later, and so IN.2's engine has
        // a concrete case to prove it improves on.
        assert_eq!(
            syntax_indent(Some(r#"    println!("{");"#), None),
            "        ",
            "lexical scan counts a brace in a string literal"
        );
    }
}
