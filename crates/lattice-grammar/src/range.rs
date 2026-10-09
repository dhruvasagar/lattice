//! The vim grammar `Range` -- the dispatcher's range arg.
//!
//! Distinct from `lattice_protocol::position::Range` (which is a structural
//! `[start, end)` byte range used by edits and decorations). The grammar
//! `Range` carries vim's ex-syntax range forms: `:1,5`, `:%`, `:'<,'>`,
//! `:.,+10`, `Selection` (active visual region), plugin-supplied custom
//! ranges.

use serde::{Deserialize, Serialize};

use crate::registry::RangeId;

/// A vim range argument: *which lines* an ex-command or operator covers,
/// before it is resolved against a document.
///
/// Symbolic on purpose: `%` or `'<,'>` means different lines in different
/// buffers and at different times, so the value is carried unresolved (in a
/// [`crate::CommandInvocation`], a recorded macro, a plugin call) and each
/// consumer resolves it at apply time.
///
/// Resolution coverage is uneven today. The dispatcher's operator path
/// resolves `CurrentLine`, `Whole` and `Selection` and rejects `Span` /
/// `Custom` with [`crate::CommandError::InvalidArgs`]; `:narrow` resolves
/// every form (patterns fall back to the cursor line, `Custom` to the
/// cursor line). No parser in the tree produces `Span` or `Custom` yet.
///
/// # Examples
///
/// ```
/// use lattice_grammar::{Range, RangeBound};
///
/// // `:.,+10` -- from the cursor line to ten lines below it.
/// let r = Range::Span {
///     start: RangeBound::CurrentLine,
///     end: RangeBound::Offset {
///         base: Box::new(RangeBound::CurrentLine),
///         delta: 10,
///     },
/// };
/// assert!(matches!(r, Range::Span { .. }));
///
/// // `:'<,'>` -- the last Visual selection, via its marks.
/// let visual = Range::Span {
///     start: RangeBound::Mark('<'),
///     end: RangeBound::Mark('>'),
/// };
/// assert_ne!(visual, Range::Selection); // same lines, different form
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Range {
    /// `:1,5`, `:'<,'>`, `:.,+10`, etc. Both ends inclusive; a consumer
    /// that resolves `start` below `end` swaps them (vim asks, lattice
    /// swaps silently).
    Span {
        /// First line of the range.
        start: RangeBound,
        /// Last line of the range (inclusive).
        end: RangeBound,
    },
    /// `:.` -- the cursor's line.
    CurrentLine,
    /// `:%` -- every line of the buffer.
    Whole,
    /// The current Visual / active region (the default range while Visual
    /// is active). Unlike the other forms it keeps Visual's shape: the
    /// dispatcher resolves it charwise (head-inclusive), linewise or
    /// blockwise according to the selection's mode.
    Selection,
    /// Plugin-registered custom range (e.g., a git-hunk-range plugin),
    /// identified by its registry id.
    Custom(RangeId),
}

/// One end of a [`Range::Span`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RangeBound {
    /// Absolute line number, **0-based** (the user's `:3` is `Line(2)`);
    /// clamped to the last line on resolution.
    Line(u32),
    /// A mark's line (`'a`, `'<`, `'>`, etc.). `<` / `>` resolve to the last
    /// Visual selection's first / last line; an unset mark resolves to the
    /// cursor line.
    Mark(char),
    /// `.` -- the cursor's line.
    CurrentLine,
    /// `$` -- the buffer's last line.
    LastLine,
    /// `/foo/` -- the next line after the cursor matching the pattern
    /// (text without delimiters), wrapping at the end of the buffer.
    Pattern(String),
    /// `?foo?` -- the previous line before the cursor matching the pattern,
    /// wrapping at the start of the buffer.
    PatternBackward(String),
    /// Offset from another bound (`+1`, `-3`, `.+5`); the result is
    /// clamped to the buffer.
    Offset {
        /// The bound the offset is relative to.
        base: Box<RangeBound>,
        /// Signed line delta added to `base`'s line.
        delta: i32,
    },
}

