//! Mode-keyed pane render dispatch (M.4 follow-up).
//!
//! Replaces the helper-side `match buffer.kind` in the renderer's
//! `draw_pane_content` /
//! [`crate::app::App::pane_status_label`] with a [`ModeId`]-keyed
//! lookup. Each major / minor mode that owns its own render flow
//! registers a [`PaneRenderProvider`] at boot; the renderer walks
//! the active buffer's minors (most-specific first) then its major
//! to find the provider, falling back to the document path when no
//! provider matches. Plugins (post-1.0) register additional modes
//! through the same registry.
//!
//! Lives in `lattice-ui-tui` rather than `lattice-mode` because the
//! function signatures take ratatui types (`Frame`, `Rect`) -- the
//! mode crate stays renderer-agnostic. A future renderer (GPUI,
//! web) gets its own registry with its own native signatures; the
//! [`ModeId`] keys are shared so the registration story is uniform
//! across renderers.

use std::collections::HashMap;

use lattice_core::BufferId;
use lattice_host::pane_render::ProviderLookup;
use lattice_mode::ModeId;
use lattice_runtime::DocumentSnapshot;
use ratatui::Frame;
use ratatui::layout::Rect;

use crate::app::App;
use crate::pane::PaneState;

/// Renderer for one pane. Receives the same arguments as the
/// dispatcher in `crate::render::draw_pane_content`: the frame,
/// content rect, app state, the active document snapshot (used by
/// the document fallback path), the pane state, an `is_active`
/// flag, and the pane index (for inactive-pane stash lookups).
pub type PaneRenderFn = fn(&mut Frame, Rect, &App, &DocumentSnapshot, &PaneState, bool, usize);

/// Status-line label for one pane. Returned to the renderer's
/// `draw_pane_status_line` so it can paint the bottom row.
pub type PaneStatusFn = fn(&App, &PaneState) -> String;

/// One mode's pane-render contribution. Kept paired so a mode owns
/// both its content rendering and its status label -- the two are
/// almost always defined together (a help pane wants `[help]` in
/// the status; a file-tree pane wants `[tree] /path/to/root`).
pub struct PaneRenderProvider {
    /// How to paint the pane's content, or `None` when the mode's
    /// buffers paint through the **shared document compose path**.
    ///
    /// DL.4: `None` is the goal state, not a special case. A mode that
    /// supplies its own painter re-implements scroll windowing, the
    /// cursor row and per-row styling — which is how one arithmetic
    /// slip came to live in four places (CV.5). The file tree became
    /// `None` when it converged to a `DocumentEntry`; oil follows in
    /// DL.5.
    pub render: Option<PaneRenderFn>,
    pub status: PaneStatusFn,
}

/// Boot-time registry. Owned by [`App`]; keyed by [`ModeId`].
#[derive(Default)]
pub struct PaneRenderRegistry {
    map: HashMap<ModeId, PaneRenderProvider>,
}

impl PaneRenderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a provider for `mode`. Replaces any previous
    /// registration for the same id.
    pub fn register(&mut self, mode: ModeId, provider: PaneRenderProvider) {
        self.map.insert(mode, provider);
    }

    /// Register a mode that owns only its status label and paints
    /// through the shared document path (DL.4).
    pub fn register_status_only(&mut self, mode: ModeId, status: PaneStatusFn) {
        self.map.insert(
            mode,
            PaneRenderProvider {
                render: None,
                status,
            },
        );
    }

    /// Look up a provider by mode id. `None` if no mode has
    /// registered for it (callers fall back to their default
    /// path).
    pub fn get(&self, mode: ModeId) -> Option<&PaneRenderProvider> {
        self.map.get(&mode)
    }
}

/// Phase 5.6: the host's [`lattice_host::pane_render::resolve_pane_render_mode`]
/// walks active modes; this impl is the renderer-side probe it
/// queries to learn which `ModeId` has a TUI-shaped provider
/// registered. Each renderer ships its own typed registry and its
/// own `ProviderLookup` impl over it; the walk algorithm itself
/// stays host-side.
impl ProviderLookup for PaneRenderRegistry {
    fn has_provider(&self, mode: ModeId) -> bool {
        self.map.contains_key(&mode)
    }
}

impl App {
    /// Resolve the [`PaneRenderProvider`] for `buffer_id`. Walks
    /// active minors in reverse activation order (most-recently
    /// activated wins -- the same priority the option resolver
    /// uses) before falling back to the major. Returns `None`
    /// when nothing is registered, in which case the renderer
    /// uses its default document path.
    ///
    /// Phase 5.6: the walk lives host-side in
    /// [`lattice_host::pane_render::resolve_pane_render_mode`]; this
    /// method is a thin renderer-side adapter that hands the host the
    /// active editor + registry and looks up the matched provider
    /// from the resulting `ModeId`. The two-step shape (resolve id,
    /// then `registry.get`) keeps the trait minimal: the host never
    /// sees the renderer-typed `PaneRenderProvider`.
    pub fn pane_render_provider(&self, buffer_id: BufferId) -> Option<&PaneRenderProvider> {
        // Slice 3c.final.X.cleanup: route through published
        // `ModesRenderState` (B.11) instead of `read_editor`.
        // Called per pane per frame from the host's split layout
        // — same cost class as the compose-loop reads B-extension
        // already lifted. Walk minors (innermost first) before
        // falling back to major, matching `resolve_pane_render_mode`.
        let modes = self.modes();
        let active = modes.map.get(&buffer_id)?;
        for minor_id in active.minors().iter().rev() {
            if self.pane_render_registry.has_provider(*minor_id) {
                return self.pane_render_registry.get(*minor_id);
            }
        }
        active
            .major()
            .filter(|id| self.pane_render_registry.has_provider(*id))
            .and_then(|id| self.pane_render_registry.get(id))
    }
}

#[cfg(test)]
mod tests {
    use crate::app::test_helpers::app_with;
    use lattice_mode::Mode;

    #[test]
    fn document_buffer_has_no_provider_falls_through_to_default() {
        // A plain document buffer's major is `text-mode` (or a
        // language major). Neither is registered with a pane-render
        // provider; the renderer must fall through to its default
        // document path.
        let a = app_with("hello", 10);
        let active_id = a.editor.pane_tree.active().buffer_id;
        assert!(a.pane_render_provider(active_id).is_none());
    }

    #[test]
    fn help_minor_provider_wins_over_markdown_major() {
        // In-pane help buffers run markdown-mode (major) +
        // help-mode (minor). The dispatch walks minors first
        // then the major, so the help-mode provider wins -- the
        // buffer renders as help, not as a plain markdown
        // document.
        let mut a = app_with("hi", 5);
        let help = crate::help::HelpContent::from_lines("test", vec!["line one".to_string()]);
        let help_id = a.open_help_in_pane(help);
        let modes = a
            .editor
            .active_modes
            .get(&help_id)
            .expect("in-pane help has modes");
        assert_eq!(modes.major(), Some(lattice_syntax::MarkdownMode.id()));
        assert!(modes.has_minor(lattice_mode::modes::HelpMode.id()));
        assert!(a.pane_render_provider(help_id).is_some());
    }
}
