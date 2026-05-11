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

pub mod actions;
pub mod app;
pub mod buffer_registry;
pub mod buffers;
pub mod chord;
pub mod excommand;
pub mod file_tree;
pub mod folds;
pub mod help;
pub mod help_topics;
pub mod host_generators;
pub mod input;
pub mod keymap;
pub mod keymap_insert;
pub mod keymap_normal;
pub mod keymap_registry;
pub mod keymap_replace;
pub mod keymap_trie;
pub mod keymap_visual;
pub mod modes;
pub mod pane;
pub mod pane_render;
pub mod popup;
pub mod render;
pub mod runtime;
pub mod icons;
pub mod oil;
pub mod theme;
pub mod tui_options;

pub use app::{Action, App, EchoLevel, EchoMessage};
pub use buffer_registry::{BufferData, BufferEntry, BufferRegistry, DocumentEntry};
pub use buffers::{BufferFlags, BufferId, BufferKind};
pub use modes::{
    FileTreeMode, HelpMode, OilMode, major_mode_id_for_buffer_kind, register_buffer_kind_modes,
};
pub use excommand::ExCommandError;
pub use input::{TranslateContext, translate};
pub use lattice_syntax::{Lang, Style, StyledSpan, Syntax};
pub use pane::{PaneDirection, PaneId, PaneNode, PaneRect, PaneState, PaneTree, SplitOrientation};
pub use runtime::run;
