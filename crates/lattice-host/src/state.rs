//! Small App-helper state types -- pure data, no renderer
//! coupling.
//!
//! Phase 5.2: extracted from `lattice-ui-tui::app` so the
//! eventual App migration carries fewer in-line type
//! definitions. Each struct here is a piece of state App holds
//! in a field (search line in progress, last search, unnamed
//! register, prev-pane snapshot). Renderer-agnostic by
//! construction.

use lattice_core::{BufferId, BufferKind};
use lattice_grammar::{SearchDirection, YankKind};
use lattice_protocol::position::Position;

/// In-progress `/` or `?` state. The cursor at entry is preserved
/// so Esc can restore it.
#[derive(Debug, Clone)]
pub struct SearchLine {
    pub direction: SearchDirection,
    pub pattern: String,
    pub origin: Position,
}

/// Last completed search -- consulted by `n` and `N`.
#[derive(Debug, Clone)]
pub struct LastSearch {
    pub pattern: String,
    pub direction: SearchDirection,
}

/// The unnamed register's payload. v1 uses a single global slot;
/// the full vim register zoo (`"a-z`, `"+`, `"*`, etc.) lands
/// later.
#[derive(Debug, Clone)]
pub struct UnnamedRegister {
    pub content: String,
    pub kind: YankKind,
}

/// Snapshot of the active pane's state captured just before help
/// took it over. Used by `dismiss_popup` to restore the user to
/// the buffer + cursor + scroll they came from. The same struct
/// serves both display modes (in-pane and popup-overlay).
#[derive(Debug, Clone, Copy)]
pub struct PrevPaneState {
    pub buffer: BufferKind,
    pub buffer_id: BufferId,
    pub cursor: Position,
    pub scroll: u32,
}
