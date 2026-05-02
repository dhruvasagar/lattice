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

pub mod app;
pub mod buffers;
pub mod chord;
pub mod excommand;
pub mod help;
pub mod input;
pub mod keymap;
pub mod pane;
pub mod render;
pub mod runtime;

pub use app::{Action, App, EchoLevel, EchoMessage, Pending};
pub use buffers::{BufferId, BufferKind};
pub use excommand::ExCommandError;
pub use input::{TranslateContext, translate};
pub use lattice_syntax::{Lang, Style, StyledSpan, Syntax};
pub use pane::{PaneDirection, PaneId, PaneNode, PaneRect, PaneState, PaneTree, SplitOrientation};
pub use runtime::run;
