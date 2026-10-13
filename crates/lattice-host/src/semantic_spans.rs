//! GD.6b: LSP semantic tokens as styled spans in the display matrix.
//!
//! Semantic tokens colour text; they never change a character. So they
//! are spans layered over the grammar's, resolved once in the cell
//! worker's row build, and every renderer paints the result. The TUI
//! used to merge them into its own spans at render time, which is why
//! the GPUI window — which paints the matrix and has no merge point —
//! showed none.
//!
//! Two halves:
//!
//! - [`spans_for_lines`] turns the decoded tokens covering a line range
//!   into per-line [`StyledSpan`]s. It runs in the worker, over the
//!   window being built, so its cost is the tokens in view.
//! - [`TokenStamps`] gives the actor an O(1) answer to "is this the
//!   token set the published rows were coloured from?", which is the
//!   `semantic_tokens` axis of `MatrixVersion`.
//!
//! Token *modifiers* (`deprecated`, `readonly`, …) are a second layer:
//! a display run holds one style, and a modifier says something more
//! about text that already has one. They ride the run's `flags`
//! ([`modifier_spans_for_line`]) and every consumer ORs them onto the
//! style's own (GD.6d).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lattice_core::BufferId;
use lattice_lsp::cache::{DecodedSemanticToken, LspSemanticTokensCache};
use lattice_syntax::{Style, StyledSpan};

/// The style a semantic token type paints with, or `None` to leave the
/// grammar's colour alone.
///
/// The names are the LSP-standard token types. A server may declare its
/// own beyond these (rust-analyzer has dozens — `brace`, `lifetime`,
/// `selfKeyword`); an unknown type answers `None` rather than guessing,
/// because a wrong colour over a right one is a regression and no
/// colour over a right one is nothing.
pub(crate) fn style_for_token_type(kind: &str) -> Option<Style> {
    Some(match kind {
        "keyword" | "modifier" => Style::Keyword,
        "type" | "class" | "struct" | "interface" | "enum" | "typeParameter" | "namespace" => {
            Style::Type
        }
        "function" | "method" | "macro" => Style::Function,
        "string" | "regexp" => Style::String,
        "number" => Style::Number,
        "comment" => Style::Comment,
        "operator" => Style::Operator,
        "variable" | "parameter" | "property" => Style::Variable,
        "enumMember" => Style::Constant,
        "decorator" => Style::Attribute,
        _ => return None,
    })
}

/// How a token's modifiers change the look of its text, as cell style
/// bits. `deprecated` is struck through; `readonly` and `static` are
/// italic; `defaultLibrary` is dimmed. The rest say nothing visible.
pub(crate) fn modifier_flags(modifiers: &[String]) -> u16 {
    use lattice_cells::cell_flags;
    let mut flags = 0;
    for m in modifiers {
        flags |= match m.as_str() {
            "deprecated" => cell_flags::STRIKETHROUGH,
            "readonly" | "static" => cell_flags::ITALIC,
            "defaultLibrary" => cell_flags::DIM,
            _ => 0,
        };
    }
    flags
}

/// The byte ranges of `text` (source line `line`) whose tokens carry a
/// visible modifier, with the style bits each adds.
///
/// Per line, and separate from [`spans_for_lines`], because it is
/// independent of the token's type: a `deprecated` token of a type with
/// no style of its own is still struck through. Empty — and
/// allocation-free — for a line with no such token, which is nearly
/// all of them.
pub(crate) fn modifier_spans_for_line(
    tokens: &[DecodedSemanticToken],
    line: u32,
    text: &str,
) -> Vec<(usize, usize, u16)> {
    let first = tokens.partition_point(|t| t.line < line);
    let mut out = Vec::new();
    for tok in &tokens[first..] {
        if tok.line != line {
            break;
        }
        let flags = modifier_flags(&tok.modifiers);
        if flags == 0 {
            continue;
        }
        let len = text.len();
        let start = (lattice_lsp::position::utf16_column_to_utf8_byte(text, tok.start_char)
            as usize)
            .min(len);
        let end = (lattice_lsp::position::utf16_column_to_utf8_byte(
            text,
            tok.start_char.saturating_add(tok.length),
        ) as usize)
            .min(len);
        if start < end {
            out.push((start, end, flags));
        }
    }
    out
}

