//! Renderer-neutral UI types: theme, style, color.
//!
//! Every type here is pure data with no renderer-specific
//! dependency. Renderer crates (`lattice-ui-tui`, future
//! `lattice-ui-gpui`) ship adapters that convert these into
//! their native style types (ratatui `Style` / `Color`, GPUI
//! `Hsla`, etc.). The host owns the canonical theme; each
//! renderer maintains a cached adapted view for hot-path reads.
//!
//! Phase 5.3 introduces these types as the substrate for the
//! eventual `App.theme: host::ui::Theme` migration. The current
//! Phase 5.3 slice adds them ALONGSIDE the existing TUI-typed
//! `App.theme` (which keeps its current shape) -- `App.host_theme`
//! is the canonical neutral state, and `sync_theme_from_config`
//! writes both. Later cleanup will remove the duplication once
//! GPUI lands and render code reads from a renderer-cached view.

pub mod theme;
pub mod theme_options;
