//! Visual-mode selection helpers — renderer-neutral computation
//! of the highlighted range when `editor.modal == Visual(_)`.
//!
//! Phase 5.8.P: hoisted out of `lattice-ui-tui::render::visual_
//! selection_range` so both renderer peers paint the same
//! selection range. The TUI peer keeps its `apply_match_overlay`
//! splice; the GPUI peer paints a flex_row cell with an inverted
//! background. Both consume [`Editor::visual_selection_range`].

use lattice_core::BufferKind;
use lattice_protocol::position::{Position, Range};
use lattice_protocol::selection::VisualMode;

use crate::editor::Editor;
use lattice_grammar::{ModalState, VisualKind};
use lattice_terminal::VisualKind as TermVisualKind;

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

/// Byte range a selection spans, folding the stored `sel.visual`
/// kind. **Visual and Select share this geometry verbatim** —
/// select-mode.md §2 — so both modes resolve their span through this
/// one helper (no drift). Charwise / Blockwise include the HEAD byte
/// (vim semantics; end = `head.byte + 1`); Linewise covers full lines
/// with a `u32::MAX` end byte the caller clamps to the line length.
/// The `(anchor, head)` pair is normalised so `start <= end`.
pub(crate) fn selection_extent(sel: &lattice_protocol::selection::Selection) -> Range {
    let (a, b) = if sel.anchor <= sel.head {
        (sel.anchor, sel.head)
    } else {
        (sel.head, sel.anchor)
    };
    match sel.visual {
        Some(VisualMode::Linewise) => {
            Range::new(Position::new(a.line, 0), Position::new(b.line, u32::MAX))
        }
        Some(VisualMode::Charwise) | Some(VisualMode::Blockwise) | None => {
            Range::new(a, Position::new(b.line, b.byte.saturating_add(1)))
        }
    }
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
        // T-paint-1 (2026-05-28): terminal Visual lives on
        // `t.visual` (grid-space) and its own modal flag stays
        // `Normal`. Derive a doc-space `Range` from it via the
        // SyntheticDoc's `origin_top_line`, so downstream
        // document-grammar consumers (operator range resolution,
        // future copy-to-register paths, etc.) see terminal
        // Visual through the same publish surface as document
        // Visual.
        if matches!(self.active_buffer, BufferKind::Terminal) {
            return self.terminal_visual_selection_range();
        }
        // Select mode (SN.3d) reuses Visual's selection geometry
        // verbatim — same anchor/head, same `selection_extent`. The
        // only difference is the typing semantics, not the painted
        // span, so the render publish surface fires for both.
        if !matches!(self.modal, ModalState::Visual(_) | ModalState::Select(_)) {
            return None;
        }
        let sels = self.document.selections();
        Some(selection_extent(sels.primary()))
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
    /// T-paint-1 (2026-05-28): also returns the block when the
    /// active buffer is a Terminal with `t.visual.kind == Block`,
    /// translating from grid-space to doc-space via
    /// `synthetic.origin_top_line`.
    pub fn visual_block_extents(&self) -> Option<BlockExtents> {
        if matches!(self.active_buffer, BufferKind::Terminal) {
            return self.terminal_visual_block_extents();
        }
        if !matches!(
            self.modal,
            ModalState::Visual(VisualKind::Blockwise) | ModalState::Select(VisualKind::Blockwise)
        ) {
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

    /// T-paint-1 (2026-05-28): doc-space derivation of the
    /// terminal-Visual selection. Reads grid-space `(anchor, head)`
    /// from `t.visual`, subtracts `origin_top_line` to get doc
    /// lines, treats the grid column as a byte column (ASCII
    /// assumption — wide-char handling is the §7 open question),
    /// then folds Charwise / Linewise / Blockwise the same way as
    /// the document path.
    fn terminal_visual_selection_range(&self) -> Option<Range> {
        let buf_id = self.active_pane_buffer_id();
        self.buffers
            .with_terminal(buf_id, |t| {
                let visual = t.visual?;
                let synthetic = t.synthetic.as_ref()?;
                let origin = synthetic.origin_top_line;
                let anchor_line = (visual.anchor_line - origin).max(0) as u32;
                let head_line = (visual.head_line - origin).max(0) as u32;
                let anchor = Position::new(anchor_line, visual.anchor_col as u32);
                let head = Position::new(head_line, visual.head_col as u32);
                let (a, b) = if anchor <= head {
                    (anchor, head)
                } else {
                    (head, anchor)
                };
                Some(match visual.kind {
                    TermVisualKind::Line => {
                        Range::new(Position::new(a.line, 0), Position::new(b.line, u32::MAX))
                    }
                    TermVisualKind::Char => {
                        Range::new(a, Position::new(b.line, b.byte.saturating_add(1)))
                    }
                    TermVisualKind::Block => {
                        Range::new(a, Position::new(b.line, b.byte.saturating_add(1)))
                    }
                })
            })
            .flatten()
    }

    /// T-paint-1 (2026-05-28): doc-space derivation of the
    /// terminal-Visual block extents. Mirrors
    /// [`Self::terminal_visual_selection_range`]; only fires for
    /// `TermVisualKind::Block`.
    fn terminal_visual_block_extents(&self) -> Option<BlockExtents> {
        let buf_id = self.active_pane_buffer_id();
        self.buffers
            .with_terminal(buf_id, |t| {
                let visual = t.visual?;
                if !matches!(visual.kind, TermVisualKind::Block) {
                    return None;
                }
                let synthetic = t.synthetic.as_ref()?;
                let origin = synthetic.origin_top_line;
                let anchor_line = (visual.anchor_line - origin).max(0) as u32;
                let head_line = (visual.head_line - origin).max(0) as u32;
                let anchor_col = visual.anchor_col as u32;
                let head_col = visual.head_col as u32;
                Some(BlockExtents {
                    start_line: anchor_line.min(head_line),
                    end_line: anchor_line.max(head_line),
                    start_col: anchor_col.min(head_col),
                    end_col: anchor_col.max(head_col),
                })
            })
            .flatten()
    }
}
