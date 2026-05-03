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
    // ---- Markup styles (markdown / org / future rich-text modes) ----
    /// `# Heading` -- level 1.
    Heading1,
    /// `## Heading` -- level 2.
    Heading2,
    /// `### Heading` -- level 3.
    Heading3,
    /// `#### Heading` -- level 4.
    Heading4,
    /// `##### Heading` -- level 5.
    Heading5,
    /// `###### Heading` -- level 6.
    Heading6,
    /// `**bold**` / `__bold__` text.
    Bold,
    /// `*italic*` / `_italic_` text.
    Italic,
    /// Link label / link text (`[label]`). Distinct from
    /// [`Style::Url`] so the renderer can underline navigable
    /// labels without underlining the URL itself.
    Link,
    /// Link destination (`(url)`) and autolinks.
    Url,
    /// Inline `` `code` ``, fenced code blocks without an info
    /// string, link titles. Themed similar to comments today;
    /// promoted to its own variant so a future theme can give it
    /// a distinct background.
    MarkupRaw,
    /// List markers (`-`, `*`, `1.`), thematic breaks, blockquote
    /// markers, and other markup punctuation that isn't a
    /// programming-language operator.
    Markup,
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
    // Per-level heading captures. The bundled tree-sitter-md queries
    // emit `text.title` without level info -- our augmented markdown
    // query (in `lang.rs`) adds the level-discriminated variants.
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

/// Map a capture index (the position in `CAPTURE_NAMES`) to a `Style`.
pub(crate) fn capture_index_to_style(idx: usize) -> Style {
    match CAPTURE_NAMES.get(idx).copied() {
        Some(name) => name_to_style(name),
        None => Style::Default,
    }
}

/// Public re-export of the capture-name → Style mapping for the
/// hand-rolled native highlighter (Step 3 of Option B). The
/// streaming highlighter consumes capture indices from the
/// pre-configured CAPTURE_NAMES list; the native path runs the
/// raw query and looks up styles by capture *name*, so it needs
/// direct access to this resolver.
pub fn name_to_style_pub(name: &str) -> Style {
    name_to_style(name)
}

/// Capture-name priority: position in [`CAPTURE_NAMES`] (lower =
/// higher precedence on overlap). Walks the dot-prefix hierarchy
/// the same way [`name_to_style`] does, so a query capture named
/// `keyword.control.return` resolves through `keyword.control` →
/// `keyword`, picking the longest matching prefix's index. Names
/// outside the table return [`u32::MAX`] -- effectively the lowest
/// priority -- so they only "win" overlap when nothing else
/// covers the byte.
///
/// This mirrors what `tree_sitter_highlight::HighlightConfiguration::configure`
/// does internally; the native pipeline uses it to break ties when
/// multiple captures span the same byte range, so e.g. Python's
/// `def f(...)` resolves to the more specific `@function` rather
/// than the broader `@variable` capture.
pub fn capture_priority(name: &str) -> u32 {
    let mut best: Option<usize> = None;
    let mut probe = name;
    loop {
        if let Some(pos) = CAPTURE_NAMES.iter().position(|n| *n == probe) {
            // Take the *first* (most-specific) match the dot-prefix
            // walk encounters; later (broader) prefixes don't beat
            // it.
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
        "punctuation" if name == "punctuation.special" => Style::Markup,
        "punctuation" => Style::Punctuation,
        "attribute" => Style::Attribute,
        "tag" => Style::Type, // HTML/JSX tags display as types for now.
        "label" => Style::Constant,
        // ---- Markup ----
        "text" => match name {
            "text.title.1" => Style::Heading1,
            "text.title.2" => Style::Heading2,
            "text.title.3" => Style::Heading3,
            "text.title.4" => Style::Heading4,
            "text.title.5" => Style::Heading5,
            "text.title.6" => Style::Heading6,
            "text.title" => Style::Heading1, // bundled query doesn't carry level
            "text.strong" => Style::Bold,
            "text.emphasis" => Style::Italic,
            "text.uri" => Style::Url,
            "text.reference" => Style::Link,
            "text.literal" => Style::MarkupRaw,
            _ => Style::Default,
        },
        // `@none` in bundled markdown queries: explicitly suppress
        // highlight on a node so an injection can paint it. Mapped
        // to Default so the node carries no style of its own.
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

    #[test]
    fn markup_heading_levels_are_distinct() {
        assert_eq!(name_to_style("text.title.1"), Style::Heading1);
        assert_eq!(name_to_style("text.title.2"), Style::Heading2);
        assert_eq!(name_to_style("text.title.6"), Style::Heading6);
        // Bundled query falls back to level 1 when no explicit
        // level info is captured.
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
        // `punctuation.special` covers list markers, heading
        // markers, thematic breaks -- all "markup punctuation".
        assert_eq!(name_to_style("punctuation.special"), Style::Markup);
        // Generic punctuation stays Punctuation.
        assert_eq!(name_to_style("punctuation"), Style::Punctuation);
        assert_eq!(name_to_style("punctuation.bracket"), Style::Punctuation);
    }
}
