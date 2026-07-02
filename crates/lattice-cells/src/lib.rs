//! Cell-grid renderer substrate.
//!
//! Pure data types: [`Cell`], [`CellRow`], [`CellChunk`],
//! [`CellMatrix`], [`MatrixVersion`]. No rendering, no I/O, no
//! rope dependencies. Both renderer crates (`lattice-ui-tui`,
//! `lattice-ui-gpui`) and the host substrate (`lattice-host`)
//! depend on this.
//!
//! Construction logic — converting `(rope, syntax spans, inlay
//! hints, folds, theme)` into `CellRow`s and chunks — lands in
//! S2 as a tokio worker in `lattice-host`. This crate's
//! responsibility ends at the type contracts.
//!
//! Design anchor:
//! [`docs/dev/architecture/cell-grid-renderer.md`](../../../docs/dev/architecture/cell-grid-renderer.md).
//!
//! ## Module layout
//!
//! - [`cell`] — 16-byte [`Cell`] + the [`cell::flags`] bit table.
//! - [`row`] — [`CellRow`] + the inlay byte↔column remap helper.
//! - [`chunk`] — [`CellChunk`], the cache + rebuild unit.
//! - [`matrix`] — [`CellMatrix`] + slicing iterators.
//! - [`version`] — [`MatrixVersion`] vector that drives rebuild
//!   decisions in the cell-builder worker.
//!
//! ## Slicing
//!
//! S1 (this crate) — types, slicing API, unit tests.
//! S2 — cell-builder worker + RenderState integration.
//! S3 — TUI cutover.
//! S4 — GPU glyph atlas + `paint_cells`.
//! S5 — bench + chunk-size + atlas tuning.
//! S6 — cleanup (strip probes, retire shape_line on code path).

pub mod cell;
pub mod chunk;
// Style types + ExcerptHighlighter trait. Defined here (not in
// lattice-syntax) to break the lattice-syntax → lattice-mode →
// lattice-runtime cycle that would block lattice-runtime's Document
// trait from referencing them.
pub mod style;
// S2.4.b (2026-05-26): substrate-level edit delta consumed by the
// cell-builder's incremental rebuild path.
pub mod edit_delta;
pub mod matrix;
pub mod row;
pub mod version;
// D.0a (2026-05-28): virtual-row primitive for inline diff
// deletion blocks, multibuffer excerpt headers, and future
// inlay / code-lens consumers. See
// `docs/dev/architecture/virtual-rows.md`.
pub mod virtual_rows;
// Generic sticky headerline — the one mechanism for a buffer to pin a status
// row above line 0. Tutor, multibuffer search, LSP status, VCS branch all use
// this. See `docs/dev/architecture/headerline.md`.
pub mod headerline;

pub use cell::{flags as cell_flags, Cell};
pub use chunk::CellChunk;
pub use edit_delta::EditDelta;
pub use matrix::{
	wrap_segments, CellMatrix, CellSlice, CellSliceIter, DisplayRowEntry, DisplaySlice,
	DisplaySliceIter, CHUNK_SIZE_WHOLE_DOC,
};
pub use row::{CellRow, InlayOffset};
pub use style::{ExcerptHighlight, ExcerptHighlighter, Style, StyledSpan};
pub use version::MatrixVersion;
pub use virtual_rows::{
	AnchorPosition, ProviderId, VirtualRow, VirtualRowKind, VirtualRowMatrix,
	VirtualRowProvider, VirtualRowVersion,
};
pub use headerline::{Headerline, HeaderlineProvider, HeaderlineRow, SimpleHeaderlineHandle};
