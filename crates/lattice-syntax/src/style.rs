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

/// TK.1: resolve a capture name that names a **registered theme
/// element** rather than a builtin category.
///
/// `Style::Element` exists precisely for vocabularies that are open by
/// nature — its own doc says a plugin "can register a theme element by
/// name but can **never** add a variant to a Rust enum". It was
/// reachable from decorations and listing icons and not from a
/// tree-sitter query, which is the gap this closes. Org's TODO
/// keywords are the first caller (`org.todo.WAITING`); nothing here
/// knows that.
///
/// Builtin names win. A capture called `keyword` stays
/// [`Style::Keyword`] even if some plugin registers an element under
/// that name, because the closed categories are the editor's own
/// vocabulary and a plugin must not be able to redefine what `keyword`
/// means for every language at once.
///
/// `theme` is `None` wherever no registry is in hand — the native
/// `LangRegistry::standard()` path, and every test that does not care.
/// Then this is exactly the pre-TK.1 mapping.
pub fn name_to_style_with_theme(
    name: &str,
    theme: Option<&dyn lattice_theme::ThemeRegistry>,
) -> Style {
    let builtin = name_to_style(name);
    if builtin != Style::Default {
        return builtin;
    }
    // Only an unrecognised name reaches the registry, so this costs a
    // lookup per unknown capture at query-COMPILE time — never per
    // span, and never per frame.
    let Some(theme) = theme else {
        return Style::Default;
    };
    theme
        .id(&lattice_theme::ElementName::from(name.to_string()))
        .map(Style::Element)
        .unwrap_or(Style::Default)
}

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
    best.map(|p| p as u32).unwrap_or(*ELEMENT_CAPTURE_PRIORITY)
}

/// TK.1: the overlap precedence an element-backed capture gets.
///
/// **This is the half that would otherwise fail silently.** The old
/// fallback was `u32::MAX` — the *lowest* precedence — and org's query
/// captures a whole headline `(item)` as `@text.title.N`. An
/// `@org.todo.WAITING` capture over the keyword would therefore lose
/// its overlap with the title and paint nothing, with every individual
/// piece correct in isolation and the feature simply absent.
///
/// The value is `keyword`'s, and that is chosen rather than invented:
/// a TODO keyword is captured as `@keyword` today, so taking the same
/// priority keeps overlap behaviour byte-identical and changes only the
/// colour. A change that is behaviour-preserving in every dimension
/// except the one being changed is the one that cannot surprise.
fn element_capture_priority() -> u32 {
    // Resolved from the table rather than written as a literal, so
    // reordering `CAPTURE_NAMES` cannot silently retune this.
    CAPTURE_NAMES
        .iter()
        .position(|n| *n == "keyword")
        .map(|p| p as u32)
        .unwrap_or(u32::MAX)
}

/// Cached form of [`element_capture_priority`] for the hot walk.
static ELEMENT_CAPTURE_PRIORITY: std::sync::LazyLock<u32> =
    std::sync::LazyLock::new(element_capture_priority);

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

    // ---- TK.1: a capture name may name a theme element ----

    use lattice_theme::{
        ColorRef, ElementName, ElementOwner, InMemoryThemeRegistry, Palette, StyleSpec,
        ThemeRegistry,
    };

    fn registry_with(names: &[&str]) -> InMemoryThemeRegistry {
        let r = InMemoryThemeRegistry::new(Palette::default());
        for n in names {
            r.register(
                ElementName::from((*n).to_string()),
                ElementOwner::Plugin("org".into()),
                StyleSpec {
                    fg: Some(ColorRef::Palette("red".into())),
                    ..Default::default()
                },
                "test element",
            );
        }
        r
    }

    #[test]
    fn tk1_a_registered_element_name_resolves_to_that_element() {
        let r = registry_with(&["org.todo.WAITING"]);
        let id = r
            .id(&ElementName::from("org.todo.WAITING".to_string()))
            .expect("registered");
        assert_eq!(
            name_to_style_with_theme("org.todo.WAITING", Some(&r)),
            Style::Element(id)
        );
    }

    #[test]
    fn tk1_an_unregistered_dotted_name_is_still_default() {
        // No accidental matching: only a name the theme actually knows
        // becomes an element.
        let r = registry_with(&["org.todo.WAITING"]);
        assert_eq!(
            name_to_style_with_theme("org.todo.NOPE", Some(&r)),
            Style::Default
        );
    }

    /// A plugin must not be able to redefine what `keyword` means for
    /// every language at once. The closed categories are the editor's
    /// own vocabulary and they win.
    #[test]
    fn tk1_a_builtin_capture_name_is_never_shadowed_by_an_element() {
        let r = registry_with(&["keyword", "comment", "text.title.1"]);
        assert_eq!(
            name_to_style_with_theme("keyword", Some(&r)),
            Style::Keyword
        );
        assert_eq!(
            name_to_style_with_theme("comment", Some(&r)),
            Style::Comment
        );
        assert_eq!(
            name_to_style_with_theme("text.title.1", Some(&r)),
            Style::Heading1
        );
    }

    #[test]
    fn tk1_no_registry_is_exactly_the_pre_tk1_mapping() {
        for n in [
            "keyword",
            "comment",
            "text.title.1",
            "org.todo.WAITING",
            "utterly.unknown",
        ] {
            assert_eq!(
                name_to_style_with_theme(n, None),
                name_to_style(n),
                "{n} must be unchanged without a registry"
            );
        }
    }

    /// **The half that would otherwise fail silently.**
    ///
    /// Org's query captures a whole headline `(item)` as
    /// `@text.title.N`, and an element capture over the keyword overlaps
    /// it. Under the old `u32::MAX` fallback the element would LOSE that
    /// overlap and paint nothing — every piece correct in isolation, the
    /// feature simply absent. An element capture takes `keyword`'s
    /// priority, so it wins exactly as `@keyword` does today.
    #[test]
    fn tk1_an_element_capture_outranks_a_title_capture() {
        let element = capture_priority("org.todo.WAITING");
        let title = capture_priority("text.title.1");
        assert!(
            element < title,
            "element ({element}) must outrank text.title.1 ({title}), \
             or the keyword never paints"
        );
    }

    #[test]
    fn tk1_an_element_capture_has_exactly_keywords_priority() {
        // Chosen rather than invented: TODO is `@keyword` today, so this
        // keeps overlap behaviour byte-identical and changes only colour.
        assert_eq!(
            capture_priority("org.todo.WAITING"),
            capture_priority("keyword")
        );
    }

    #[test]
    fn tk1_builtin_capture_priorities_are_unchanged() {
        // The fallback moved from u32::MAX; nothing that already had a
        // position may have shifted.
        assert!(capture_priority("comment.line") < capture_priority("comment"));
        assert!(capture_priority("keyword.control") < capture_priority("keyword"));
        assert!(capture_priority("text.title.1") < capture_priority("none"));
    }
}
