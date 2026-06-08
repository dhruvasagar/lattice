//! Style category emitted by the highlighter, plus the capture-name table.
//!
//! `Style` and `StyledSpan` now live in `lattice-cells` (to break the
//! `lattice-syntax → lattice-mode → lattice-runtime` dep cycle).
//! Re-exported here so all existing `lattice_syntax::Style` / `StyledSpan`
//! call-sites see no change.

pub use lattice_cells::style::{Style, StyledSpan};

/// The capture-name table consumed by `HighlightConfiguration::configure`.
/// Order matters only as far as it's consistent with the index-to-style
/// mapping below.
///
/// The `text.*` family is the nvim-treesitter convention used by
/// `tree-sitter-md`'s bundled queries (and most other markup
/// grammars). Extending this list with `markup.*` entries lets newer
/// queries fall through cleanly when we adopt them.
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
    "punctuation.special",
    "punctuation.delimiter",
    "punctuation",
    "attribute",
    "tag",
    "label",
    // ---- Markup captures (markdown block + inline grammars) ----
    "text.title.1",
    "text.title.2",
    "text.title.3",
    "text.title.4",
    "text.title.5",
    "text.title.6",
    "text.title",
    "text.strong",
    "text.emphasis",
    "text.uri",
    "text.reference",
    "text.literal",
    "none",
];

/// Public re-export of the capture-name → Style mapping.
pub fn name_to_style_pub(name: &str) -> Style {
    name_to_style(name)
}

/// Capture-name priority: position in `CAPTURE_NAMES` (lower = higher
/// precedence on overlap). Walks the dot-prefix hierarchy.
pub fn capture_priority(name: &str) -> u32 {
    let mut best: Option<usize> = None;
    let mut probe = name;
    loop {
        if let Some(pos) = CAPTURE_NAMES.iter().position(|n| *n == probe) {
            best = Some(pos);
            break;
        }
        match probe.rfind('.') {
            Some(i) => probe = &probe[..i],
            None => break,
        }
    }
    best.map(|p| p as u32).unwrap_or(u32::MAX)
}

pub(crate) fn name_to_style(name: &str) -> Style {
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
        "punctuation" if name == "punctuation.special" => Style::Markup,
        "punctuation" => Style::Punctuation,
        "attribute" => Style::Attribute,
        "tag" => Style::Type,
        "label" => Style::Constant,
        "text" => match name {
            "text.title.1" => Style::Heading1,
            "text.title.2" => Style::Heading2,
            "text.title.3" => Style::Heading3,
            "text.title.4" => Style::Heading4,
            "text.title.5" => Style::Heading5,
            "text.title.6" => Style::Heading6,
            "text.title" => Style::Heading1,
            "text.strong" => Style::Bold,
            "text.emphasis" => Style::Italic,
            "text.uri" => Style::Url,
            "text.reference" => Style::Link,
            "text.literal" => Style::MarkupRaw,
            _ => Style::Default,
        },
        "none" => Style::Default,
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
    fn capture_names_table_is_non_empty_and_has_no_duplicates() {
        assert!(!CAPTURE_NAMES.is_empty());
        let mut seen = std::collections::HashSet::new();
        for &n in CAPTURE_NAMES {
            assert!(seen.insert(n), "duplicate capture name: {n}");
        }
    }

    #[test]
    fn markup_heading_levels_are_distinct() {
        assert_eq!(name_to_style("text.title.1"), Style::Heading1);
        assert_eq!(name_to_style("text.title.2"), Style::Heading2);
        assert_eq!(name_to_style("text.title.6"), Style::Heading6);
        assert_eq!(name_to_style("text.title"), Style::Heading1);
    }

    #[test]
    fn markup_emphasis_styles_resolve() {
        assert_eq!(name_to_style("text.strong"), Style::Bold);
        assert_eq!(name_to_style("text.emphasis"), Style::Italic);
        assert_eq!(name_to_style("text.uri"), Style::Url);
        assert_eq!(name_to_style("text.reference"), Style::Link);
        assert_eq!(name_to_style("text.literal"), Style::MarkupRaw);
    }

    #[test]
    fn punctuation_special_maps_to_markup() {
        assert_eq!(name_to_style("punctuation.special"), Style::Markup);
        assert_eq!(name_to_style("punctuation"), Style::Punctuation);
        assert_eq!(name_to_style("punctuation.bracket"), Style::Punctuation);
    }
}
