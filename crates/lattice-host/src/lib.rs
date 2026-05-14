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

pub mod actions;
pub mod buffers;
pub mod excommand;
pub mod file_tree;
pub mod help;
pub mod help_topics;
pub mod oil;
pub mod popup;
