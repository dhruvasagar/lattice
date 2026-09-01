//! `foldable-view-mode` — the one place `<Tab>` folds the block at point.
//!
//! ## Why a shared minor and not a chord per view
//!
//! A grouped, read-only view folds by blocks, and cycling the block at the
//! cursor is a property of *that class of view* rather than of any one of
//! them. By the "shared behaviour is a minor mode, never a copied keymap"
//! standing rule the chord belongs here once.
//!
//! It did not start that way, and the shape of the drift is the argument.
//! As of 2026-09-01 `magit-nav-mode` bound both chords, and its own module
//! doc already stated the generalisation — *"navigating sections and folding
//! are meaningful wherever there are sections"* — while scoping it to magit.
//! `org-agenda-mode` then grew an independent copy. Meanwhile **project
//! search, the LSP references view, `*problems*` and `*compilation*` had
//! neither chord**: four foldable grouped views with no way to collapse a
//! block, and nobody noticed, because a gap in a copied set does not announce
//! itself. That is the same failure `refreshable-view-mode` closed for `gr`,
//! one chord later.
//!
//! ## The split
//!
//! Exactly [`refreshable_view_mode`](crate::refreshable_view_mode)'s:
//!
//! - **`<S-Tab>` is owned outright.** Cycling every fold in the buffer is
//!   generic — magit's `action:magit-cycle-sections` body was literally
//!   `Effect::AppAction(AppEffect::CycleFoldsGlobal)`, the same expression
//!   this mode's own action evaluates to. There is nothing per-view to
//!   declare, so there is no declaration.
//! - **`<Tab>` is a target, not a body.** A view that wants the plain
//!   cycle-the-fold-at-cursor names [`FOLD_TOGGLE_DEFAULT_ACTION`]; a view
//!   with a genuine specialisation names its own action. Magit's is real: on
//!   a status file line the first press expands the diff so `<Tab>` and `=`
//!   agree, and everywhere else it is the plain toggle.
//!
//! Resolution is host-side (`Editor::resolve_fold_toggle_action`) because it
//! needs the buffer's active-mode set, which lives on the editor rather than
//! in the `ServiceRegistry` — the same reason the refresh resolution lives
//! there.
//!
//! ## Activation
//!
//! Automatic: a mode returning `Some` from [`Mode::fold_toggle_action`] pulls
//! this minor in through the implies cascade. One line per view, and no second
//! thing to remember — forgetting it would kill the chord exactly as silently
//! as the copied keymaps did.
//!
//! [`ActivationPolicy::Manual`] so it never auto-attaches to ordinary buffers.
//! **`<Tab>` in a document buffer is the terminal alias for `<C-i>`,
//! jump-list-forward, and must stay that way.** Taking it costs that motion in
//! the views that opt in, which is the deliberate trade: in a grouped
//! read-only view you navigate with `<CR>` and `]]`, and folding a block is
//! the thing you reach for.

use std::sync::{Arc, OnceLock};

use crate::registry::ModeRegistry;
use crate::{
    ActivationPolicy, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    keymap_entry,
};

/// The canonical command name `<Tab>` resolves to. The host intercepts this
/// id, resolves the active modes' declared fold-toggle action, and dispatches
/// *that*.
pub const VIEW_FOLD_TOGGLE_ACTION: &str = "action:view-fold-toggle";

/// The body a view names when it wants the ordinary behaviour: cycle the fold
/// containing the cursor. Named explicitly rather than defaulted so that
/// "this view folds" is a statement the view makes, not one it falls into.
pub const FOLD_TOGGLE_DEFAULT_ACTION: &str = "action:view-fold-toggle-default";

/// `<S-Tab>`. Owned outright — see the module doc.
pub const VIEW_FOLD_CYCLE_ACTION: &str = "action:view-fold-cycle";

/// `foldable-view-mode` minor. Two keymap entries, no per-buffer resources
/// (`Guard = ()`), and no `<Tab>` handler — that body belongs to each view.
pub struct FoldableViewMode;