/// The inclusive whole lines an operator's byte span covers, given its start
/// and end `(line, byte)` in either order.
///
/// A span ending at byte 0 of a later line is half-open: nothing on that line
/// is covered, so the last covered line is the one before. That's the shape a
/// forward exclusive motion leaves (`}`, `G`), and vim agrees:
/// `:h exclusive-linewise`, "the end is moved to the end of the previous line".
///
/// Known gap: a BACKWARD exclusive motion (`k` from column 0) also ends at byte
/// 0 of the cursor's own line, and from the span alone that can't be told apart,
/// so the cursor's line is dropped. Lattice has no linewise operator targets
/// yet (`dk` is charwise too); threading the cursor through is the fix when it
/// matters.
///
/// Shared by the narrow operator (`zn`) and the fold operator (`zf`), which is
/// why it lives here rather than in either.
///
/// Lines are 0-based; the returned `(first, last)` is inclusive and ordered.
///
/// # Examples
///
/// ```
/// use lattice_grammar::range::span_to_whole_lines;
///
/// // Ends mid-line on line 3: lines 0..=3 are covered.
/// assert_eq!(span_to_whole_lines(0, 0, 3, 5), (0, 3));
/// // Ends at byte 0 of line 3 (a forward exclusive motion like `}`):
/// // line 3 is not covered.
/// assert_eq!(span_to_whole_lines(0, 0, 3, 0), (0, 2));
/// // Reversed input is ordered.
/// assert_eq!(span_to_whole_lines(4, 2, 1, 7), (1, 4));
/// ```
pub fn span_to_whole_lines(
    start_line: u32,
    start_byte: u32,
    end_line: u32,
    end_byte: u32,
) -> (u32, u32) {
    let ((lo_line, _lo_byte), (hi_line, hi_byte)) = if start_line <= end_line {
        ((start_line, start_byte), (end_line, end_byte))
    } else {
        ((end_line, end_byte), (start_line, start_byte))
    };
    let mut end = hi_line;
    if hi_byte == 0 && end > lo_line {
        end -= 1;
    }
    (lo_line, end)
}

/// Why a `:` line's range could not be read or resolved. The text is vim's,
/// number and all, because that is what a user searches for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeError(pub String);

impl std::fmt::Display for RangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Split the range off the front of a `:` line: `1,5d` is `1,5` and `d`.
///
/// Returns the range, when the line opens with one, and what follows it
/// (left-trimmed). A line with no range comes back whole with `None`.
///
/// The address grammar is vim's (`:help cmdline-ranges`):
///
/// | Written | Meaning |
/// |---|---|
/// | `5` | line 5 |
/// | `.` | the cursor line |
/// | `$` | the last line |
/// | `%` | every line, `1,$` |
/// | `'a`, `'<`, `'>` | a mark's line |
/// | `/pat/`, `?pat?` | the next / previous line matching `pat` |
/// | `+3`, `-2`, `+`, `-` | an offset; on its own it is from the cursor line |
/// | `5,10` | from one address through another |
/// | `,10` and `5,` | a missing side is the cursor line |
///
/// Offsets chain (`.+3-1`, `$-5`, `/fn/+1`).
///
/// Not read: `;` (the second address counted from the first rather than
/// from the cursor), `\/` `\?` `\&` (the last search or substitute pattern),
/// and `*`. Each is an error rather than being taken for something else.
///
/// # Examples
///
/// ```
/// use lattice_grammar::range::parse_range_prefix;
/// use lattice_grammar::{Range, RangeBound};
///
/// let (range, rest) = parse_range_prefix("2,$d").unwrap();
/// assert_eq!(
///     range,
///     Some(Range::Span { start: RangeBound::Line(1), end: RangeBound::LastLine })
/// );
/// assert_eq!(rest, "d");
///
/// assert_eq!(parse_range_prefix("%s/a/b/").unwrap(), (Some(Range::Whole), "s/a/b/"));
/// assert_eq!(parse_range_prefix("write").unwrap(), (None, "write"));
/// ```
pub fn parse_range_prefix(input: &str) -> Result<(Option<Range>, &str), RangeError> {
    let line = input.trim_start();
    if let Some(rest) = line.strip_prefix('%') {
        return Ok((Some(Range::Whole), rest.trim_start()));
    }
    let (first, after_first) = parse_address(line)?;
    let after_first = after_first.trim_start();
    if let Some(after_sep) = after_first.strip_prefix(',') {
        let (second, rest) = parse_address(after_sep.trim_start())?;
        return Ok((
            Some(Range::Span {
                // A missing side is the cursor line: `,5` is `.,5`.
                start: first.unwrap_or(RangeBound::CurrentLine),
                end: second.unwrap_or(RangeBound::CurrentLine),
            }),
            rest.trim_start(),
        ));
    }
    if after_first.starts_with(';') {
        return Err(RangeError(
            "E492: `;` ranges are not supported; use `,`".to_string(),
        ));
    }
    match first {
        Some(bound) => Ok((
            Some(Range::Span {
                start: bound.clone(),
                end: bound,
            }),
            after_first,
        )),
        None => Ok((None, line)),
    }
}

