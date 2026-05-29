//! Buffer-level folding semantics.
//!
//! [`FoldMethod`] decides which provider feeds the per-buffer fold
//! list. [`Fold`] is the per-range entry the list holds. Both are
//! renderer-agnostic: a fold is just `(start_line, end_line,
//! closed, identity)`; the gutter glyph + summary-line text are
//! rendering concerns layered on top.

crate::labeled_enum! {
    /// `:set foldmethod=...` (DESIGN.md §15:18, C.2;
    /// `docs/user/folding.md`). Decides which provider feeds the
    /// per-buffer fold list.
    ///
    /// Each variant's marginalia doc (right of `=>`) is what
    /// appears in `:set foldmethod=<Tab>`. Variant-level `///`
    /// docs are for API/rustdoc consumers. Slice
    /// `3c.unify.option-docs-builtin` migrated this enum to
    /// `labeled_enum!` — adding a new fold method is now one
    /// line; the `label` / `parse_label` / `doc` / `all`
    /// accessors are derived automatically.
    ///
    /// D.3.f.0 added `#[derive(Hash)]` so the `FoldRegistry`
    /// can key its primary-provider map on `FoldMethod`.
    #[derive(Hash)]
    pub enum FoldMethod {
        /// Only user `zf` ranges, no auto-recompute.
        #[default]
        Manual = "manual"
            => "User-defined folds only (zf to create, zd to delete)",
        /// Universal indent walker.
        Indent = "indent"
            => "Fold by indent level",
        /// ATX heading nesting (`*.md`).
        Markdown = "markdown"
            => "Fold by markdown headings (#, ##, ###, …)",
        /// Tree-sitter scope queries; cascades to `Markdown` for
        /// `.md` buffers and `Indent` otherwise when the tree-
        /// sitter provider has nothing to offer.
        Syntax = "syntax"
            => "Folds from the tree-sitter syntax tree",
        /// 4.4.f: feeds from `textDocument/foldingRange`. Async:
        /// the per-tick pump fires the request when the buffer's
        /// document version changes; the response lands in a
        /// per-buffer cache and triggers a recompute. Cascades to
        /// `Syntax` when no attached server advertises the
        /// capability.
        Lsp = "lsp"
            => "Folds from LSP `textDocument/foldingRange`",
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

/// D.3.f.0: distinguishes mutually-exclusive primary fold sources
/// (one runs at a time, picked by `:set foldmethod=`) from
/// additive overlay sources (always compose). See
/// `docs/dev/architecture/fold-architecture.md` §2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    Primary,
    Overlay,
}

/// D.3.f.0: stable identifier for a registered fold provider.
/// Two distinct providers must produce distinct ids; a single
/// provider produces the same id across recomputes. Used by the
/// registry for lookup and by diagnostics that need to attribute
/// a fold back to its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProviderId(pub u64);

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