impl FoldableViewMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("foldable-view-mode")
    }
}

impl Mode for FoldableViewMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    /// Never auto-activated by policy — it arrives through the implies
    /// cascade when a mode declares `fold_toggle_action()`. Keeping this
    /// `Manual` is what stops `<Tab>` shadowing jump-list-forward in ordinary
    /// document buffers.
    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Manual
    }

    fn keymap(&self) -> Keymap {
        Keymap::from_entries(foldable_view_keymap_entries())
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

fn foldable_view_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! {
                mode: Normal,
                chord: "<Tab>",
                doc: "Fold or unfold the block at the cursor",
                cmd: "action:view-fold-toggle"
            },
            keymap_entry! {
                mode: Normal,
                chord: "<S-Tab>",
                doc: "Fold or unfold every block",
                cmd: "action:view-fold-cycle"
            },
        ]
    })
}

pub fn register_foldable_view_mode(registry: &mut ModeRegistry) {
    registry
        .register(FoldableViewMode)
        .expect("foldable-view-mode must register without conflict");
}

/// Register the three action names.
///
/// `action:view-fold-toggle` has a dead body for
/// [`refreshable_view_mode`](crate::refreshable_view_mode)'s reason: the host
/// intercepts the `CommandId` and dispatches whatever the active modes
/// declared, so this apply never runs. The other two are real.
pub fn register_foldable_view_actions(registry: &mut lattice_grammar::CommandRegistry) {
    use lattice_grammar::app_effect::AppEffect;
    use lattice_grammar::effect::Effect;
    use lattice_grammar::registry::ActionSpec;

    registry.register_action(
        VIEW_FOLD_TOGGLE_ACTION,
        "Fold or unfold the block at the cursor (resolves to the active mode's \
         declared fold-toggle action).",
        ActionSpec {
            apply: Arc::new(|_| Ok(Effect::None)),
            args_schema: vec![],
        },
    );
    registry.register_action(
        FOLD_TOGGLE_DEFAULT_ACTION,
        "Fold or unfold the block at the cursor.",
        ActionSpec {
            apply: Arc::new(|_| Ok(Effect::AppAction(AppEffect::CycleFoldAtCursor))),
            args_schema: vec![],
        },
    );
    registry.register_action(
        VIEW_FOLD_CYCLE_ACTION,
        "Fold or unfold every block in this view.",
        ActionSpec {
            apply: Arc::new(|_| Ok(Effect::AppAction(AppEffect::CycleFoldsGlobal))),
            args_schema: vec![],
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_id_uses_the_mode_suffix() {
        assert_eq!(FoldableViewMode::mode_id().as_str(), "foldable-view-mode");
    }

    #[test]
    fn is_a_manual_minor() {
        let m = FoldableViewMode;
        assert_eq!(m.kind(), ModeKind::Minor);
        assert!(matches!(m.activation_policy(), ActivationPolicy::Manual));
    }

    /// Two chords, or the shared-ness is a lie.
    #[test]
    fn binds_tab_and_shift_tab() {
        let entries = foldable_view_keymap_entries();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].chord, "<Tab>");
        assert_eq!(entries[0].command, Some(VIEW_FOLD_TOGGLE_ACTION));
        assert_eq!(entries[1].chord, "<S-Tab>");
        assert_eq!(entries[1].command, Some(VIEW_FOLD_CYCLE_ACTION));
    }

    /// The `<Tab>` body belongs to each view; a handler here would be the
    /// copied-keymap problem wearing a different hat.
    #[test]
    fn contributes_no_action_handler() {
        assert!(FoldableViewMode.action_handlers().is_empty());
    }

    /// It does not declare a fold toggle of its own — otherwise it would pull
    /// itself into the implies cascade.
    #[test]
    fn declares_no_fold_toggle_itself() {
        assert_eq!(FoldableViewMode.fold_toggle_action(), None);
    }
}