/// One address off the front of `s`: an optional base, then any offsets.
/// `None` when `s` does not open with one.
fn parse_address(s: &str) -> Result<(Option<RangeBound>, &str), RangeError> {
    let mut rest = s;
    let mut bound: Option<RangeBound> = None;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 {
        let n: u32 = rest[..digits]
            .parse()
            .map_err(|_| RangeError("E16: Invalid range".to_string()))?;
        // `:0` is line 1's address for the commands lattice has; vim keeps
        // it distinct only for `:0put` and friends.
        bound = Some(RangeBound::Line(n.saturating_sub(1)));
        rest = &rest[digits..];
    } else if let Some(r) = rest.strip_prefix('.') {
        bound = Some(RangeBound::CurrentLine);
        rest = r;
    } else if let Some(r) = rest.strip_prefix('$') {
        bound = Some(RangeBound::LastLine);
        rest = r;
    } else if let Some(r) = rest.strip_prefix('\'') {
        let name = r
            .chars()
            .next()
            .ok_or_else(|| RangeError("E20: Mark not set".to_string()))?;
        bound = Some(RangeBound::Mark(name));
        rest = &r[name.len_utf8()..];
    } else if let Some(delim) = rest.chars().next().filter(|c| matches!(c, '/' | '?')) {
        let (pattern, r) = take_pattern(&rest[1..], delim);
        if pattern.is_empty() {
            return Err(RangeError(
                "E35: No previous regular expression".to_string(),
            ));
        }
        bound = Some(if delim == '/' {
            RangeBound::Pattern(pattern)
        } else {
            RangeBound::PatternBackward(pattern)
        });
        rest = r;
    } else if rest.starts_with('\\') || rest.starts_with('*') {
        return Err(RangeError(format!(
            "E492: the `{}` address is not supported",
            rest.chars().take(2).collect::<String>().trim_end()
        )));
    }
    // Offsets: `+3`, `-2`, a bare `+` or `-` (one line), chained.
    loop {
        let trimmed = rest.trim_start();
        let sign = match trimmed.as_bytes().first() {
            Some(b'+') => 1i32,
            Some(b'-') => -1i32,
            _ => break,
        };
        let after = &trimmed[1..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        let amount: i32 = if digits == 0 {
            1
        } else {
            after[..digits]
                .parse()
                .map_err(|_| RangeError("E16: Invalid range".to_string()))?
        };
        bound = Some(RangeBound::Offset {
            base: Box::new(bound.unwrap_or(RangeBound::CurrentLine)),
            delta: sign * amount,
        });
        rest = &after[digits..];
    }
    Ok((bound, rest))
}

/// The pattern up to an unescaped `delim`, and what follows the delimiter.
/// A pattern left open at the end of the line is the whole remainder, as in
/// vim (`:/foo` works). `\/` inside a `/…/` pattern is a literal slash.
fn take_pattern(s: &str, delim: char) -> (String, &str) {
    let mut pattern = String::new();
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some((_, next)) if next == delim => pattern.push(delim),
                Some((_, next)) => {
                    pattern.push('\\');
                    pattern.push(next);
                }
                None => pattern.push('\\'),
            }
            continue;
        }
        if c == delim {
            return (pattern, &s[i + c.len_utf8()..]);
        }
        pattern.push(c);
    }
    (pattern, "")
}

