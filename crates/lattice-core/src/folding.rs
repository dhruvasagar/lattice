//! Buffer-level folding semantics.
//!
//! [`FoldMethod`] decides which provider feeds the per-buffer fold
//! list. The actual fold list itself lives renderer-side today
//! (`lattice-ui-tui::app::Fold`), because the gutter glyph + summary
//! line are rendering concerns; only the *method* discriminator
//! lives here so any renderer (or LSP integration that wants to
//! announce fold ranges) reads from the same enum.

/// `:set foldmethod=...` (DESIGN.md §15:18, C.2;
/// `docs/help/folding.md`). Decides which provider feeds the
/// per-buffer fold list.
///
/// - `Manual` -- only user `zf` ranges, no auto-recompute.
/// - `Indent` -- universal indent walker.
/// - `Markdown` -- ATX heading nesting (`*.md`).
/// - `Syntax` -- tree-sitter scope queries; cascades to `Markdown`
///   for `.md` buffers and `Indent` otherwise when the tree-sitter
///   provider has nothing to offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FoldMethod {
    #[default]
    Manual,
    Indent,
    Markdown,
    Syntax,
}

impl FoldMethod {
    /// Canonical string label used by `:set foldmethod=...` parsing
    /// + the `:set foldmethod?` echo.
    pub fn label(self) -> &'static str {
        match self {
            FoldMethod::Manual => "manual",
            FoldMethod::Indent => "indent",
            FoldMethod::Markdown => "markdown",
            FoldMethod::Syntax => "syntax",
        }
    }

    /// Parse a `foldmethod=value` payload. Used by the typed
    /// options registry (`OptionType::parse`); also the raw entry
    /// point if anything wants to convert a label without going
    /// through the registry.
    ///
    /// Error message preserves the wording the pre-typed-options
    /// setter produced so the migration is byte-identical from
    /// the user's perspective.
    pub fn parse_label(value: &str) -> Result<Self, String> {
        match value {
            "manual" => Ok(FoldMethod::Manual),
            "indent" => Ok(FoldMethod::Indent),
            "markdown" => Ok(FoldMethod::Markdown),
            "syntax" => Ok(FoldMethod::Syntax),
            other => Err(format!(
                "expected `manual`, `indent`, `markdown`, or `syntax`, got `{other}`"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn label_round_trips_through_parse_label() {
        for fm in [
            FoldMethod::Manual,
            FoldMethod::Indent,
            FoldMethod::Markdown,
            FoldMethod::Syntax,
        ] {
            assert_eq!(FoldMethod::parse_label(fm.label()), Ok(fm));
        }
    }

    #[test]
    fn parse_label_rejects_unknown_with_helpful_message() {
        let err = FoldMethod::parse_label("xyz").unwrap_err();
        assert!(err.contains("expected `manual`"));
        assert!(err.contains("xyz"));
    }

    #[test]
    fn default_is_manual() {
        assert_eq!(FoldMethod::default(), FoldMethod::Manual);
    }
}
