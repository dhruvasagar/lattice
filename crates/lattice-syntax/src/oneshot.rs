//! D.3.b.2 (2026-05-29): one-shot synchronous highlight.
//!
//! Wraps `Syntax::for_language_with_registry` + `parse` +
//! `snapshot().highlight_lines` into a single call so callers
//! that don't own a long-lived `SyntaxHandle` (notably the
//! diff overlay's deletion-block renderer, which parses
//! an ephemeral baseline rope) can get styled spans without
//! standing up an actor or wiring incremental state.
//!
//! No caching. Every call re-parses the source from scratch.
//! The caller is expected to cache the returned spans
//! alongside whatever revision it tracks for the source.
//!
//! See `docs/dev/architecture/diff-system.md` §6.6.

use std::sync::Arc;

use crate::{Lang, LangRegistry, StyledSpan, Syntax};

/// Run a fresh tree-sitter parse over `source` and return
/// per-line styled spans for lines in `start_line..end_line`.
///
/// Returns `None` when:
/// - `lang` is `Lang::Plain` (no grammar to apply),
/// - the registry has no registered grammar for `lang`,
/// - the parse or highlight call fails for any reason
///   (registry lookup error, malformed source, etc.) — fail
///   silently so the caller's "no syntax" fallback path
///   (monochrome rendering) takes over without a panic.
///
/// `start_line` and `end_line` are inclusive-exclusive in
/// line indices, matching
/// [`crate::SyntaxSnapshot::highlight_lines`]. Returns one
/// `Vec<StyledSpan>` per line in the range. Spans within
/// each inner vec are line-relative byte offsets.
///
/// **Cost model.** Tree-sitter parse is O(source_len);
/// highlight query walk is O(matches). For the diff
/// deletion-block case where a typical baseline is hundreds
/// to thousands of lines, both are well under 10 ms on
/// modern hardware — within
/// `DiffOverlayRefreshTask`'s off-thread budget. Caller is
/// expected to invoke at hunk-publish frequency (debounced
/// at the session level), not per-frame.
pub fn oneshot_highlight_lines(
    lang: Lang,
    registry: Arc<LangRegistry>,
    source: &str,
    start_line: u32,
    end_line: u32,
) -> Option<Vec<Vec<StyledSpan>>> {
    let mut syntax = Syntax::for_language_with_registry(lang, registry).ok().flatten()?;
    syntax.parse(source);
    syntax.snapshot().highlight_lines(start_line, end_line).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_none_for_plain_language() {
        let registry = LangRegistry::standard().expect("standard registry");
        let out = oneshot_highlight_lines(Lang::Plain, registry, "anything", 0, 1);
        assert!(out.is_none());
    }

    #[test]
    fn returns_some_for_rust_source_with_lines() {
        let registry = LangRegistry::standard().expect("standard registry");
        let source = "fn main() {\n    let x = 1;\n}\n";
        let out = oneshot_highlight_lines(Lang::Rust, registry, source, 0, 3);
        let lines = out.expect("rust grammar registered");
        // Three source lines requested.
        assert_eq!(lines.len(), 3);
        // First line "fn main() {" should produce at least
        // one styled span (the `fn` keyword).
        assert!(!lines[0].is_empty(), "expected styled spans on line 0");
    }

    #[test]
    fn empty_range_returns_empty_vec() {
        let registry = LangRegistry::standard().expect("standard registry");
        let out =
            oneshot_highlight_lines(Lang::Rust, registry, "fn main() {}", 0, 0).expect("ok");
        assert!(out.is_empty());
    }

    #[test]
    fn handles_empty_source_gracefully() {
        // Parsing an empty string shouldn't panic; the
        // highlight call against an empty rope returns
        // either an empty vec or one empty line — either is
        // acceptable for the caller's fallback path. Just
        // assert the call doesn't return None for a known
        // language.
        let registry = LangRegistry::standard().expect("standard registry");
        let out = oneshot_highlight_lines(Lang::Rust, registry, "", 0, 1);
        assert!(out.is_some());
    }
}