/// Finds a line matching a pattern: `(pattern, from, forward)` is the first
/// matching line strictly after `from` (or before it, when not `forward`),
/// wrapping around the buffer. `Err` carries the regex error; `Ok(None)` is
/// no match anywhere.
pub type LineSearch<'a> = dyn Fn(&str, u32, bool) -> Result<Option<u32>, String> + 'a;

/// What resolving a range needs to know about the buffer and the editor.
pub struct RangeEnv<'a> {
    /// The cursor's line, 0-based.
    pub cursor: u32,
    /// The buffer's last line, 0-based.
    pub last: u32,
    /// The mark table. `'<` and `'>` are asked of it like any other mark.
    pub marks: &'a dyn crate::registry::MarkResolver,
    /// How `/pat/` and `?pat?` find their line.
    pub search: &'a LineSearch<'a>,
}

/// Resolve a range to the inclusive 0-based lines it covers, in order.
///
/// `strict` is whether an address outside the buffer is an error (E16), as
/// it is when a command will act on the lines (`:1,999d`), or is pulled back
/// to the nearest line, as it is for a jump (`:999`).
///
/// A range written backwards (`:5,1`) is swapped. vim asks first; there is
/// nothing useful a "no" would do.
///
/// `Selection` and `Custom` are not line addresses and are not resolved
/// here: they come back as `None` for the caller, which knows what the
/// selection is.
pub fn resolve_lines(
    range: &Range,
    env: &RangeEnv<'_>,
    strict: bool,
) -> Result<Option<(u32, u32)>, RangeError> {
    match range {
        Range::CurrentLine => Ok(Some((env.cursor, env.cursor))),
        Range::Whole => Ok(Some((0, env.last))),
        Range::Selection | Range::Custom(_) => Ok(None),
        Range::Span { start, end } => {
            let a = resolve_bound(start, env, strict)?;
            let b = resolve_bound(end, env, strict)?;
            Ok(Some((a.min(b), a.max(b))))
        }
    }
}

fn resolve_bound(bound: &RangeBound, env: &RangeEnv<'_>, strict: bool) -> Result<u32, RangeError> {
    let line = signed_bound(bound, env)?;
    if strict && (line < 0 || line > i64::from(env.last)) {
        return Err(RangeError("E16: Invalid range".to_string()));
    }
    Ok(line.clamp(0, i64::from(env.last)) as u32)
}

