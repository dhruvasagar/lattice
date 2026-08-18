//! TC.3b — the sticky-context layer both renderers paint from.
//!
//! [`StickyContext`] is the resolved answer to "which source lines are pinned
//! above this pane's text, and what do they look like". It is built by
//! `cells_worker` in the same pass that builds the pane's
//! [`DisplayMatrix`](crate::display_matrix::DisplayMatrix) and
//! [`IndentGuides`](crate::indent_guides::IndentGuides), from the same snapshot
//! and stamped with the same [`MatrixVersion`] — so the three cannot disagree
//! and this layer needs no staleness axis of its own.
//!
//! ## Why the rows are built here rather than copied by the renderer
//!
//! A context header is, by construction, a line that has scrolled ABOVE the
//! viewport. The `CellMatrix` is chunked above `4 × viewport_height` lines, so
//! a header three thousand lines up is routinely **not resident** in any built
//! chunk. A renderer copying from the published matrix would find nothing and
//! have to fall back to unhighlighted text — a visible colour flicker on
//! scroll, which the UX contract vetoes.
//!
//! The worker holds the rope, the syntax snapshot and the cell builder, so it
//! can build a row for any line whether or not a chunk covers it. That is also
//! what makes the highlighting *identical* to the document's rather than merely
//! similar: there is one derivation, not two.
//!
//! ## Why it is keyed by pane and not by buffer
//!
//! Every other per-pane layer is keyed by `BufferId`, which means two panes
//! showing one buffer share it. `IndentGuides` gets away with that by
//! publishing block extents and letting each renderer pick the active one from
//! its own cursor. Context cannot: the ROWS differ per pane, not just which one
//! is emphasised. One buffer open in two splits with cursors in different
//! scopes must show different context.
//!
//! Design: `docs/dev/architecture/treesitter-context.md`.

use std::sync::Arc;

use lattice_cells::{Cell, MatrixVersion};

/// One pinned context row: the source line it mirrors, and its cells.
///
/// `source_line` is carried so a renderer can show the real line number in the
/// gutter (`context.line-numbers`) without a second lookup, and so a future
/// click-to-jump has the target without re-resolving.
#[derive(Clone, Debug)]
pub struct StickyContextRow {
    pub source_line: u32,
    pub cells: Arc<[Cell]>,
}

/// The pane's pinned context strip, outermost scope first.
///
/// Empty is the overwhelmingly common case — no context plugin loaded, or
/// nothing enclosing the cursor has scrolled away — and it is represented as an
/// empty `rows` rather than an `Option` so both renderers can iterate
/// unconditionally.
#[derive(Clone, Debug, Default)]
pub struct StickyContext {
    /// Outermost first: the row nearest the text is the nearest enclosing
    /// scope, so the strip reads as a continuation of the code.
    pub rows: Vec<StickyContextRow>,
    /// The build's version stamp — the same `MatrixVersion` the pane's
    /// `DisplayMatrix` carries, so a renderer can tell the two came from one
    /// build.
    pub version: MatrixVersion,
    /// Resolved backdrop (`0xRRGGBB`) from the `sticky.context.background`
    /// theme element, or `None` when the theme leaves it unset.
    ///
    /// Resolved once here rather than per row in each renderer: the strip is
    /// host chrome — the host builds, reserves and paints it — so the host
    /// owns its styling, while the plugin owns only the scopes. Without a
    /// backdrop the strip is the same colour as the code beneath it and reads
    /// as ordinary text that will not scroll, which is what prompted this.
    pub bg: Option<u32>,
    /// TC.8: whether the rows show their source line number in the gutter
    /// (`context.line-numbers`).
    ///
    /// Resolved here, alongside the backdrop, for the same reason: the strip
    /// is host chrome, so the host owns its presentation and neither renderer
    /// reads a plugin option. Both peers then reduce to one expression —
    /// `line_numbers.then_some(row.source_line)` — which is hard to get
    /// differently wrong in two places.
    pub line_numbers: bool,
    /// TC.11: resolved `sticky.context.line_number` foreground, or `None` when
    /// the theme leaves it unset (the renderer then uses its default gutter
    /// colour, as it does for document rows).
    pub line_number_fg: Option<u32>,
    /// TC.11: resolved `sticky.context.active` backdrop for the INNERMOST row
    /// — the scope the cursor is actually in.
    ///
    /// Rows are outermost-first, so this applies to the last one. It is the
    /// line the reader is looking for; giving it the same backdrop as its
    /// ancestors makes a deep strip read as an undifferentiated block.
    pub active_bg: Option<u32>,
}

impl StickyContext {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }
}
