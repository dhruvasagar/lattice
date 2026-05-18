//! Visual-mode selection helpers — renderer-neutral computation
//! of the highlighted range when `editor.modal == Visual(_)`.
//!
//! Phase 5.8.P: hoisted out of `lattice-ui-tui::render::visual_
//! selection_range` so both renderer peers paint the same
//! selection range. The TUI peer keeps its `apply_match_overlay`
//! splice; the GPUI peer paints a flex_row cell with an inverted
//! background. Both consume [`Editor::visual_selection_range`].

use lattice_protocol::position::{Position, Range};
use lattice_protocol::selection::VisualMode;

use crate::editor::Editor;
use lattice_grammar::ModalState;

impl Editor {
    /// Half-open byte range covered by the active Visual selection,
    /// or `None` outside Visual mode. Spans the primary selection's
    /// anchor → head pair, normalised so `start <= end`.
    ///
    /// - **Linewise**: covers full lines from `start.line` to
    ///   `end.line`. The end byte is `u32::MAX` — callers should
    ///   clamp it to the actual line length when painting
    ///   (`match_overlay_range` in TUI; the GPUI peer's per-line
    ///   clamp likewise).
    /// - **Charwise** (and `None` — uninitialised selections that
    ///   default to charwise): includes the HEAD byte (vim
    ///   semantics). End byte = `head.byte + 1`.
    /// - **Blockwise**: v1 stub — renders as charwise. A future
    ///   slice extends this to per-line column ranges.
    ///
    /// Renderer-neutral; the returned `Range` is the renderer-
    /// agnostic [`lattice_protocol::position::Range`].
    pub fn visual_selection_range(&self) -> Option<Range> {
        if !matches!(self.modal, ModalState::Visual(_)) {
            return None;
        }
        let sels = self.document.selections();
        let sel = sels.primary();
        let (a, b) = if sel.anchor <= sel.head {
            (sel.anchor, sel.head)
        } else {
            (sel.head, sel.anchor)
        };
        match sel.visual {
            Some(VisualMode::Linewise) => Some(Range::new(
                Position::new(a.line, 0),
                Position::new(b.line, u32::MAX),
            )),
            Some(VisualMode::Charwise) | None => Some(Range::new(
                a,
                Position::new(b.line, b.byte.saturating_add(1)),
            )),
            Some(VisualMode::Blockwise) => Some(Range::new(
                a,
                Position::new(b.line, b.byte.saturating_add(1)),
            )),
        }
    }
}
