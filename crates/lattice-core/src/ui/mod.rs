//! UI primitives shared across renderers.
//!
//! Renderer-agnostic UI concepts -- popup placement, pane / window
//! geometry -- live here so any renderer (TUI, future GPU, future
//! web) can consume them without depending on a specific UI crate.
//! Concrete rendering (ratatui widgets, GPUI views, etc.) stays in
//! the renderer crate; only the *data shapes* and *geometry math*
//! live in lattice-core.

pub mod icons;
pub mod pane;
pub mod popup;
