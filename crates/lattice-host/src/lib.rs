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
pub(crate) mod events;
pub mod activator;
// Autoread (AR.*): external-change detection + refresh for file-backed
// buffers. AR.0 lands the on-disk fingerprint seam only; the `notify`
// watcher lands in AR.2. See `docs/dev/architecture/autoread.md`.
pub mod autoread;
// Boot-composition BC.1: the `BootContext` skeleton — the host's
// generic-primitive surface (the typed bundle a subsystem's future
// `install(boot)` receives). Additive this slice; `editor_boot` migrates onto
// it at BC.3. See `docs/dev/architecture/boot-composition.md`.
pub mod boot_context;
pub mod buffer_registry;
pub mod buffers;
pub mod chord;
// CB.4: native `arboard` clipboard backend shared by both renderer peers.
// Only compiled with the `system-clipboard` feature (pulls X11/Wayland
// libs); host's default build stays headless-friendly.
#[cfg(feature = "system-clipboard")]
pub mod clipboard;
pub mod cursor_shape;
pub mod dashboard;
pub mod diagnostics_query;
pub mod dispatch;
pub mod editor;
// BC.5: `emacs-keys-mode` moved to `lattice-mode` (a builtin). The host keeps
// only its keymap-layer push (editor_boot keymap block + dispatch `:set`).
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
// providers wrapping today's `compute_*_folds` helpers.
// Overlays are mode-owned (DX.3-C7): `diff-mode` registers a
// `HunkFoldSource` via the `FoldOverlayService`.
pub mod fold_provider;
// Diff subsystem (D.1–D.5+) lives under `crate::diff::*` as
// a submodule group; see `diff/mod.rs` for the per-submodule
// breakdown and `docs/dev/architecture/diff-system.md` for
// the design fragment. The pure algorithm layer (`Hunk`,
// `HunkIndex`, `compute_diff`) stays
// in the `lattice-diff` crate per design §3 so it remains
// reusable by non-editor consumers.
pub mod diff;
// M.2.b.1 (2026-05-31): all multibuffer concerns extracted into
// the dedicated `lattice-multibuffer` crate. Host has no
// multibuffer-specific code beyond a one-line `register(...)`
// call at boot once M.2.b.2 lands the major mode.
// D.4.a (2026-05-29): pane-group substrate. `PaneGroup` is
// a set of `(pane, buffer)` pairs that scroll together
// under a pluggable `RowMapper`. The trait + registry
// land in this slice; the `HunkRowMapper` consumer is
// D.4.b. See `docs/dev/architecture/pane-groups.md`.
pub mod help;
pub mod help_topics;
pub mod highlights;
// display-line B4.2 (gut + rename): formerly `highlights_worker`.
// The dead span/row prepaint cache was deleted; only the live
// static-overlay bucketing remains, so the module was renamed to
// reflect that it no longer produces highlights.
pub mod overlay_worker;
pub mod pane_group;
// S2.2 (2026-05-26): cell-grid renderer's cell-builder worker.
// Sibling of `overlay_worker`; consumes `RenderState.cells`
// inputs, builds a whole-doc `CellMatrix`, publishes via
// `Editor::cells_matrix_cell`. See
// `docs/dev/architecture/cell-grid-renderer.md`.
pub mod cells_worker;
// B1 (2026-06-04): per-line display cache — the substrate that
// retires the per-character `CellMatrix`. See
// docs/dev/architecture/display-line.md.
pub mod display_matrix;
// D.0a.1 (2026-05-29): virtual-rows worker. Sibling of
// `cells_worker`; owns the `VirtualRowMatrix` rebuild path,
// polls registered `VirtualRowProvider`s on wake, publishes
// via `Editor::virtual_rows_matrix_cell`. See
// `docs/dev/architecture/virtual-rows.md`.
pub mod grep_highlight;
pub mod host_generators;
pub mod input;
pub mod keymap;
pub mod virtual_rows_worker;
// K.3.2 (2026-06-02): emacs-style <C-h> help-prefix bindings
// (Normal-mode, Builtin layer). See module doc for the design
// rationale incl. Option 2 (no bare <C-h> leaf).
pub mod keymap_help;
pub mod keymap_insert;
// K.2.4 (2026-06-01): host translation pass that walks
// `ModeRegistry`, calls `Mode::keymap()` on each mode, and
// pushes each non-empty contribution as a `MinorMode(mode_id)`
// layer on the host's `KeymapHandle`. The K.2 substrate
// (K.2.1 chord primitives, K.2.2 BindingMode, K.2.3 real
// Keymap) is in place; this is the plumbing.
pub mod keymap_mode_contributions;
pub mod keymap_normal;
pub mod keymap_registry;
pub mod keymap_replace;
// Terminal-mode T2.a (2026-05-25): keystroke → ANSI byte
// encoder consumed by the Terminal-Insert translate branch.
pub mod keymap_select;
pub mod keymap_terminal;
pub mod keymap_trie;
pub mod keymap_visual;
// K.2.5 (2026-06-02): `multibuffer_keymap` retired. Multibuffer +
// project-search keymap bindings and ex-commands moved to
// `lattice-multibuffer` (`MultibufferMode::keymap()`,
// `ProjectSearchMultibufferMode::keymap()`,
// `lattice_multibuffer::register_multibuffer_ex_commands`,
// `lattice_multibuffer::providers::search::register_search_ex_command`).
// Boot wiring in `editor_boot.rs` calls into the new home directly;
// the K.2.4 translation pass picks up the `Mode::keymap()`
// contributions and inserts them into the matcher trie.
pub mod lsp_helpers;
pub mod lsp_watcher;
pub mod mode_action_handlers;
pub mod modeline;
pub mod modes;
pub mod oil;
pub mod pane;
pub mod pane_render;
pub mod popup;
pub mod preview;
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
pub mod tutor;
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

// K.2.4.A.0.1 (2026-06-02): re-export the `keymap_entry!` macro
// at the host crate root so the `lattice_host::keymap_entry!`
// path used by `lattice-ui-tui/src/lib.rs:51` keeps resolving
// after the macro's `#[macro_export]` origin moved to
// `lattice-mode`. The macro's canonical home is
// `lattice_mode::keymap_entry!`; this re-export retires once
// downstream call sites flip to the canonical path.
pub use lattice_mode::keymap_entry;
