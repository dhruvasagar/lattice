//! The host-side `Renderer` trait.
//!
//! Phase 5.B.1 introduces the abstraction that lets `App` be
//! generic over its renderer. Each frontend crate
//! (`lattice-ui-tui`, future `lattice-ui-gpui`) defines a
//! zero-sized marker type and implements [`Renderer`] for it,
//! specifying the renderer-native types that fill the two
//! associated slots `App` carries.
//!
//! ## Why only two associated types?
//!
//! The Phase 5.B.0 field audit
//! ([`docs/dev/architecture/phase-5b-app-fields.md`]) classified
//! every field on the current [`crate::App`] (when it lives here
//! after 5.B.3) against its type's home crate. Of ~200 fields,
//! **two** carry renderer-specific types:
//!
//! - the cached ratatui-typed `Theme` adapter the TUI's render
//!   loop reads on the hot path, and
//! - the per-mode pane-render dispatch registry whose function
//!   pointers take a renderer-native frame type.
//!
//! Every other field is pure data or pulls from already-host-side
//! crates. That makes the trait's surface deliberately small.
//! Frame-level types (`Frame`, `InputEvent`, `LayoutConstraints`,
//! …) live on Phase 5.6's separate `lattice-render::Renderer`
//! trait — they have a different cardinality (one App-Renderer
//! pairing ↔ many frame renders) and a different home.
//!
//! ## Anchor docs
//!
//! - [`docs/dev/architecture/phase-5-extraction.md`] — the
//!   overall Phase 5 plan.
//! - [`docs/dev/architecture/phase-5b-app-fields.md`] — the
//!   field-by-field audit that confirmed this surface.

/// The host-side renderer abstraction.
///
/// Implementors are zero-sized marker types in renderer crates:
///
/// ```ignore
/// // in lattice-ui-tui:
/// pub struct TuiRenderer;
/// impl lattice_host::Renderer for TuiRenderer {
///     type Theme = TuiTheme;
///     type PaneRenderRegistry = TuiPaneRenderRegistry;
/// }
///
/// // in lattice-ui-gpui (Phase 5.8+):
/// pub struct GpuiRenderer;
/// impl lattice_host::Renderer for GpuiRenderer {
///     type Theme = GpuiTheme;
///     type PaneRenderRegistry = GpuiPaneRenderRegistry;
/// }
/// ```
///
/// The trait is intentionally bare — every method goes through
/// the associated types' native APIs rather than a virtual
/// dispatch surface. Renderer crates own their hot-path reads.
///
/// **Bounds.** `'static + Send + Sync` so `App<R>` can cross
/// threads (the LSP supervisor, syntax worker, mode dispatcher
/// all spawn work on shared runtimes that hold references to
/// `&App<R>`). The renderer marker type itself is ZST so the
/// bounds are trivially satisfied.
pub trait Renderer: 'static + Send + Sync {
    /// Renderer-specific cached theme view. The host owns the
    /// canonical neutral [`crate::ui::theme::Theme`]; on every
    /// `:set ui.*` cascade the renderer rebuilds this cache
    /// from the neutral theme via its own
    /// `From<&host::ui::theme::Theme>` adapter. Reads on the
    /// per-frame paint path go straight to this typed field
    /// without indirection.
    type Theme: 'static + Send + Sync;

    /// Renderer-specific per-mode pane-render dispatch table.
    /// The TUI's stores function pointers shaped
    /// `fn(&mut ratatui::Frame, Rect, &App<TuiRenderer>, ...)`;
    /// GPUI's stores its analogous shape for the GPUI paint
    /// context. The host's mode-activation path doesn't care
    /// about the inside — it just owns the field on App and
    /// hands `&self.pane_render_registry` back to the renderer
    /// at paint time.
    type PaneRenderRegistry: 'static + Send + Sync;
}

/// Headless renderer marker for host-side tests and any
/// renderer-neutral integration test that needs a concrete `R`
/// without pulling in a real renderer's types.
///
/// Both associated types are `()`. App fields parametrized
/// over `R::Theme` / `R::PaneRenderRegistry` collapse to unit
/// values, costing one byte at most (and zero with niche
/// optimization). Tests that exercise renderer-agnostic
/// behaviour use `App<MinimalRenderer>`; tests that exercise
/// TUI-specific behaviour use `App<TuiRenderer>` (defined in
/// `lattice-ui-tui`).
pub struct MinimalRenderer;

impl Renderer for MinimalRenderer {
    type Theme = ();
    type PaneRenderRegistry = ();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time check: `MinimalRenderer` satisfies the
    /// trait's `'static + Send + Sync` bounds without explicit
    /// impls (the ZST trivially does).
    fn assert_renderer<R: Renderer>() {}

    #[test]
    fn minimal_renderer_implements_renderer() {
        assert_renderer::<MinimalRenderer>();
    }

    #[test]
    fn minimal_renderer_associated_types_are_unit() {
        // Exists primarily to document the design intent; the
        // type system would catch a divergence at compile time.
        let _: <MinimalRenderer as Renderer>::Theme = ();
        let _: <MinimalRenderer as Renderer>::PaneRenderRegistry = ();
    }
}
