//! `refreshable-view-mode` — the one place `gr` means "refresh this view".
//!
//! ## Why a shared minor and not a chord per mode
//!
//! `gr` refreshes a synthetic buffer in every synthetic buffer that has
//! one. That is a property of synthetic views *as a class*, so by the
//! "shared behaviour is a minor mode, never a copied keymap" standing
//! rule the chord belongs here once.
//!
//! It did not start that way. As of 2026-08-10 three independent copies
//! existed — `magit-core-mode` (`action:magit-refresh`),
//! `compilation-mode` (`action:compilation-recompile`) and
//! `providers::search` (`action:search-refresh`) — and the two synthetic
//! views that landed most recently, `*problems*` and narrow, **had no
//! `gr` at all**. Nobody noticed, because a gap in a copied set does not
//! announce itself. That is the failure this mode closes.
//!
//! ## The split
//!
//! This mode owns the **chord**. Each view's mode owns the **body**, and
//! declares which of its own actions is the refresh via
//! [`Mode::refresh_action`](crate::Mode::refresh_action) — a target, not
//! a body, so existing handlers keep working untouched.
//!
//! Resolution is host-side (`Editor::resolve_refresh_action`) because it
//! needs the buffer's active-mode set, which lives on the editor rather
//! than in the `ServiceRegistry`. Same split as
//! [`invocation_runner`](crate::Mode::invocation_runner): the mode
//! declares, the host walks and dispatches. `action:view-refresh` is
//! therefore a *generic* host action — it carries no per-view logic, it
//! only redirects to whatever the active modes named.
//!
//! ## Activation
//!
//! Automatic: a mode returning `Some` from `refresh_action()` pulls this
//! minor in through the implies cascade (see
//! `ModeRegistry::record_implies_cascade`). A mode author writes one
//! line and gets the chord — there is no second thing to remember, which
//! matters because forgetting it would kill the chord exactly as
//! silently as the copied keymaps did.
//!
//! `ActivationPolicy::Manual` so it never auto-attaches to ordinary
//! buffers: `gr` in a source buffer is LSP references and must stay that
//! way.

use std::sync::{Arc, OnceLock};

use crate::registry::ModeRegistry;
use crate::{
    ActivationPolicy, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    keymap_entry,
};

/// The canonical command name the shared `gr` resolves to. The host
/// intercepts this id, resolves the active modes' declared refresh
/// action, and dispatches *that*.
pub const VIEW_REFRESH_ACTION: &str = "action:view-refresh";

/// `refreshable-view-mode` minor. A marker mode: one keymap layer, no
/// per-buffer resources (`Guard = ()`), and deliberately **no action
/// handler** — the body it would hold lives in each view's own mode.
pub struct RefreshableViewMode;

impl RefreshableViewMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("refreshable-view-mode")
    }
}

impl Mode for RefreshableViewMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    /// Never auto-activated by policy — it arrives through the implies
    /// cascade when a mode declares `refresh_action()`. Keeping this
    /// `Manual` is what stops `gr` shadowing LSP references on ordinary
    /// document buffers.
    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Manual
    }

    /// Pushed once at boot under `MinorMode(refreshable-view-mode)`;
    /// K.1.c's per-keystroke filter gates it to buffers where this mode
    /// is active.
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(refreshable_view_keymap_entries())
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// The single entry. `gr` — the chord every magit buffer, the
/// compilation buffer and the search view already used, now declared
/// once.
fn refreshable_view_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![keymap_entry! {
            mode: Normal,
            chord: "gr",
            doc: "Refresh this view",
            cmd: "action:view-refresh"
        }]
    })
}

pub fn register_refreshable_view_mode(registry: &mut ModeRegistry) {
    registry
        .register(RefreshableViewMode)
        .expect("refreshable-view-mode must register without conflict");
}

/// Register `action:view-refresh` so the mode's keymap `cmd` name
/// resolves at boot.
///
/// The `apply` body is a dead `Effect::None`, like `repl-mode`'s: the
/// host intercepts this `CommandId` in chord dispatch, resolves the
/// active modes' declared refresh action, and dispatches *that* — so
/// this body never runs. It exists so the `CommandId` resolves for the
/// chord binding.
pub fn register_refreshable_view_actions(registry: &mut lattice_grammar::CommandRegistry) {
    use lattice_grammar::registry::ActionSpec;
    registry.register_action(
        VIEW_REFRESH_ACTION,
        "Refresh this view (resolves to the active mode's declared refresh action).",
        ActionSpec {
            apply: Arc::new(|_| Ok(lattice_grammar::effect::Effect::None)),
            args_schema: vec![],
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_id_uses_the_mode_suffix() {
        assert_eq!(
            RefreshableViewMode::mode_id().as_str(),
            "refreshable-view-mode"
        );
    }

    #[test]
    fn is_a_manual_minor() {
        let m = RefreshableViewMode;
        assert_eq!(m.kind(), ModeKind::Minor);
        assert!(matches!(m.activation_policy(), ActivationPolicy::Manual));
    }

    #[test]
    fn binds_gr_to_the_generic_action() {
        let entries = refreshable_view_keymap_entries();
        assert_eq!(entries.len(), 1, "one chord, or the shared-ness is a lie");
        assert_eq!(entries[0].chord, "gr");
        assert_eq!(entries[0].command, Some(VIEW_REFRESH_ACTION));
    }

    /// The mode must contribute no handler: the body belongs to each
    /// view's own mode. A handler here would be the copied-keymap
    /// problem wearing a different hat.
    #[test]
    fn contributes_no_action_handler() {
        assert!(RefreshableViewMode.action_handlers().is_empty());
    }

    /// It does not declare a refresh of its own — otherwise it would
    /// pull itself into the implies cascade.
    #[test]
    fn declares_no_refresh_action_itself() {
        assert_eq!(RefreshableViewMode.refresh_action(), None);
    }
}
