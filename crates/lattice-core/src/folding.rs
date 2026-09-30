//! Buffer-level folding semantics.
//!
//! M.7 adds `FoldSource` + `FoldOverlayService` — a dep-safe
//! bridge that lets subsystems below `lattice-host` (e.g.
//! `lattice-multibuffer`) register overlay fold providers
//! without depending on `FoldContext` or `FoldProvider`.
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
        /// Feeds from `textDocument/foldingRange`. Async:
        /// the per-tick pump fires the request when the buffer's
        /// document version changes; the response lands in a
        /// per-buffer cache and triggers a recompute. Cascades to
        /// `Syntax` when no attached server advertises the
        /// capability.
        ///
        /// Slice: 4.4.f.
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
    /// First line of the fold, 0-based. Stays visible when the fold is
    /// closed (it carries the summary).
    pub start_line: u32,
    /// Last line of the fold, 0-based and **inclusive**. Providers only emit
    /// folds with `end_line > start_line`.
    pub end_line: u32,
    /// Whether the fold is collapsed, hiding `start_line + 1 ..= end_line`.
    pub closed: bool,
    /// Stable identity used to carry `closed` across recomputes; `None` for
    /// manual folds, whose identity is the line range. See the type docs.
    pub identity: Option<u64>,
}

/// Distinguishes mutually-exclusive primary fold sources
/// (one runs at a time, picked by `:set foldmethod=`) from
/// additive overlay sources (always compose). See
/// `docs/dev/architecture/fold-architecture.md` §2.
///
/// Slice: D.3.f.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    /// Selected by [`FoldMethod`]; exactly one primary provider feeds a
    /// buffer at a time.
    Primary,
    /// Always composed on top of the primary folds (e.g. multibuffer
    /// excerpt folds), regardless of `foldmethod`.
    Overlay,
}

/// Stable identifier for a registered fold provider.
/// Two distinct providers must produce distinct ids; a single
/// provider produces the same id across recomputes. Used by the
/// registry for lookup and by diagnostics that need to attribute
/// a fold back to its source.
///
/// Slice: D.3.f.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProviderId(pub u64);

/// Data-only fold source for subsystems that live below
/// `lattice-host` (e.g. `ExcerptFoldProvider` in
/// `lattice-multibuffer`). Implementors cannot depend on
/// `FoldContext` or `FoldProvider` (both defined in `lattice-host`).
/// `FoldSourceAdapter` in `lattice-host::fold_provider` wraps any
/// `FoldSource` as a `FoldProvider` by delegating `compute()` to
/// `compute_folds` and ignoring `FoldContext`.
///
/// Slice: M.7.
pub trait FoldSource: Send + Sync {
    /// This source's stable id: the same on every call, and distinct from
    /// every other registered source's (see [`ProviderId`]).
    fn id(&self) -> ProviderId;
    /// Produce the current fold ranges. Called by the host on recompute, so
    /// it should be cheap (read already-computed state; no I/O).
    fn compute_folds(&self) -> Vec<Fold>;
}

/// Service for registering / deregistering overlay fold sources.
/// Implemented by `FoldOverlayServiceImpl` in `lattice-host` (which
/// wraps `Arc<Mutex<FoldRegistry>>`). Registered in the
/// `ServiceRegistry` at boot so `MultibufferMode::on_activate` can
/// call `add_source` without depending on `lattice-host`.
///
/// `buffer_id` scopes the overlay: `FoldSourceAdapter` in
/// `lattice-host` only calls `compute_folds` when
/// `FoldContext::buffer_id` matches, so providers from multiple
/// simultaneous multibuffers don't bleed into each other's views.
///
/// Slice: M.7.
pub trait FoldOverlayService: Send + Sync {
    /// Register `source` as an overlay provider scoped to `buffer_id` and
    /// return the id to pass to [`Self::remove_source`] later.
    fn add_source(
        &self,
        source: std::sync::Arc<dyn FoldSource>,
        buffer_id: crate::BufferId,
    ) -> ProviderId;
    /// Deregister a source added by [`Self::add_source`]. Removing an
    /// unknown id is a no-op.
    fn remove_source(&self, id: ProviderId);
}

/// Cheap-clone handle for the fold-overlay service. Mode guards hold
/// one Arc so they can call `remove_source` on Drop.
pub type FoldOverlayServiceHandle = std::sync::Arc<dyn FoldOverlayService>;

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
