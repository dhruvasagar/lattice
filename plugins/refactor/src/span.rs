//! Ordered positions and spans — the arithmetic the actions need, kept apart
//! from the WIT records so it can be tested without a host.
//!
//! A position is `(line, byte-in-line)`, the shape the tree seam's
//! `byte-range` and the editor's cursor both use. A [`Span`] is half-open,
//! as tree-sitter's ranges are; a Visual selection is inclusive of its
//! head's character, and [`after_char`] / [`last_char`] convert between the
//! two.

use crate::lattice::plugin_host::buffer::Document;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Pos {
    pub line: u32,
    pub byte: u32,
}

/// `[start, end)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Span {
    pub start: Pos,
    pub end: Pos,
}

impl Span {
    /// RF.4g: the span as the opaque `args.bytes` a fallback command carries
    /// — four little-endian `u32`s, start then end.
    pub fn to_bytes(self) -> Vec<u8> {
        [
            self.start.line,
            self.start.byte,
            self.end.line,
            self.end.byte,
        ]
        .iter()
        .flat_map(|n| n.to_le_bytes())
        .collect()
    }

    /// The inverse of [`Self::to_bytes`]; `None` for anything else.
    pub fn from_bytes(bytes: &[u8]) -> Option<Span> {
        if bytes.len() != 16 {
            return None;
        }
        let n = |i: usize| {
            let mut word = [0u8; 4];
            word.copy_from_slice(&bytes[i * 4..i * 4 + 4]);
            u32::from_le_bytes(word)
        };
        Some(Span {
            start: Pos {
                line: n(0),
                byte: n(1),
            },
            end: Pos {
                line: n(2),
                byte: n(3),
            },
        })
    }

    /// Whether this span contains all of `other`.
    pub fn covers(self, other: Span) -> bool {
        self.start <= other.start && other.end <= self.end
    }
}

/// Where a line's text comes from. The document in production; a slice of
/// strings in the tests.
pub trait Lines {
    /// Line `n` without its newline, `None` past the last.
    fn line_text(&self, n: u32) -> Option<String>;
}

impl Lines for Document {
    fn line_text(&self, n: u32) -> Option<String> {
        self.line(n)
    }
}

/// The position just past the character at `p` — the exclusive end of a
/// selection whose head is `p`. At or beyond a line's end (an empty line,
/// or the cursor resting on the newline) that is the next line's start, so
/// the span still covers the line break the head sits on.
pub fn after_char(lines: &impl Lines, p: Pos) -> Pos {
    let text = lines.line_text(p.line).unwrap_or_default();
    let at = p.byte as usize;
    match text.get(at..).and_then(|rest| rest.chars().next()) {
        Some(c) => Pos {
            line: p.line,
            byte: p.byte + c.len_utf8() as u32,
        },
        None => Pos {
            line: p.line + 1,
            byte: 0,
        },
    }
}

/// The start of `span`'s last character — the head of a Visual selection
/// covering it. A span ending at a line's start ends on the previous line's
/// break, which a selection's head names as that line's last character (or
/// its start, when the line is empty).
pub fn last_char(lines: &impl Lines, span: Span) -> Pos {
    let end = span.end;
    if end <= span.start {
        return span.start;
    }
    if end.byte > 0 {
        let text = lines.line_text(end.line).unwrap_or_default();
        let upto = (end.byte as usize).min(text.len());
        let back = text
            .get(..upto)
            .and_then(|s| s.chars().next_back())
            .map_or(1, char::len_utf8);
        return Pos {
            line: end.line,
            byte: end.byte.saturating_sub(back as u32),
        };
    }
    let line = end.line.saturating_sub(1);
    let text = lines.line_text(line).unwrap_or_default();
    let byte = text
        .chars()
        .next_back()
        .map_or(0, |c| (text.len() - c.len_utf8()) as u32);
    Pos { line, byte }
}

/// Where text inserted at `start` ends: past its last line break, the column
/// is the length of what follows it; with none, `start` moved along the line.
pub fn advance(start: Pos, text: &str) -> Pos {
    match text.rfind('\n') {
        Some(i) => Pos {
            line: start.line + text.matches('\n').count() as u32,
            byte: (text.len() - i - 1) as u32,
        },
        None => Pos {
            line: start.line,
            byte: start.byte + text.len() as u32,
        },
    }
}

/// What an editing command comes to: one replacement and where the cursor
/// lands, or the reason there is nothing to do. Every structural edit is one
/// replacement, so one undo step by construction.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Edit {
        range: Span,
        text: String,
        cursor: Pos,
    },
    Refuse(&'static str),
}

