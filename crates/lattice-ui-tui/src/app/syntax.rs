//! Syntax / tree-sitter App surface -- the (re)parse
//! request trigger that keeps `self.editor.syntax`'s worker in
//! lockstep with the document's `text_version`.
//!
//! Methods that live here:
//! - `maybe_reparse_syntax` (DESIGN.md §B.2: hands edit
//!   deltas to the syntax worker on text_version bump,
//!   then triggers `recompute_folds` so foldmethod=indent
//!   stays in sync). Idempotent and cheap when nothing
//!   changed.
//!
//! Stays in app.rs (deferred):
//! - `refresh_highlights` (per-frame render-side cache;
//!   moves with a render-coupled slice -- it touches
//!   `VisibleHighlightsKey` + the visible-line extent
//!   walker that's also a render concern).
//! - `refresh_pane_highlights` and inactive-pane parse
//!   coordination (render-coupled).
//!
//! What does NOT live here: tree-sitter parser cache
//! (`crate::syntax`), grammar registration -- those are
//! content-shape concerns owned by `lattice-syntax`.

use super::App;

impl App {
    /// 5.5.D: reparse-trigger logic moved to
    /// [`lattice_host::editor::Editor::maybe_reparse_syntax`].
    /// Renderer call sites keep this thin wrapper until 5.5.G
    /// collapses App's match entirely.
    pub(super) fn maybe_reparse_syntax(&mut self) {
        self.mutate_editor_with(move |e| e.maybe_reparse_syntax());
    }
}
