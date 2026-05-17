//! Mode-keyed pane render resolution (renderer-agnostic algorithm).
//!
//! Phase 5.6: the *algorithm* that walks a buffer's active modes
//! (minors most-recently-activated first, then the major) lives
//! host-side here; the per-renderer *storage* of provider fn-pointers
//! stays renderer-specific (TUI uses `ratatui::Frame`-typed fns; a
//! future GPUI renderer will use its own native shapes). Renderers
//! implement [`ProviderLookup`] over their own registry and call
//! [`resolve_pane_render_mode`] to find the `ModeId` whose provider
//! should drive a pane.
//!
//! See `docs/dev/architecture/phase-5-extraction.md` "Hard Case §2"
//! → "Post-Option-E revision (slice 5.6)". The trait-object plan
//! from the original draft is superseded — composition made the
//! registry naturally renderer-specific, so the only shared piece
//! is the mode-walking lookup.

use lattice_core::BufferId;
use lattice_mode::ModeId;

use crate::editor::Editor;

/// Renderer-supplied "is this mode registered?" probe. Each renderer
/// implements this trait on its own typed pane-render registry; the
/// host walks modes via [`resolve_pane_render_mode`] and asks the
/// probe whether the candidate `ModeId` has a provider.
///
/// Kept deliberately minimal (one boolean method) so the host's
/// algorithm carries no renderer-specific shape and the renderer's
/// registry stays free to hold native-typed fn-pointers without
/// trait-object overhead on the paint hot path.
pub trait ProviderLookup {
    /// True when `mode` has a registered pane-render provider.
    fn has_provider(&self, mode: ModeId) -> bool;
}

/// Resolve the `ModeId` whose pane-render provider should drive the
/// pane displaying `buffer_id`. Walks active minors in reverse
/// activation order (most-recently activated wins — same priority
/// the option resolver uses) before falling back to the major.
/// Returns `None` when no active mode has a provider registered, in
/// which case the renderer uses its default document path.
///
/// The probe is `&L` rather than `&impl Fn(ModeId) -> bool` so the
/// trait acts as a documented seam between host and renderer; the
/// monomorphisation cost is the same.
pub fn resolve_pane_render_mode<L: ProviderLookup>(
    editor: &Editor,
    buffer_id: BufferId,
    lookup: &L,
) -> Option<ModeId> {
    let modes = editor.active_modes.get(&buffer_id)?;
    for &minor_id in modes.minors().iter().rev() {
        if lookup.has_provider(minor_id) {
            return Some(minor_id);
        }
    }
    modes.major().filter(|id| lookup.has_provider(*id))
}