/// The text of `span`.
pub fn text(doc: &Document, span: Span) -> String {
    doc.get_text_range(span.into()).unwrap_or_default()
}

/// The span `n` covers.
pub fn span_of(n: &crate::lattice::plugin_host::tree_sitter::Node) -> Span {
    Span::from(n.byte_range())
}

/// `span` with whitespace and list separators (`,` `;`) trimmed from both
/// ends. An operator's range arrives however its source drew it: `aa`
/// includes the comma after the argument and `ia` does not, and both mean
/// the argument.
pub fn trim_list_edges(doc: &Document, span: Span) -> Span {
    let body = text(doc, span);
    let edge = |c: char| c.is_whitespace() || c == ',' || c == ';';
    let lead = body.len() - body.trim_start_matches(edge).len();
    let kept = body.trim_end_matches(edge).len();
    if kept <= lead {
        return Span {
            start: span.start,
            end: span.start,
        };
    }
    Span {
        start: advance(span.start, &body[..lead]),
        end: advance(span.start, &body[..kept]),
    }
}

/// The innermost of nested `spans`: the latest start, then the earliest
/// end. A length comparison is wrong for two spans over the same lines.
pub fn innermost(spans: impl Iterator<Item = Span>) -> Option<Span> {
    spans.max_by_key(|s| (s.start, std::cmp::Reverse(s.end)))
}

/// The smallest named node whose span covers `span`.
pub fn covering_node(
    tree: &crate::lattice::plugin_host::tree_sitter::TreeSnapshot,
    span: Span,
) -> Option<crate::lattice::plugin_host::tree_sitter::Node> {
    let mut node = tree.node_at(span.start.into());
    while let Some(n) = node {
        if n.is_named() && span_of(&n).covers(span) {
            return Some(n);
        }
        node = n.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Text(&'static [&'static str]);
    impl Lines for Text {
        fn line_text(&self, n: u32) -> Option<String> {
            self.0.get(n as usize).map(|s| s.to_string())
        }
    }

    fn p(line: u32, byte: u32) -> Pos {
        Pos { line, byte }
    }

    #[test]
    fn after_char_steps_over_one_character_however_wide() {
        let t = Text(&["aé", ""]);
        assert_eq!(after_char(&t, p(0, 0)), p(0, 1));
        assert_eq!(after_char(&t, p(0, 1)), p(0, 3), "é is two bytes");
        assert_eq!(after_char(&t, p(0, 3)), p(1, 0), "past the end: the break");
        assert_eq!(after_char(&t, p(1, 0)), p(2, 0), "an empty line's break");
    }

    #[test]
    fn last_char_is_the_head_of_the_selection_covering_a_span() {
        let t = Text(&["foo(bar)", "é", ""]);
        let s = |a, b| Span { start: a, end: b };
        assert_eq!(last_char(&t, s(p(0, 4), p(0, 7))), p(0, 6));
        assert_eq!(
            last_char(&t, s(p(1, 0), p(1, 2))),
            p(1, 0),
            "é is two bytes"
        );
        assert_eq!(
            last_char(&t, s(p(0, 0), p(1, 0))),
            p(0, 7),
            "ending at the next line's start: the line's last character"
        );
        assert_eq!(last_char(&t, s(p(2, 0), p(3, 0))), p(2, 0), "an empty line");
        assert_eq!(last_char(&t, s(p(0, 2), p(0, 2))), p(0, 2), "empty span");
    }

    #[test]
    fn the_two_conversions_invert_each_other() {
        let t = Text(&["let x = f(a, bé);"]);
        for start in [0u32, 4, 10] {
            for head in [start, 11, 13] {
                let span = Span {
                    start: p(0, start),
                    end: after_char(&t, p(0, head)),
                };
                assert_eq!(last_char(&t, span), p(0, head));
            }
        }
    }

    #[test]
    fn advance_follows_inserted_text_across_lines() {
        assert_eq!(advance(p(2, 4), ""), p(2, 4));
        assert_eq!(advance(p(2, 4), "ab, "), p(2, 8));
        assert_eq!(advance(p(2, 4), "a();\n    "), p(3, 4));
        assert_eq!(advance(p(0, 0), "x\n\nyz"), p(2, 2));
    }

    #[test]
    fn covers_includes_equal_spans() {
        let a = Span {
            start: p(0, 2),
            end: p(0, 8),
        };
        assert!(a.covers(a));
        assert!(a.covers(Span {
            start: p(0, 3),
            end: p(0, 8)
        }));
        assert!(!a.covers(Span {
            start: p(0, 1),
            end: p(0, 8)
        }));
        assert!(!a.covers(Span {
            start: p(0, 2),
            end: p(1, 0)
        }));
    }
}