/// A bound's line before it is held to the buffer, so that `$+1-1` is the
/// last line rather than being clamped halfway through.
fn signed_bound(bound: &RangeBound, env: &RangeEnv<'_>) -> Result<i64, RangeError> {
    let searched = |pattern: &str, forward: bool| -> Result<i64, RangeError> {
        match (env.search)(pattern, env.cursor, forward) {
            Ok(Some(line)) => Ok(i64::from(line)),
            Ok(None) => Err(RangeError(format!("E486: Pattern not found: {pattern}"))),
            Err(e) => Err(RangeError(format!("regex: {e}"))),
        }
    };
    Ok(match bound {
        RangeBound::Line(n) => i64::from(*n),
        RangeBound::CurrentLine => i64::from(env.cursor),
        RangeBound::LastLine => i64::from(env.last),
        RangeBound::Mark(name) => i64::from(
            env.marks
                .mark(*name)
                .ok_or_else(|| RangeError("E20: Mark not set".to_string()))?
                .line,
        ),
        RangeBound::Pattern(pattern) => searched(pattern, true)?,
        RangeBound::PatternBackward(pattern) => searched(pattern, false)?,
        RangeBound::Offset { base, delta } => signed_bound(base, env)? + i64::from(*delta),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn whole_range_renders_distinct_variant() {
        assert_ne!(Range::Whole, Range::CurrentLine);
        assert_ne!(Range::Whole, Range::Selection);
    }

    #[test]
    fn span_constructed_from_bounds() {
        let r = Range::Span {
            start: RangeBound::Line(0),
            end: RangeBound::Line(4),
        };
        match r {
            Range::Span { start, end } => {
                assert_eq!(start, RangeBound::Line(0));
                assert_eq!(end, RangeBound::Line(4));
            }
            _ => panic!("expected Span"),
        }
    }

    #[test]
    fn offset_bounds_compose() {
        let off = RangeBound::Offset {
            base: Box::new(RangeBound::CurrentLine),
            delta: 5,
        };
        match off {
            RangeBound::Offset { base, delta } => {
                assert_eq!(*base, RangeBound::CurrentLine);
                assert_eq!(delta, 5);
            }
            _ => panic!("expected Offset"),
        }
    }

    #[test]
    fn span_to_whole_lines_mid_line_end_is_inclusive() {
        // `j`-like: next line, end mid-line → both lines covered.
        assert_eq!(span_to_whole_lines(0, 0, 3, 5), (0, 3));
    }

    #[test]
    fn span_to_whole_lines_half_open_end_at_col0_drops_trailing_line() {
        // Forward exclusive motions end at column 0 of the line AFTER the
        // last content line → the last covered line is the previous one.
        assert_eq!(span_to_whole_lines(0, 0, 3, 0), (0, 2));
    }

    #[test]
    fn span_to_whole_lines_single_line() {
        assert_eq!(span_to_whole_lines(2, 0, 2, 4), (2, 2));
    }

    #[test]
    fn span_to_whole_lines_reversed_is_ordered() {
        // A backward span (end before start) is ordered first. This also pins
        // the documented `k`-from-column-0 gap: line 5 is dropped.
        assert_eq!(span_to_whole_lines(5, 0, 2, 0), (2, 4));
    }

    fn parsed(line: &str) -> (Option<Range>, String) {
        let (range, rest) = parse_range_prefix(line).expect("parses");
        (range, rest.to_string())
    }

    fn span(start: RangeBound, end: RangeBound) -> Option<Range> {
        Some(Range::Span { start, end })
    }

    fn off(base: RangeBound, delta: i32) -> RangeBound {
        RangeBound::Offset {
            base: Box::new(base),
            delta,
        }
    }

    #[test]
    fn a_line_with_no_range_comes_back_whole() {
        for line in ["write", "s/a/b/", "g/x/d", "", "d"] {
            assert_eq!(parsed(line), (None, line.to_string()));
        }
    }

    #[test]
    fn numbers_dot_dollar_and_percent() {
        use RangeBound::*;
        assert_eq!(parsed("1,5d"), (span(Line(0), Line(4)), "d".into()));
        assert_eq!(parsed(".,$ d"), (span(CurrentLine, LastLine), "d".into()));
        assert_eq!(parsed("%d"), (Some(Range::Whole), "d".into()));
        assert_eq!(parsed("7"), (span(Line(6), Line(6)), String::new()));
        assert_eq!(parsed("$"), (span(LastLine, LastLine), String::new()));
    }

    #[test]
    fn a_missing_side_is_the_cursor_line() {
        use RangeBound::*;
        assert_eq!(parsed(",5d"), (span(CurrentLine, Line(4)), "d".into()));
        assert_eq!(parsed("5,d"), (span(Line(4), CurrentLine), "d".into()));
    }

    #[test]
    fn offsets_default_to_the_cursor_and_chain() {
        use RangeBound::*;
        assert_eq!(
            parsed(".,+3d"),
            (span(CurrentLine, off(CurrentLine, 3)), "d".into())
        );
        assert_eq!(
            parsed("-2,$-1y"),
            (span(off(CurrentLine, -2), off(LastLine, -1)), "y".into())
        );
        assert_eq!(
            parsed(".+3-1"),
            (
                span(off(off(CurrentLine, 3), -1), off(off(CurrentLine, 3), -1)),
                String::new()
            )
        );
        assert_eq!(
            parsed("+"),
            (
                span(off(CurrentLine, 1), off(CurrentLine, 1)),
                String::new()
            )
        );
    }

    #[test]
    fn marks_and_patterns() {
        use RangeBound::*;
        assert_eq!(parsed("'a,'bd"), (span(Mark('a'), Mark('b')), "d".into()));
        assert_eq!(parsed("'<,'>d"), (span(Mark('<'), Mark('>')), "d".into()));
        assert_eq!(
            parsed("/fn main/,/^}/d"),
            (
                span(Pattern("fn main".into()), Pattern("^}".into())),
                "d".into()
            )
        );
        assert_eq!(
            parsed("?a\\?b?+1"),
            (
                span(
                    off(PatternBackward("a?b".into()), 1),
                    off(PatternBackward("a?b".into()), 1)
                ),
                String::new()
            )
        );
        // Left open at the end of the line, as vim allows.
        assert_eq!(
            parsed("/TODO"),
            (
                span(Pattern("TODO".into()), Pattern("TODO".into())),
                String::new()
            )
        );
    }

    #[test]
    fn what_is_not_supported_says_so_rather_than_guessing() {
        for line in ["1;5d", "\\/,5d", "*d"] {
            assert!(parse_range_prefix(line).is_err(), "{line}");
        }
    }

    fn env_at<'a>(
        cursor: u32,
        marks: &'a std::collections::HashMap<char, lattice_protocol::position::Position>,
        search: &'a LineSearch<'a>,
    ) -> RangeEnv<'a> {
        RangeEnv {
            cursor,
            last: 9,
            marks,
            search,
        }
    }

    fn lines(line: &str, cursor: u32, strict: bool) -> Result<(u32, u32), RangeError> {
        let marks = std::collections::HashMap::from([(
            'a',
            lattice_protocol::position::Position::new(7, 2),
        )]);
        // Line 6 is the only match.
        let search = |pattern: &str, from: u32, _forward: bool| match pattern {
            "bad(" => Err("unclosed group".to_string()),
            "six" if from != 6 => Ok(Some(6)),
            _ => Ok(None),
        };
        let (range, _) = parse_range_prefix(line).expect("parses");
        resolve_lines(
            &range.expect("a range"),
            &env_at(cursor, &marks, &search),
            strict,
        )
        .map(|r| r.expect("a line range"))
    }

    #[test]
    fn addresses_resolve_against_the_cursor_the_buffer_and_the_marks() {
        assert_eq!(lines("2,4", 0, true), Ok((1, 3)));
        assert_eq!(lines(".,+2", 5, true), Ok((5, 7)));
        assert_eq!(lines("%", 5, true), Ok((0, 9)));
        assert_eq!(lines("$-1,$", 0, true), Ok((8, 9)));
        assert_eq!(lines("'a,$", 0, true), Ok((7, 9)));
        assert_eq!(lines("/six/,'a", 0, true), Ok((6, 7)));
        assert_eq!(
            lines("$+1-1", 0, true),
            Ok((9, 9)),
            "held to the buffer once"
        );
    }

    #[test]
    fn a_backwards_range_is_swapped() {
        assert_eq!(lines("5,2", 0, true), Ok((1, 4)));
    }

    #[test]
    fn outside_the_buffer_is_an_error_for_a_command_and_a_clamp_for_a_jump() {
        assert_eq!(
            lines("1,99", 0, true),
            Err(RangeError("E16: Invalid range".into()))
        );
        assert_eq!(
            lines(".-5", 2, true),
            Err(RangeError("E16: Invalid range".into()))
        );
        assert_eq!(lines("99", 0, false), Ok((9, 9)));
        assert_eq!(lines(".-5", 2, false), Ok((0, 0)));
    }

    #[test]
    fn an_unset_mark_and_an_unmatched_pattern_are_errors() {
        assert_eq!(
            lines("'z", 0, true),
            Err(RangeError("E20: Mark not set".into()))
        );
        assert_eq!(
            lines("/nope/", 0, true),
            Err(RangeError("E486: Pattern not found: nope".into()))
        );
        assert!(
            lines("/bad(/", 0, true)
                .unwrap_err()
                .0
                .starts_with("regex:")
        );
    }
}
