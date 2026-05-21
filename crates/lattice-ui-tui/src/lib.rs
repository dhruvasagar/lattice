// Glob imports (`use super::*;`) and other dead `use` items have caused
// observable rendering glitches in editors with diagnostics overlays --
// the unused-imports warning's underline + the way some editors composite
// diagnostic underlines with tree-sitter spans interacted badly enough
// that lines after the flagged `use` rendered without their syntax colours.
// Promoting `unused_imports` from warning to deny means every test mod's
// `use ...::*;` either pays its way or fails the build, so the class of
// glitch can't slip back in. `cargo check --all-targets` was clean at the
// time this was added.
#![deny(unused_imports)]

//! Terminal renderer for `lattice` (DESIGN.md §5.6.1 `TuiRenderer`).
//!
//! v1 status: a first-class peer of the GPU UI for headless / SSH /
//! low-bandwidth use, with documented limitations bound to terminal
//! capabilities (no variable fonts, monospace cells only, sprites degrade
//! to fallback glyphs).
//!
//! Phase 2 scope: a read-only viewer with cursor motion (hjkl, gg, G),
//! viewport scrolling, line-number gutter, mode line, and a stub
//! syntax highlighter (Rust keywords for `.rs` files). The stub highlighter
//! produces the same `StyledSpan` shape that real tree-sitter integration
//! (Phase 3) will feed; swapping in tree-sitter does not require rewiring
//! the renderer.
//!
//! Phase 3+ replaces the stub with real tree-sitter queries; the modal
//! engine drives input through `CommandInvocation`s instead of the
//! hardcoded `Action` enum used here.

pub mod actor_call_counter;
pub mod app;
// Phase 5.2: HOST modules migrated to `lattice-host`. The
// `pub use` here preserves every existing
// `lattice_ui_tui::<module>::*` import in downstream crates +
// this crate's tests + benches; no call-site changes needed.
pub use lattice_host::{
    actions, buffer_registry, buffers, excommand, file_tree, folds, help, help_topics,
    host_generators, keymap, keymap_registry, keymap_trie, modes, oil, pane, popup,
};
// Re-export the `keymap_entry!` macro (defined with `#[macro_export]`
// in lattice-host) at this crate's root so the catalog files
// (keymap_normal/insert/visual/replace -- still in this crate
// because their `dispatch_*` functions are crossterm-coupled)
// keep invoking it as `keymap_entry!` without a path prefix.
pub use lattice_host::keymap_entry;
pub mod chord;
pub mod icons;
pub mod input;
pub mod keymap_insert;
pub mod keymap_normal;
pub mod keymap_replace;
pub mod keymap_visual;
pub mod pane_render;
pub mod picker_sources;
pub mod render;
pub mod runtime;
pub mod theme;
// Phase 5.5.E.6: the renderer-neutral typed-option declarations
// previously in `tui_options` (ui.dim_inactive, ui.separator,
// ui.separator_color, ui.statusline_*_fg, ui.nerd_fonts) relocated
// to `lattice_host::ui::theme_options` so the host can read the
// values directly when synchronising `host_theme` from config
// (`Editor::sync_host_theme_from_config`). `linkme` self-registers
// the entries regardless of which crate emitted them, so the boot
// path's `init_from_linkme()` picks them up unchanged.

pub use app::{Action, App, EchoLevel, EchoMessage};

// Phase 5.B.2: TuiRenderer marker type implementing
// `lattice_host::Renderer`. The App struct in `app.rs` is now
// generic over `R: Renderer` with `R = TuiRenderer` as the
// default, so every existing `App` reference in this crate +
// downstream consumers keeps resolving via the default. When
// the App struct moves to lattice-host (Phase 5.B.3),
// `App` becomes a type alias `pub type App =
// lattice_host::App<TuiRenderer>;` and this marker stays here
// as the renderer's identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TuiRenderer;

impl lattice_host::Renderer for TuiRenderer {
    type Theme = crate::theme::Theme;
    type PaneRenderRegistry = crate::pane_render::PaneRenderRegistry;
}
pub use buffer_registry::{BufferData, BufferEntry, BufferRegistry, DocumentEntry};
pub use buffers::{BufferFlags, BufferId, BufferKind};
pub use excommand::ExCommandError;
pub use input::{TranslateContext, translate};
pub use lattice_syntax::{Lang, Style, StyledSpan, Syntax};
pub use modes::{
    FileTreeMode, HelpMode, OilMode, major_mode_id_for_buffer_kind, register_buffer_kind_modes,
};
pub use pane::{PaneDirection, PaneId, PaneNode, PaneRect, PaneState, PaneTree, SplitOrientation};
pub use runtime::run;
