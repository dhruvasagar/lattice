//! `lattice-host` -- the renderer-agnostic editor substrate.
//!
//! This crate is the home of the editor's logic: `App` state,
//! dispatch, picker sources, mode lifecycle, LSP coordination,
//! options cascade, keymap registry, buffer registry, folds,
//! search, completion engine, file ops, oil + file-tree state.
//!
//! It depends on `lattice-core`, `lattice-grammar`,
//! `lattice-config`, `lattice-mode`, `lattice-picker`,
//! `lattice-completion`, `lattice-lsp`, `lattice-runtime`,
//! `lattice-syntax`, and the other domain crates. It does **not**
//! depend on any renderer crate -- `lattice-ui-tui` and the
//! future `lattice-ui-gpui` both depend on this, never the other
//! way around.
//!
//! The public entry point (Phase 5.6+) will be:
//!
//! ```ignore
//! pub fn run(app: App, renderer: impl lattice_render::Renderer) -> Result<()>
//! ```
//!
//! ## Phase 5 status
//!
//! Phase 5.1 (this slice): the crate is empty. The seam exists;
//! the workspace builds with the new crate declared; downstream
//! consumers can depend on it. Phase 5.2 begins the actual
//! migration of renderer-agnostic modules out of
//! `lattice-ui-tui`.
//!
//! See `docs/dev/architecture/phase-5-extraction.md` for the
//! slice plan and module classification.

pub mod action;
pub mod actions;
pub mod buffer_registry;
pub mod buffers;
pub mod chord;
pub mod cursor_shape;
pub mod dispatch;
pub mod editor;
pub mod visual;
// Phase 5.7.B.1: Editor::boot extraction from
// `lattice-ui-tui::app::boot::App::new`.
mod editor_boot;
pub mod excommand;
pub mod file_tree;
pub mod folds;
// D.3.f.0 (2026-05-29): FoldProvider trait + registry. See
// `docs/dev/architecture/fold-architecture.md`. Substrate-only
// slice; the five existing fold methods become Primary
// providers wrapping today's `compute_*_folds` helpers. The
// first overlay consumer (`HunkFoldProvider`) lands in D.3.f.1.
pub mod fold_provider;
// D.3.f.1 (2026-05-29): HunkFoldProvider — overlay that
// emits one Fold per non-empty current-side hunk range in
// the active diff session. Registered as an overlay at
// editor boot; emits empty when no diff session is active.
// See `docs/dev/architecture/fold-architecture.md` and
// `docs/dev/architecture/diff-system.md` §6.5.
pub mod diff_fold;
// D.4.a (2026-05-29): pane-group substrate. `PaneGroup` is
// a set of `(pane, buffer)` pairs that scroll together
// under a pluggable `RowMapper`. The trait + registry
// land in this slice; the `HunkRowMapper` consumer is
// D.4.b. See `docs/dev/architecture/pane-groups.md`.
pub mod pane_group;
// D.4.b (2026-05-29): `HunkRowMapper` — `RowMapper` impl
// that translates rows between the baseline and current
// sides of a two-way diff via cumulative-shift walks over
// the published `HunkIndex`. Pure function of the session
// + row + member-index pair. Composed with D.4.a, consumed
// by D.4.d (`:diffsplit` / `:diffthis`).
pub mod diff_pane_group;
// D.4.c (2026-05-29): `FillerRowProvider` — emits blank
// virtual rows on whichever side of a side-by-side diff is
// shorter for each hunk, so hunks align visually between
// the two panes. One provider per side. Pure-sync collect
// (no off-thread render needed — work is O(hunks)).
// Consumed by D.4.d (`:diffsplit` / `:diffthis`).
pub mod diff_filler;
pub mod help;
pub mod help_topics;
pub mod highlights;
pub mod highlights_worker;
// S2.2 (2026-05-26): cell-grid renderer's cell-builder worker.
// Sibling of `highlights_worker`; consumes `RenderState.cells`
// inputs, builds a whole-doc `CellMatrix`, publishes via
// `Editor::cells_matrix_cell`. See
// `docs/dev/architecture/cell-grid-renderer.md`.
pub mod cells_worker;
// D.2.a (2026-05-28): diff-subsystem registry skeleton.
// `DiffSubsystem` keys `DiffSession` entries by `BufferId`;
// sessions publish `Arc<HunkIndex>` via `ArcSwap`. Compute +
// recompute scheduling land D.2.b. See
// `docs/dev/architecture/diff-system.md` §6.
pub mod diff_subsystem;
// D.0a.1 (2026-05-29): virtual-rows worker. Sibling of
// `cells_worker`; owns the `VirtualRowMatrix` rebuild path,
// polls registered `VirtualRowProvider`s on wake, publishes
// via `Editor::virtual_rows_matrix_cell`. See
// `docs/dev/architecture/virtual-rows.md`.
pub mod virtual_rows_worker;
// D.3.a (2026-05-29): inline diff overlay's
// `VirtualRowProvider` impl. Converts the active
// `DiffSession`'s published `HunkIndex` to deletion-block
// `VirtualRow`s anchored above the current-side hunk start.
// See `docs/dev/architecture/diff-system.md` §3.3.
pub mod diff_overlay;
pub mod host_generators;
pub mod input;
pub mod keymap;
pub mod keymap_insert;
pub mod keymap_normal;
pub mod keymap_registry;
pub mod keymap_replace;
// Terminal-mode T2.a (2026-05-25): keystroke → ANSI byte
// encoder consumed by the Terminal-Insert translate branch.
pub mod keymap_terminal;
pub mod keymap_trie;
pub mod keymap_visual;
pub mod lsp_helpers;
pub mod lsp_watcher;
pub mod modes;
pub mod oil;
pub mod pane;
pub mod pane_highlights;
pub mod pane_render;
pub mod popup;
// Phase 5.8.AF.5 / Slice 3b: per-buffer cache primitive for the
// LSP feature drains migrating off the renderer thread. See
// `per_buffer_cache` module docs.
pub mod per_buffer_cache;
// Phase 5.8.AF.5 / Slice 3c: editor-actor scaffolding. Dormant
// in 3c.0; wired by later sub-slices once render-side reads
// migrate to RenderState.
pub mod editor_actor;
// Phase 5.8.AF.5 / Slice 3a: renderer's wait-free read contract
// with the host. See `render_state` module docs.
pub mod render_state;
pub mod renderer;
pub mod state;
// Phase 5.7.B.9: synthetic-buffer + messages helpers migrate
// from `impl App` (TUI) to `impl Editor` (host) so both
// renderer peers seed `*lsp*` + `*messages*` eagerly.
pub mod messages;
pub mod synthetic_buffers;
pub mod ui;

// Perf plan B.4: tiny newtype wrapper that bumps a `u64` version
// on every `DerefMut` access. Drives the identity-preserving Arc
// publish in `render_state` so unchanged sub-states reuse their
// prior Arc across publishes.
pub mod versioned;

// Re-export the host-side Renderer trait at the crate root for
// the conventional `lattice_host::Renderer` path renderer
// crates use when implementing.
pub use renderer::{MinimalRenderer, Renderer};
