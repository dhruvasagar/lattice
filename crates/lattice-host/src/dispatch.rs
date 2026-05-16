//! Renderer-neutral action dispatch.
//!
//! Phase 5.5 / slice 5.5.A scaffolding. After Phase 5.4 closed the
//! input side (every `KeyEvent → KeyChord → Action` path lives in
//! `lattice-host`), this module is the seam for the output side
//! that 5.5 fills in: `Action → state mutation`. The renderer's
//! `App::apply` body (today ~2.6k LoC of `match action { ... }`
//! in `lattice-ui-tui::app::dispatch`) will relocate here
//! sub-slice by sub-slice as 5.5.B → 5.5.H land.
//!
//! ## Why this module exists today
//!
//! 5.5.A defines the surface ([`Editor::dispatch`] +
//! [`DispatchOutcome`] + [`RendererSignal`]) but leaves the body
//! empty. Renderer `App` structs keep doing all the work in their
//! own `apply` paths; the stub is a no-op. Fixing the public shape
//! up front means subsequent sub-slices are mechanical moves
//! rather than design decisions, and the future `lattice-ui-gpui`
//! has a stable function signature to compose against from day one.
//!
//! Sub-slices populate the stub:
//!
//! - **5.5.B** -- macro-recording capture, partial-chord clear,
//!   read-only-help guard (the preamble).
//! - **5.5.C** -- simplest `match action { ... }` arms (mutate
//!   `editor` fields directly with no helper call). First
//!   emission of [`RendererSignal::Quit`].
//! - **5.5.D** -- pure-editor-mutation helpers (`clamp_cursor_to_buffer`,
//!   `ensure_cursor_visible`, `dismiss_popup`, ...).
//! - **5.5.E** -- ex-command effect handlers (the ~60-variant
//!   `apply_effect` table + the `do_*` family). First emission of
//!   [`RendererSignal::ThemeChanged`] (from `Effect::SetOption`
//!   on `ui.*` keys).
//! - **5.5.F** -- mode-lifecycle helpers
//!   (`do_open_file_tree` / `do_open_oil` / `do_open_hover` / ...).
//! - **5.5.G** -- final remnants; `App::apply` collapses to the
//!   dispatch call + signal-handling wrapper.
//! - **5.5.H** -- render-coupled cleanup; removes now-vestigial
//!   `App` methods.
//!
//! Focused design doc:
//! `docs/dev/architecture/phase-5-dispatch-extraction.md`.

use crate::action::Action;
use crate::editor::Editor;

/// Result of [`Editor::dispatch`]. Carries the renderer-side
/// side-effects the caller must surface after the host-side state
/// mutation completes.
///
/// Today the TUI's runtime loop repaints every tick, so most
/// dispatches return an empty `renderer_signals`. The `Vec` shape
/// lets host helpers append signals from nested call sites without
/// having to thread them up the call stack -- mirrors how
/// `lattice_grammar::Effect::Many` already aggregates inner effects.
#[derive(Debug, Default)]
pub struct DispatchOutcome {
    /// Host-to-renderer side-effects. Empty for the vast majority
    /// of dispatches (state changed; renderer just refreshes its
    /// per-frame caches on the next tick).
    pub renderer_signals: Vec<RendererSignal>,
}

/// Host-to-renderer side-effect signal.
///
/// **v1 scope is deliberately small** (see
/// `phase-5-dispatch-extraction.md` §"RendererSignal scope"). Only
/// variants with planned emission sites in the existing dispatch
/// path are included; speculative variants (`Repaint`,
/// `TitleChanged`) are deferred until a real need surfaces.
///
/// The renderer matches on this in its post-dispatch hook:
///
/// ```ignore
/// let outcome = self.editor.dispatch(action);
/// for signal in outcome.renderer_signals {
///     match signal {
///         RendererSignal::ThemeChanged => self.rebuild_renderer_theme(),
///         RendererSignal::Quit => self.shutdown(),
///     }
/// }
/// ```
///
/// First emission sites land in sub-slices 5.5.C ([`Self::Quit`])
/// and 5.5.E ([`Self::ThemeChanged`]); the variants exist from
/// 5.5.A so the type surface is fixed before any consumer composes
/// against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererSignal {
    /// The host's neutral [`crate::ui::theme::Theme`] changed
    /// (typically via a `:set ui.*` cascade). The renderer should
    /// rebuild its cached typed theme mirror.
    ThemeChanged,
    /// Quit requested. The renderer should begin its shutdown
    /// sequence. `editor.should_quit` is also set for back-compat
    /// with renderers that poll per-tick.
    Quit,
}

impl Editor {
    /// Renderer-neutral dispatch entry point.
    ///
    /// **5.5.A scaffolding**: body is a stub. Renderer `App::apply`
    /// paths still do all the work in their own crates. Calling this
    /// today returns an empty [`DispatchOutcome`] and changes no
    /// state -- behaviour-preserving by construction.
    ///
    /// **After 5.5 lands**: every [`Action`] flows through here.
    /// Renderer code becomes:
    ///
    /// ```ignore
    /// pub fn apply(&mut self, action: Action) {
    ///     let outcome = self.editor.dispatch(action);
    ///     for signal in outcome.renderer_signals {
    ///         // renderer-specific handling
    ///     }
    ///     // render-coupled per-frame cache refresh stays here
    /// }
    /// ```
    pub fn dispatch(&mut self, action: Action) -> DispatchOutcome {
        let mut out = DispatchOutcome::default();
        handle_action(self, action, &mut out);
        out
    }
}

/// Internal action handler -- the destination 5.5.B+ migrates the
/// `App::apply` body into. Today an empty stub.
///
/// The signature stays stable as sub-slices fill the body: per-arm
/// moves mutate `editor` directly and push into `out.renderer_signals`.
pub(crate) fn handle_action(
    _editor: &mut Editor,
    _action: Action,
    _out: &mut DispatchOutcome,
) {
    // 5.5.A: intentionally empty. Subsequent sub-slices populate
    // this from `lattice-ui-tui::app::dispatch::apply`'s body.
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `RendererSignal` is `Copy + Eq` so renderers can match on it
    /// without cloning and dedupe a signal list cheaply. Pinning the
    /// derives now keeps a future contributor from adding a non-Copy
    /// variant without thinking about hot-path call sites.
    #[test]
    fn renderer_signal_is_copy_eq() {
        fn assert_copy_eq<T: Copy + Eq>(_: T) {}
        assert_copy_eq(RendererSignal::ThemeChanged);
        assert_copy_eq(RendererSignal::Quit);
        assert_eq!(RendererSignal::Quit, RendererSignal::Quit);
        assert_ne!(RendererSignal::Quit, RendererSignal::ThemeChanged);
    }

    /// 5.5.A acceptance shape: [`DispatchOutcome::default()`]
    /// starts with no signals, and `handle_action` is a no-op that
    /// preserves that. When sub-slices populate the body, dedicated
    /// per-arm tests replace this smoke test.
    #[test]
    fn dispatch_outcome_default_has_no_signals() {
        let out = DispatchOutcome::default();
        assert!(out.renderer_signals.is_empty());
    }
}