/// Per-line spans for the tokens on lines `lo..hi`, indexed relative to
/// `lo` (the shape `highlight_lines` returns, so the two merge by
/// index).
///
/// `line_text` supplies a line's text for the UTF-16 → byte conversion;
/// it is only called for lines that carry a token. Tokens are read in
/// the order the server sent them, which the protocol requires to be
/// line order — a server that breaks that loses tokens here, it does
/// not corrupt rows.
pub(crate) fn spans_for_lines(
    tokens: &[DecodedSemanticToken],
    lo: u32,
    hi: u32,
    mut line_text: impl FnMut(u32) -> Option<String>,
) -> Vec<Vec<StyledSpan>> {
    let mut out: Vec<Vec<StyledSpan>> = vec![Vec::new(); hi.saturating_sub(lo) as usize];
    if out.is_empty() {
        return out;
    }
    let first = tokens.partition_point(|t| t.line < lo);
    let mut cached: Option<(u32, String)> = None;
    for tok in &tokens[first..] {
        if tok.line >= hi {
            break;
        }
        let Some(style) = style_for_token_type(&tok.token_type) else {
            continue;
        };
        if cached.as_ref().map(|(l, _)| *l) != Some(tok.line) {
            let Some(text) = line_text(tok.line) else {
                continue;
            };
            cached = Some((tok.line, text));
        }
        let Some((_, text)) = cached.as_ref() else {
            continue;
        };
        let len = text.len();
        let start = (lattice_lsp::position::utf16_column_to_utf8_byte(text, tok.start_char)
            as usize)
            .min(len);
        let end = (lattice_lsp::position::utf16_column_to_utf8_byte(
            text,
            tok.start_char.saturating_add(tok.length),
        ) as usize)
            .min(len);
        if start >= end {
            continue;
        }
        if let Some(line) = out.get_mut((tok.line - lo) as usize) {
            line.push(StyledSpan { start, end, style });
        }
    }
    out
}

/// Put `semantic` in front of the grammar `spans`, line by line, so a
/// token wins the row build's first-match lookup over the grammar scope
/// under it. Both are indexed relative to the same first line.
pub(crate) fn merge_semantic_spans(spans: &mut [Vec<StyledSpan>], semantic: Vec<Vec<StyledSpan>>) {
    for (line_spans, mut sem) in spans.iter_mut().zip(semantic) {
        if sem.is_empty() {
            continue;
        }
        sem.append(line_spans);
        *line_spans = sem;
    }
}

/// Which token set each buffer's rows were last asked to use.
///
/// The token cache is written from the LSP task, off the actor, by
/// replacing an `Arc`. So "did the tokens change?" is "is this a
/// different `Arc`?" — answered here by holding the last one seen, which
/// keeps its address from being reused, and handing out a counter that
/// only ever goes up. A content hash would be O(tokens) on every
/// publish; the address alone could repeat once the old set is freed.
#[derive(Debug, Default)]
pub(crate) struct TokenStamps {
    inner: Mutex<StampTable>,
}

#[derive(Debug, Default)]
struct StampTable {
    next: u64,
    seen: HashMap<BufferId, (Arc<LspSemanticTokensCache>, u64)>,
}

