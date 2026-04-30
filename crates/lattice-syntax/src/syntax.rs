//! `Syntax`: per-document tree-sitter state.
//!
//! Wraps `tree_sitter_highlight::Highlighter` + a precomputed
//! `HighlightConfiguration` for the document's language. The highlighter
//! handles overlap resolution (innermost capture wins) and produces a flat
//! event stream that we walk to assemble per-line `StyledSpan`s.
//!
//! Reparse is a `parse(source: &str)` call. Internally we re-run the
//! highlighter on the full source. Incremental reparse via `Tree::edit`
//! is a follow-up; the public surface won't change because today's full
//! reparse stays correct.

use thiserror::Error;
use tree_sitter_highlight::{Error as TsHighlightError, HighlightConfiguration, HighlightEvent, Highlighter};

use crate::lang::Lang;
use crate::style::{CAPTURE_NAMES, Style, StyledSpan, capture_index_to_style};

#[derive(Debug, Error)]
pub enum SyntaxError {
    #[error("tree-sitter language error: {0}")]
    Language(String),

    #[error("highlighter error: {0}")]
    Highlight(#[from] TsHighlightError),
}

pub struct Syntax {
    lang: Lang,
    config: HighlightConfiguration,
    highlighter: Highlighter,
    /// Last-parsed source bytes. Owned so callers can call `highlight_lines`
    /// independently of holding the source.
    source: Vec<u8>,
}

impl std::fmt::Debug for Syntax {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Syntax")
            .field("lang", &self.lang)
            .field("source_bytes", &self.source.len())
            .finish_non_exhaustive()
    }
}

impl Syntax {
    /// Build a `Syntax` for the given language. `Lang::Plain` returns `None`
    /// because there's nothing to parse.
    pub fn for_language(lang: Lang) -> Result<Option<Self>, SyntaxError> {
        let config = match lang {
            Lang::Plain => return Ok(None),
            Lang::Rust => build_config(
                tree_sitter_rust::LANGUAGE.into(),
                "rust",
                tree_sitter_rust::HIGHLIGHTS_QUERY,
                tree_sitter_rust::INJECTIONS_QUERY,
                "",
            )?,
            Lang::Python => build_config(
                tree_sitter_python::LANGUAGE.into(),
                "python",
                tree_sitter_python::HIGHLIGHTS_QUERY,
                "",
                "",
            )?,
            Lang::JavaScript => build_config(
                tree_sitter_javascript::LANGUAGE.into(),
                "javascript",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_javascript::INJECTIONS_QUERY,
                tree_sitter_javascript::LOCALS_QUERY,
            )?,
        };

        Ok(Some(Self {
            lang,
            config,
            highlighter: Highlighter::new(),
            source: Vec::new(),
        }))
    }

    pub fn lang(&self) -> Lang {
        self.lang
    }

    /// Replace the cached source. Called whenever the document mutates.
    /// Phase 3 is a full reparse; the seam stays the same when we move to
    /// incremental.
    pub fn parse(&mut self, source: &str) {
        self.source.clear();
        self.source.extend_from_slice(source.as_bytes());
    }

    /// Compute styled spans for each line in `[start_line, end_line)`.
    /// `start_line` and `end_line` are 0-based and clamped to the source's
    /// line count.
    ///
    /// Returns one `Vec<StyledSpan>` per line in the requested range. Spans
    /// use line-relative byte offsets (consistent with the renderer's
    /// existing assumption).
    pub fn highlight_lines(
        &mut self,
        start_line: u32,
        end_line: u32,
    ) -> Result<Vec<Vec<StyledSpan>>, SyntaxError> {
        if end_line <= start_line {
            return Ok(Vec::new());
        }

        let line_starts = compute_line_starts(&self.source);
        let total_lines = line_starts.len().saturating_sub(1).max(1) as u32;
        let end_line = end_line.min(total_lines + 1);
        if start_line >= end_line {
            return Ok(Vec::new());
        }

        let mut result: Vec<Vec<StyledSpan>> =
            (0..(end_line - start_line)).map(|_| Vec::new()).collect();

        let highlights = self
            .highlighter
            .highlight(&self.config, &self.source, None, |_| None)?;

        // Walk the highlight event stream maintaining a stack of active
        // styles. Each `Source { start, end }` event produces styled spans
        // distributed across the lines it covers.
        let mut stack: Vec<Style> = Vec::with_capacity(8);
        for event in highlights {
            match event? {
                HighlightEvent::HighlightStart(h) => {
                    stack.push(capture_index_to_style(h.0));
                }
                HighlightEvent::HighlightEnd => {
                    stack.pop();
                }
                HighlightEvent::Source { start, end } => {
                    let style = stack.last().copied().unwrap_or(Style::Default);
                    if matches!(style, Style::Default) {
                        continue;
                    }
                    distribute_span_across_lines(
                        start,
                        end,
                        style,
                        &line_starts,
                        start_line,
                        end_line,
                        &mut result,
                    );
                }
            }
        }
        Ok(result)
    }
}

fn build_config(
    language: tree_sitter::Language,
    name: &str,
    highlights: &str,
    injections: &str,
    locals: &str,
) -> Result<HighlightConfiguration, SyntaxError> {
    let mut config = HighlightConfiguration::new(language, name, highlights, injections, locals)
        .map_err(|e| SyntaxError::Language(e.to_string()))?;
    config.configure(CAPTURE_NAMES);
    Ok(config)
}

