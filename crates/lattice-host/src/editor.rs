//! The renderer-agnostic editor state.
//!
//! Phase 5.B.3 introduces [`Editor`] as the destination for
//! the per-cluster field migration from
//! `lattice-ui-tui::App`. See
//! [`docs/dev/architecture/phase-5b-app-design.md`] for the
//! Option-D → Option-E pivot that this struct realises:
//!
//! - The host owns the editor's state and logic in `Editor`.
//! - Each renderer crate composes `Editor` into its own
//!   concrete `App` wrapper alongside its renderer-specific
//!   caches (`theme`, `pane_render_registry`, ...).
//!
//! This file lands empty. Subsequent slices (5.B.4 onwards)
//! relocate field clusters one at a time from `App` into
//! `Editor`, moving the methods that touch only those fields
//! into `impl Editor` here. Each per-cluster commit ships
//! green: methods that still live in `impl App` access
//! migrated fields via `self.editor.foo`; methods that have
//! moved access them via `self.foo` (now an inherent method
//! on `Editor`).
//!
//! The empty-now/grows-later shape is intentional: it lets
//! the wrapper field `editor: Editor` get added to `App`
//! before any field actually moves, giving every subsequent
//! migration a target that already exists in the type
//! system.

/// Renderer-agnostic editor state.
///
/// The renderer-agnostic half of every editor App. Each
/// renderer's `App` struct composes one of these alongside
/// its renderer-specific caches. Host-level code (mode
/// lifecycle, dispatch, picker sources, LSP supervisor, ...)
/// takes `&mut Editor` directly; renderer-side code takes
/// `&mut App` and reaches the editor via `app.editor`.
///
/// **Field set at 5.B.3:** empty. The struct exists as a
/// destination so subsequent per-cluster migrations have a
/// type to move fields into. As clusters land, this struct
/// grows; in parallel, `App`'s direct field set shrinks.
///
/// **End state (after the per-cluster migration finishes):**
/// every renderer-agnostic field on App lives here, every
/// renderer-agnostic method on App lives in this crate's
/// `impl Editor` blocks. App becomes a thin wrapper holding
/// `editor: Editor` plus renderer-specific caches only.
#[derive(Debug, Default)]
pub struct Editor {}