impl TokenStamps {
    /// The stamp for `current`: the same value for as long as the buffer
    /// keeps this exact token set, a new one the moment it is replaced,
    /// and `0` for no tokens at all.
    pub(crate) fn stamp(&self, id: BufferId, current: Option<&Arc<LspSemanticTokensCache>>) -> u64 {
        // A poisoned lock means a panic elsewhere mid-update; the table
        // is a cache of stamps, so carrying on with it is safe.
        let mut table = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(current) = current else {
            table.seen.remove(&id);
            return 0;
        };
        if let Some((held, stamp)) = table.seen.get(&id)
            && Arc::ptr_eq(held, current)
        {
            return *stamp;
        }
        table.next += 1;
        let stamp = table.next;
        table.seen.insert(id, (Arc::clone(current), stamp));
        stamp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(line: u32, start_char: u32, length: u32, kind: &str) -> DecodedSemanticToken {
        DecodedSemanticToken {
            line,
            start_char,
            length,
            token_type: kind.into(),
            modifiers: Vec::new(),
        }
    }

    fn cache(tokens: Vec<DecodedSemanticToken>) -> Arc<LspSemanticTokensCache> {
        Arc::new(LspSemanticTokensCache {
            document_version: 1,
            result_id: None,
            raw_data: Vec::new(),
            tokens,
        })
    }

    #[test]
    fn tokens_become_spans_on_their_own_lines() {
        let lines = ["fn main() {}", "let x = 1;"];
        let tokens = vec![
            tok(0, 0, 2, "keyword"),
            tok(0, 3, 4, "function"),
            tok(1, 4, 1, "variable"),
        ];
        let out = spans_for_lines(&tokens, 0, 2, |l| {
            lines.get(l as usize).map(|s| s.to_string())
        });
        assert_eq!(
            out[0],
            vec![
                StyledSpan {
                    start: 0,
                    end: 2,
                    style: Style::Keyword
                },
                StyledSpan {
                    start: 3,
                    end: 7,
                    style: Style::Function
                },
            ]
        );
        assert_eq!(
            out[1],
            vec![StyledSpan {
                start: 4,
                end: 5,
                style: Style::Variable
            }]
        );
    }

    /// The window is what gets built, so only its tokens are read and
    /// they are indexed from its first line.
    #[test]
    fn only_the_requested_lines_are_covered() {
        let tokens = vec![
            tok(0, 0, 1, "keyword"),
            tok(5, 0, 1, "keyword"),
            tok(9, 0, 1, "keyword"),
        ];
        let mut asked = Vec::new();
        let out = spans_for_lines(&tokens, 4, 7, |l| {
            asked.push(l);
            Some("x".into())
        });
        assert_eq!(out.len(), 3);
        assert!(out[0].is_empty() && out[2].is_empty());
        assert_eq!(out[1].len(), 1);
        assert_eq!(asked, vec![5], "only a line with a token has its text read");
    }

    /// LSP columns count UTF-16 units; spans are bytes.
    #[test]
    fn columns_are_converted_from_utf16_to_bytes() {
        // "é" is one UTF-16 unit and two bytes.
        let out = spans_for_lines(&[tok(0, 2, 2, "type")], 0, 1, |_| Some("é Ty".into()));
        assert_eq!(
            out[0],
            vec![StyledSpan {
                start: 3,
                end: 5,
                style: Style::Type
            }]
        );
    }

    /// Tokens are computed for a text the buffer may have moved on from.
    /// A token past the end of today's line is dropped or clipped, never
    /// an out-of-range span.
    #[test]
    fn a_token_past_the_end_of_the_line_is_clipped() {
        let out = spans_for_lines(
            &[tok(0, 1, 50, "string"), tok(0, 80, 3, "string")],
            0,
            1,
            |_| Some("abc".into()),
        );
        assert_eq!(
            out[0],
            vec![StyledSpan {
                start: 1,
                end: 3,
                style: Style::String
            }]
        );
    }

    #[test]
    fn an_unknown_token_type_leaves_the_grammar_colour() {
        assert_eq!(style_for_token_type("brace"), None);
        let out = spans_for_lines(&[tok(0, 0, 1, "brace")], 0, 1, |_| Some("{".into()));
        assert!(out[0].is_empty());
    }

    #[test]
    fn a_token_wins_over_the_grammar_span_under_it() {
        let grammar = StyledSpan {
            start: 0,
            end: 4,
            style: Style::Variable,
        };
        let semantic = StyledSpan {
            start: 0,
            end: 4,
            style: Style::Function,
        };
        let mut spans = vec![vec![grammar], vec![grammar]];
        merge_semantic_spans(&mut spans, vec![vec![semantic], Vec::new()]);
        assert_eq!(spans[0], vec![semantic, grammar]);
        assert_eq!(spans[1], vec![grammar], "a line with no token is untouched");
    }

    #[test]
    fn modifiers_become_style_bits_on_their_tokens_bytes() {
        use lattice_cells::cell_flags;
        let mut dep = tok(2, 3, 3, "function");
        dep.modifiers = vec!["deprecated".into(), "static".into()];
        // A type with no style of its own still carries its modifier.
        let mut odd = tok(2, 8, 1, "brace");
        odd.modifiers = vec!["defaultLibrary".into()];
        let plain = tok(2, 0, 2, "keyword");
        let other_line = {
            let mut t = tok(3, 0, 2, "keyword");
            t.modifiers = vec!["deprecated".into()];
            t
        };
        let tokens = vec![plain, dep, odd, other_line];
        assert_eq!(
            modifier_spans_for_line(&tokens, 2, "fn old() {}"),
            vec![
                (3, 6, cell_flags::STRIKETHROUGH | cell_flags::ITALIC),
                (8, 9, cell_flags::DIM),
            ]
        );
        assert!(modifier_spans_for_line(&tokens, 0, "x").is_empty());
        assert_eq!(modifier_flags(&["async".into()]), 0);
    }

    #[test]
    fn the_stamp_holds_for_one_token_set_and_moves_for_the_next() {
        let stamps = TokenStamps::default();
        let id = BufferId(7);
        let a = cache(vec![tok(0, 0, 1, "keyword")]);
        let first = stamps.stamp(id, Some(&a));
        assert_ne!(first, 0);
        assert_eq!(stamps.stamp(id, Some(&a)), first);

        // Same content, different response: still a new set.
        let b = cache(vec![tok(0, 0, 1, "keyword")]);
        let second = stamps.stamp(id, Some(&b));
        assert_ne!(second, first);

        assert_eq!(stamps.stamp(id, None), 0);
        // Coming back after "none" never reuses an old stamp.
        let third = stamps.stamp(id, Some(&a));
        assert!(third != first && third != second && third != 0);
    }

    #[test]
    fn two_buffers_do_not_share_a_stamp() {
        let stamps = TokenStamps::default();
        let a = cache(Vec::new());
        assert_ne!(
            stamps.stamp(BufferId(1), Some(&a)),
            stamps.stamp(BufferId(2), Some(&a))
        );
    }
}
