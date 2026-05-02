//! Style category emitted by the highlighter, plus the capture-name table.
//!
//! `tree-sitter-highlight` emits captures by index into a pre-configured
//! list. We provide the list once, in priority order (most specific first),
//! and map each list index to a `Style`. Capture names that aren't in the
//! list are ignored by the highlighter -- the runtime walks the dot-prefix
//! hierarchy itself, so adding `keyword.control` here will match
//! `keyword.control.return` query captures without us doing anything else.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Default,
    Comment,
    LineComment,
    String,
    Keyword,
    Type,
    Number,
    Function,
    Constant,
    Variable,
    Operator,
    Punctuation,
    Attribute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StyledSpan {
    /// Byte offset within the line where the span starts.
    pub start: usize,
    /// Byte offset within the line where the span ends (exclusive).
    pub end: usize,
    pub style: Style,
}

/// The capture-name table consumed by `HighlightConfiguration::configure`.
/// Order matters only as far as it's consistent with the index-to-style
/// mapping below.
pub(crate) const CAPTURE_NAMES: &[&str] = &[
    "comment.line",
    "comment",
    "string.escape",
    "string",
    "keyword.control",
    "keyword.function",
    "keyword.operator",
    "keyword",
    "type.builtin",
    "type",
    "number",
    "constant.builtin",
    "constant",
    "function.method",
    "function.macro",
    "function",
    "variable.parameter",
    "variable.builtin",
    "variable",
    "operator",
    "punctuation.bracket",
    "punctuation.delimiter",
    "punctuation",
    "attribute",
    "tag",
    "label",
];

/// Map a capture index (the position in `CAPTURE_NAMES`) to a `Style`.
pub(crate) fn capture_index_to_style(idx: usize) -> Style {
    match CAPTURE_NAMES.get(idx).copied() {
        Some(name) => name_to_style(name),
        None => Style::Default,
    }
}

fn name_to_style(name: &str) -> Style {
    let head = name.split('.').next().unwrap_or(name);
    match head {
        "comment" if name.starts_with("comment.line") => Style::LineComment,
        "comment" => Style::Comment,
        "string" => Style::String,
        "keyword" => Style::Keyword,
        "type" => Style::Type,
        "number" => Style::Number,
        "function" => Style::Function,
        "constant" => Style::Constant,
        "variable" => Style::Variable,
        "operator" => Style::Operator,
        "punctuation" => Style::Punctuation,
        "attribute" => Style::Attribute,
        "tag" => Style::Type, // HTML/JSX tags display as types for now.
        "label" => Style::Constant,
        _ => Style::Default,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn known_names_map_to_distinct_styles() {
        assert_eq!(name_to_style("keyword"), Style::Keyword);
        assert_eq!(name_to_style("string"), Style::String);
        assert_eq!(name_to_style("type"), Style::Type);
        assert_eq!(name_to_style("number"), Style::Number);
    }

    #[test]
    fn dot_prefixed_names_use_the_head() {
        assert_eq!(name_to_style("keyword.control"), Style::Keyword);
        assert_eq!(name_to_style("type.builtin"), Style::Type);
        assert_eq!(name_to_style("string.escape"), Style::String);
    }

    #[test]
    fn comment_line_specialization() {
        assert_eq!(name_to_style("comment.line"), Style::LineComment);
        assert_eq!(name_to_style("comment"), Style::Comment);
    }

    #[test]
    fn unknown_names_fall_back_to_default() {
        assert_eq!(name_to_style("unknown"), Style::Default);
        assert_eq!(name_to_style(""), Style::Default);
    }

    #[test]
    fn capture_index_rounds_to_style() {
        // The first entry in CAPTURE_NAMES is "comment.line".
        assert_eq!(capture_index_to_style(0), Style::LineComment);
        // Out of range -> Default.
        assert_eq!(capture_index_to_style(10_000), Style::Default);
    }

    #[test]
    fn capture_names_table_is_non_empty_and_has_no_duplicates() {
        assert!(!CAPTURE_NAMES.is_empty());
        let mut seen = std::collections::HashSet::new();
        for &n in CAPTURE_NAMES {
            assert!(seen.insert(n), "duplicate capture name: {n}");
        }
    }
}
