//! Buffer-level folding semantics.
//!
//! [`FoldMethod`] decides which provider feeds the per-buffer fold
//! list. [`Fold`] is the per-range entry the list holds. Both are
//! renderer-agnostic: a fold is just `(start_line, end_line,
//! closed, identity)`; the gutter glyph + summary-line text are
//! rendering concerns layered on top.

/// `:set foldmethod=...` (DESIGN.md §15:18, C.2;
/// `docs/user/folding.md`). Decides which provider feeds the
/// per-buffer fold list.
///
/// - `Manual` -- only user `zf` ranges, no auto-recompute.
/// - `Indent` -- universal indent walker.
/// - `Markdown` -- ATX heading nesting (`*.md`).
/// - `Syntax` -- tree-sitter scope queries; cascades to `Markdown`
///   for `.md` buffers and `Indent` otherwise when the tree-sitter
///   provider has nothing to offer.
/// - `Lsp` -- 4.4.f: feeds from `textDocument/foldingRange`.
///   Async: the per-tick pump fires the request when the buffer's
///   document version changes; the response lands in a per-buffer
///   cache and triggers a recompute. Cascades to `Syntax` when no
///   attached server advertises the capability (so `:set
///   foldmethod=lsp` is still useful in mixed-language workspaces).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FoldMethod {
    #[default]
    Manual,
    Indent,
    Markdown,
    Syntax,
    Lsp,
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
            FoldMethod::Lsp => "lsp",
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
            "lsp" => Ok(FoldMethod::Lsp),
            other => Err(format!(
                "expected `manual`, `indent`, `markdown`, `syntax`, or `lsp`, got `{other}`"
            )),
        }
    }
}

/// One contiguous fold range in a document buffer.
///
/// `identity` is the stable handle used to carry closed-state
/// across recomputes. Computed providers (indent / markdown) hash
/// the trimmed start-line text together with the leading-indent
/// depth so that adding or removing lines elsewhere in the buffer
/// doesn't reopen this fold. Manual folds (`zf`) leave it `None`
/// -- their stable identity is the line range itself.
///
/// Phase 5.2: moved from `lattice-ui-tui::app::Fold` to this
/// renderer-agnostic home. Existing `crate::app::Fold` call sites
/// continue to resolve via a `pub use lattice_core::Fold;`
/// re-export in `lattice-ui-tui::app`.
#[derive(Debug, Clone, Copy)]
pub struct Fold {
    pub start_line: u32,
    pub end_line: u32,
    pub closed: bool,
    pub identity: Option<u64>,
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
            FoldMethod::Lsp,
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
