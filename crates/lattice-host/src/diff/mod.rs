//! Host façade over the diff subsystem, which now lives in the
//! `lattice-diff` crate (moved out of `lattice-host` at BC.6/DX.6).
//!
//! The pure algorithm (`Hunk`, `HunkIndex`, `compute_diff`) and — since
//! DX.6 — the host-attached *subsystem* (`DiffSubsystem` + `DiffSession`
//! lifecycle, `diff-mode`, and the overlay/filler/fold/pane-group
//! presentation providers) both live in `lattice-diff`. This module
//! re-exports those crate modules under the historical `crate::diff::*`
//! paths so existing host call sites (dispatch, boot, the TUI + GPUI
//! renderers) are unchanged. DX.7 collapses the boot wiring into
//! `lattice_diff::install(boot)` and begins retiring these shims.
//!
//! The one piece that stays host-side is [`resolver`]: the
//! `BufferRegistry`-backed impls of the subsystem's
//! `BufferTextProvider` / `DocumentBufferResolver` seams (coupling C6;
//! they reference the host `BufferRegistry`, so they can't move). They
//! are re-exported under [`subsystem`] so
//! `crate::diff::subsystem::BufferRegistry{TextProvider,DocumentResolver}`
//! still resolves everywhere.
//!
//! See `docs/dev/architecture/diff-extraction.md` for the cut and
//! `docs/dev/operations/slice-plans/diff-extraction.md` for sequencing.

pub use lattice_diff::{filler, fold, mode, overlay, pane_group};

pub mod resolver;

/// `crate::diff::subsystem` — the moved `lattice_diff::subsystem`
/// surface PLUS the host-owned `BufferRegistry`-backed resolver impls,
/// so every `crate::diff::subsystem::*` call site (including
/// `BufferRegistryTextProvider` / `BufferRegistryDocumentResolver`)
/// resolves unchanged after the DX.6 move.
pub mod subsystem {
    pub use crate::diff::resolver::{BufferRegistryDocumentResolver, BufferRegistryTextProvider};
    pub use lattice_diff::subsystem::*;
}