/// Compute the byte offset where each line starts. The returned vec has
/// `line_count + 1` entries; the last is `source.len()` (a sentinel).
fn compute_line_starts(source: &[u8]) -> Vec<usize> {
    let mut starts = Vec::with_capacity(source.iter().filter(|b| **b == b'\n').count() + 2);
    starts.push(0);
    for (i, b) in source.iter().enumerate() {
        if *b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts.push(source.len());
    starts
}

/// Place a styled span into the per-line result vector, splitting at
/// newline boundaries and clipping to the requested `[start_line, end_line)`
/// window.
fn distribute_span_across_lines(
    span_start: usize,
    span_end: usize,
    style: Style,
    line_starts: &[usize],
    window_start: u32,
    window_end: u32,
    result: &mut [Vec<StyledSpan>],
) {
    let span_start_line = byte_to_line(line_starts, span_start);
    let span_end_line = byte_to_line(line_starts, span_end.saturating_sub(1));
    for line in span_start_line..=span_end_line {
        if line < window_start || line >= window_end {
            continue;
        }
        let line_start_byte = line_starts[line as usize];
        // The line "content" excludes the trailing newline.
        let line_end_byte = line_starts
            .get((line as usize) + 1)
            .copied()
            .unwrap_or(usize::MAX);
        let content_end = line_end_byte.saturating_sub(1).max(line_start_byte);
        let s = span_start.max(line_start_byte) - line_start_byte;
        let e = span_end.min(content_end + 1) - line_start_byte;
        if e > s {
            result[(line - window_start) as usize].push(StyledSpan {
                start: s,
                end: e,
                style,
            });
        }
    }
}

fn byte_to_line(line_starts: &[usize], byte: usize) -> u32 {
    // Binary search; line_starts is sorted, ends with source.len() sentinel.
    match line_starts.binary_search(&byte) {
        Ok(idx) => idx as u32,
        Err(idx) => idx.saturating_sub(1) as u32,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    fn highlight(lang: Lang, src: &str) -> Vec<Vec<StyledSpan>> {
        let mut s = Syntax::for_language(lang).unwrap().expect("non-plain lang");
        s.parse(src);
        let lines = src.lines().count() as u32;
        s.highlight_lines(0, lines.max(1)).unwrap()
    }

    fn styles(spans: &[StyledSpan]) -> Vec<Style> {
        spans.iter().map(|s| s.style).collect()
    }

    #[test]
    fn plain_lang_yields_no_syntax_instance() {
        assert!(Syntax::for_language(Lang::Plain).unwrap().is_none());
    }

    #[test]
    fn empty_source_returns_empty_per_line_results() {
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse("");
        let lines = s.highlight_lines(0, 1).unwrap();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].is_empty());
    }

    #[test]
    fn rust_keyword_is_highlighted() {
        let lines = highlight(Lang::Rust, "fn main() {}");
        let kinds = styles(&lines[0]);
        assert!(
            kinds.contains(&Style::Keyword),
            "expected Keyword in {kinds:?}"
        );
    }

    #[test]
    fn rust_string_literal_is_highlighted() {
        let lines = highlight(Lang::Rust, r#"const X: &str = "hi";"#);
        let kinds = styles(&lines[0]);
        assert!(
            kinds.contains(&Style::String),
            "expected String in {kinds:?}"
        );
    }

    #[test]
    fn rust_type_is_highlighted() {
        let lines = highlight(Lang::Rust, "let v: Vec<u8> = Vec::new();");
        let kinds = styles(&lines[0]);
        // `Vec` and `u8` should produce at least one Type span.
        assert!(kinds.contains(&Style::Type), "expected Type in {kinds:?}");
    }

    #[test]
    fn rust_line_comment_is_highlighted() {
        let lines = highlight(Lang::Rust, "let x = 1; // comment");
        let kinds = styles(&lines[0]);
        assert!(
            kinds.contains(&Style::LineComment) || kinds.contains(&Style::Comment),
            "expected a comment style in {kinds:?}"
        );
    }

    #[test]
    fn python_keyword_is_highlighted() {
        let lines = highlight(Lang::Python, "def main():\n    pass\n");
        let line0 = &lines[0];
        assert!(
            styles(line0).contains(&Style::Keyword),
            "expected Keyword on line 0, got {:?}",
            styles(line0)
        );
    }

    #[test]
    fn javascript_keyword_is_highlighted() {
        let lines = highlight(Lang::JavaScript, "const x = 1;");
        let kinds = styles(&lines[0]);
        assert!(
            kinds.contains(&Style::Keyword),
            "expected Keyword in {kinds:?}"
        );
    }

    #[test]
    fn line_relative_offsets_stay_within_line_bounds() {
        let src = "fn main() {}\nfn other() {}";
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse(src);
        let lines = s.highlight_lines(0, 2).unwrap();
        for (i, line_spans) in lines.iter().enumerate() {
            let expected_max = src.lines().nth(i).map(|l| l.len()).unwrap_or(0);
            for span in line_spans {
                assert!(
                    span.end <= expected_max,
                    "span {:?} exceeds line length {expected_max} on line {i}",
                    span
                );
                assert!(span.start <= span.end);
            }
        }
    }

    #[test]
    fn requesting_window_returns_only_those_lines() {
        let src = "// a\n// b\n// c\n// d\n";
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse(src);
        let lines = s.highlight_lines(1, 3).unwrap();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn highlight_lines_with_inverted_range_returns_empty() {
        let mut s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        s.parse("fn main() {}");
        let lines = s.highlight_lines(5, 1).unwrap();
        assert!(lines.is_empty());
    }
}
