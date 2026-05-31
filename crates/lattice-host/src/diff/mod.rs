//! Host-side diff subsystem and its presentation companions.
//!
//! Layering: the pure algorithm (`Hunk`, `HunkIndex`,
//! `compute_diff`) lives in the
//! `lattice-diff` crate so it stays reusable by non-editor
//! consumers. Everything in this module is host-attached —
//! it reaches for [`crate::buffer_registry::BufferRegistry`],
//! the [`lattice_runtime::EventBus`], the
//! [`lattice_mode::registry::ModeRegistry`], and tokio. See
//! `docs/dev/architecture/diff-system.md` §3 for the cut and
//! `docs/dev/operations/slice-plans/diff-system.md` for the
//! slice sequencing.
//!
//! Submodules:
//! - [`subsystem`] — `DiffSubsystem` + `DiffSession` lifecycle
//!   (D.2.a–D.2.e). Process-wide registry keyed by
//!   `BufferId`; sessions publish `Arc<HunkIndex>` via
//!   `ArcSwap`; debounce + bus subscription per §3.4.
//! - [`mode`] — `diff-mode` minor mode + the host-side bridge
//!   that toggles it on participating buffers as
//!   `DiffSession`s open and close (D.5.a). See §3.4.7.
//! - [`overlay`] — inline diff overlay's `VirtualRowProvider`
//!   impl (D.3.a). Converts the active session's
//!   `HunkIndex` to deletion-block `VirtualRow`s anchored
//!   above the current-side hunk start. See §3.3.
//! - [`fold`] — `HunkFoldProvider` overlay (D.3.f.1). Emits
//!   one fold per non-empty current-side hunk range; the
//!   standard fold vocabulary (`za` / `zo` / …) covers
//!   hunks identically to syntactic folds. See §6.5 and
//!   `fold-architecture.md`.
//! - [`pane_group`] — `HunkRowMapper` (D.4.b). `RowMapper`
//!   impl that translates rows between the baseline and
//!   current sides of a two-way diff via cumulative-shift
//!   walks over the published `HunkIndex`. See
//!   `pane-groups.md`.
//! - [`filler`] — `FillerRowProvider` (D.4.c). Emits blank
//!   virtual rows on whichever side of a side-by-side diff
//!   is shorter for each hunk so hunks align visually
//!   between the two panes. One provider per side.

pub mod subsystem;
pub mod mode;
pub mod overlay;
pub mod fold;
pub mod pane_group;
pub mod filler;
