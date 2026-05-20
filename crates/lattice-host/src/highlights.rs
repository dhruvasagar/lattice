//! Phase 5.8.AF.6 / Slice X2.6: this module is intentionally empty
//! after the legacy `refresh_highlights_window` + `VisibleHighlightsKey`
//! were retired. Visible-spans live on the worker-published cell at
//! `Editor::syntax_visible_spans_cell` and are produced by
//! `lattice_host::highlights_worker::recompute`. The path through the
//! cell is renderer-agnostic; no `Editor` method is needed for it.
//!
//! The module is kept (rather than `pub mod highlights;` being removed)
//! so an `Editor::refresh_*` follow-up that wants a public surface
//! has a documented home, and so existing `crate::highlights::*` paths
//! in external code resolve to an empty module rather than a missing
//! one mid-migration.
