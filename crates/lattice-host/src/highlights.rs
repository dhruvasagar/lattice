//! Phase 5.8.AF.6 / Slice X2.6: this module is intentionally empty
//! after the legacy `refresh_highlights_window` + `VisibleHighlightsKey`
//! were retired. display-line B4.2: the worker-published visible-spans
//! cell this module used to point at was also deleted (the dead span/
//! row prepaint cache); syntax colour now flows through the cells /
//! `DisplayMatrix` substrate, and overlay backgrounds through
//! `lattice_host::overlay_worker`. The path is renderer-agnostic; no
//! `Editor` method is needed for it.
//!
//! The module is kept (rather than `pub mod highlights;` being removed)
//! so an `Editor::refresh_*` follow-up that wants a public surface
//! has a documented home, and so existing `crate::highlights::*` paths
//! in external code resolve to an empty module rather than a missing
//! one mid-migration.
