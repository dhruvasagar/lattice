//! `hover-mode` -- minor mode activated on the hover popup buffer
//! (M.4 hover-popup unification). Marker mode for v1; the only
//! behaviour gated on it today is the State-A auto-dismiss-on-
//! doc-cursor-motion check in the App's dispatch loop. Future
//! hover-only contributions (auto-close timer, bound-`<Esc>`-to-
//! dismiss, signature-help fan-in) layer on without touching the
//! popup-overlay code.
//!
//! Hover content is markdown; the major mode the App activates
//! alongside `hover-mode` is `markdown-mode`, so the renderer's
//! syntax + link extraction treats hover content as any other
//! markdown buffer. Help-mode is intentionally NOT activated on
//! hover popups -- hover content's links are typically external
//! URLs we don't follow internally.

use lattice_config::OptionOverrideSet;

use crate::{CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind};

/// `hover-mode` — marker minor on the hover popup buffer (activated
/// alongside `markdown-mode`). Wraps long lines and borrows help-mode's
/// read-only invocation runner; see the module docs.
pub struct HoverMode;

impl HoverMode {
    /// The canonical id, `"hover-mode"` — what [`Mode::id`](crate::Mode::id)
    /// returns. Use it to name this mode without an instance (activation,
    /// `implies`, keymap layers, tests).
    pub fn mode_id() -> ModeId {
        ModeId::new("hover-mode")
    }
}

impl Mode for HoverMode {
    type Guard = ();
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn options(&self) -> OptionOverrideSet {
        // Bug 4: hover popups are width-constrained (cursor-
        // anchored, capped at ~80 cells); long markdown bodies
        // need to wrap or they overflow horizontally with
        // no horizontal scroll path.
        //
        // A hover is read while focus stays in the document, so the popup
        // is never the focused pane — and inactive-pane dimming made the
        // documentation being read hard to read until it was focused.
        lattice_config::overrides! {
            lattice_config::Wrap = true,
            lattice_config::UiDimInactive = false,
        }
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    /// 2026-05-26: hover popups want the same read-only motion
    /// dispatch help buffers use. Point at help-mode's runner id
    /// so the host's invocation_runners map (where boot registers
    /// `Editor::run_help_invocation` under `help-mode`) routes
    /// `j` / `k` / `gg` / `G` through the help runner without
    /// needing a separate `run_hover_invocation`.
    fn invocation_runner(&self) -> Option<ModeId> {
        Some(crate::HelpMode::mode_id())
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_and_kind() {
        assert_eq!(HoverMode.id(), HoverMode::mode_id());
        assert_eq!(HoverMode::mode_id().as_str(), "hover-mode");
        assert_eq!(HoverMode.kind(), ModeKind::Minor);
    }

    #[test]
    fn contributes_wrap_and_no_inactive_dim() {
        // Bug 4: hover popups are width-constrained; long markdown bodies
        // need to wrap. And a hover is read while focus stays in the
        // document, so the popup must not take the inactive-pane dim.
        let opts = HoverMode.options();
        let value_of = |id: std::any::TypeId| {
            opts.iter()
                .find(|o| o.option_type_id == id)
                .and_then(|o| o.value.downcast_ref::<bool>().copied())
        };
        assert_eq!(
            opts.iter().count(),
            2,
            "Wrap and UiDimInactive, nothing else"
        );
        assert_eq!(
            value_of(std::any::TypeId::of::<lattice_config::Wrap>()),
            Some(true)
        );
        assert_eq!(
            value_of(std::any::TypeId::of::<lattice_config::UiDimInactive>()),
            Some(false)
        );
    }
}
