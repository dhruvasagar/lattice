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
use lattice_grammar::{ModalState, VisualKind};

/// Rectangle defined by a Blockwise Visual selection's
/// `(anchor, head)` positions, normalised so that
/// `start_line ≤ end_line` and `start_col ≤ end_col`. Byte
/// columns, not display columns — renderers fold/inlay-expand at
/// paint time.
///
/// Renderer-neutral; published in
/// [`crate::render_state::ActiveDocumentRenderState`] so TUI and
/// GPUI peers paint the block from the same data instead of each
/// re-deriving it from `selections` + `modal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockExtents {
    pub start_line: u32,
    pub end_line: u32,
    pub start_col: u32,
    pub end_col: u32,
}

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
    /// - **Blockwise**: returns the same linear span as Charwise,
    ///   but renderers ignore this value when the publisher's
    ///   `visual_block_extents` is `Some` (2026-05-27 — the
    ///   per-line column band lives there). Kept around so non-
    ///   renderer consumers (e.g. `Range::Selection` operator
    ///   resolution) still see *some* selection range.
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

    /// Rectangular block of the active Visual selection when
    /// `modal == Visual(Blockwise)`; `None` otherwise. Normalised
    /// so `start_line ≤ end_line` and `start_col ≤ end_col`.
    ///
    /// 2026-05-27: hoisted from
    /// `lattice-ui-tui::render::visual_block_extents` so both
    /// renderer peers paint the same block. Charwise / Linewise
    /// stay on [`visual_selection_range`] — Blockwise needs a
    /// per-line column band that a linear `Range` can't express.
    pub fn visual_block_extents(&self) -> Option<BlockExtents> {
        if !matches!(self.modal, ModalState::Visual(VisualKind::Blockwise)) {
            return None;
        }
        let sels = self.document.selections();
        let sel = sels.primary();
        Some(BlockExtents {
            start_line: sel.anchor.line.min(sel.head.line),
            end_line: sel.anchor.line.max(sel.head.line),
            start_col: sel.anchor.byte.min(sel.head.byte),
            end_col: sel.anchor.byte.max(sel.head.byte),
        })
    }
}
