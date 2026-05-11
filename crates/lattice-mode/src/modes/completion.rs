//! `completion-mode` -- minor mode activated on the document
//! buffer while an Insert-mode completion popup is open
//! (insert-completion.md §12). Two responsibilities:
//!
//! - **Gate the popup-layer chord overlay.** The host's
//!   `sync_keymap_overlays` pushes / pops the
//!   `"completion-popup"` keymap layer based on whether this
//!   mode is active. Same shape `hover-mode` /
//!   `active-snippet-mode` use; replaces the imperative
//!   `App.insert_completion.is_some()` flag the v1 wiring
//!   gated on (CSM.2).
//!
//! - **Act as the engine's active marker for source
//!   resolution.** Sources are contributed by *other* minors
//!   (`lsp-completion-mode`, `buffer-words-mode`,
//!   `snippet-completion-mode`, ...). `completion-mode` is
//!   the engine surface that consumes them; the
//!   `ActiveCompletionSources` cache (CSM.3) only matters
//!   when this mode is active because that's the only state
//!   in which the popup is consuming candidates.
//!
//! Placement: in `lattice-mode::modes` alongside `HelpMode` /
//! `HoverMode` because:
//!
//! - `lattice-completion` can't host the mode without
//!   creating a dep cycle (`lattice-mode` already depends on
//!   `lattice-completion` per CSM.1 for the
//!   `CompletionSourceContribution` return type on
//!   `Mode::completion_sources()`).
//! - The mode is a *framework* minor -- it has no feature
//!   crate to live with. Same rationale `hover-mode` uses for
//!   sitting here despite being consumed by LSP hover, the
//!   lattice-grammar palette, and any future "show some
//!   markdown in a popup" caller.
//!
//! v1 behavior is marker-only -- empty options, empty
//! lifecycle. The host owns activation / deactivation and the
//! keymap-overlay sync; CSM.2 only formalises the existence
//! of the mode so the gate is mode-driven rather than
//! state-driven.

use lattice_completion::CompletionSourceContribution;
use lattice_config::OptionOverrideSet;

use crate::{
    BufferLocal, CapabilitySet, Mode, ModeActivationError, ModeContext, ModeId, ModeKind,
};

/// The completion popup's engine minor. See module docs.
pub struct CompletionMode;

impl CompletionMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("completion-mode")
    }
}

impl Mode for CompletionMode {
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn options(&self) -> OptionOverrideSet {
        // No contributed options today. A future
        // `completion.popup-anchor` / `completion.docs-side`
        // could live here once the renderer surface stabilises.
        OptionOverrideSet::default()
    }
    fn required_capabilities(&self) -> CapabilitySet {
        // Marker mode -- nothing to require.
        CapabilitySet::empty()
    }
    fn on_activate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
        // The popup's `InsertCompletionState` lives on the App
        // (`App.insert_completion`); the host populates it before
        // calling `activate_minor` for this mode. Nothing to do
        // here -- the mode being active is the signal the keymap
        // overlay + active-source resolver read.
        Ok(())
    }
    fn on_deactivate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
        // Symmetric: the host drops `App.insert_completion`
        // before calling `deactivate_minor`. The mode going
        // inactive is the signal the keymap overlay pops the
        // `"completion-popup"` layer in `sync_keymap_overlays`.
        Ok(())
    }
}

/// CSM.3 (insert-completion.md §12.4): cached active completion-
/// source set for a buffer. The host
/// (`App::recompute_active_completion_sources_for`) recomputes
/// this on every mode-activation / -deactivation transition by
/// walking `active_modes` and calling
/// `mode.completion_sources()` on each. The aggregator reads
/// the cache on the popup-open / refilter path -- O(1) buffer-
/// local lookup, never a walk over every active mode per
/// keystroke.
///
/// `OWNER_MODE` is `"completion-mode"` because the engine mode
/// is what consumes the cache; the cache is meaningless when
/// `completion-mode` is inactive (no popup is open). Sources
/// are *contributed* by other minors (`lsp-completion-mode`,
/// `buffer-words-mode`, ...); the cache merges their
/// contributions for the engine to consume.
#[derive(Debug, Clone, Default)]
pub struct ActiveCompletionSources(pub Vec<CompletionSourceContribution>);

impl BufferLocal for ActiveCompletionSources {
    const NAME: &'static str = "completion-mode.active-sources";
    const DOC: &'static str =
        "Cached active insert-completion source set for this \
         buffer. Recomputed on every mode-activation / \
         -deactivation transition; read by the aggregator on \
         the popup-open / refilter path.";
    const OWNER_MODE: &'static str = "completion-mode";
    fn describe(&self) -> String {
        format!("{} source(s)", self.0.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActiveModes, BufferLocals, ModeRegistry};
    use lattice_protocol::ids::BufferId;

    #[test]
    fn completion_mode_is_a_minor() {
        assert_eq!(CompletionMode.kind(), ModeKind::Minor);
        assert_eq!(CompletionMode.id(), CompletionMode::mode_id());
        assert_eq!(CompletionMode::mode_id().as_str(), "completion-mode");
    }

    #[test]
    fn completion_mode_registers_and_activates() {
        let mut registry = ModeRegistry::new();
        registry
            .register(CompletionMode)
            .expect("register completion-mode");
        let mut active = ActiveModes::new();
        let mut locals = BufferLocals::new();
        let events = registry
            .activate_minor(
                &mut active,
                &mut locals,
                BufferId::new(0),
                CompletionMode::mode_id(),
                CapabilitySet::empty(),
            )
            .expect("activate completion-mode");
        assert!(active.has_minor(CompletionMode::mode_id()));
        assert!(!events.is_empty(), "MinorActivated event should fire");
    }

    #[test]
    fn completion_mode_default_completion_sources_is_empty() {
        // `completion-mode` is the engine, not a source-
        // contributor. Sources come from other minors
        // (`lsp-completion-mode`, `buffer-words-mode`, ...).
        assert!(CompletionMode.completion_sources().is_empty());
    }

    #[test]
    fn active_completion_sources_describes_count() {
        // The buffer-local's `describe()` is what
        // `:describe-buffer`'s descriptor surface renders.
        let empty = ActiveCompletionSources::default();
        assert_eq!(empty.describe(), "0 source(s)");
    }

    #[test]
    fn active_completion_sources_is_a_buffer_local() {
        // Trait shape check: the type satisfies BufferLocal so
        // it can be stored in BufferLocals and surfaced through
        // `iter_descriptors`. CSM.4 -- CSM.8 verify the
        // production read path lights up under each migrated
        // source.
        let mut locals = BufferLocals::new();
        locals.insert(ActiveCompletionSources(Vec::new()));
        assert!(locals.get::<ActiveCompletionSources>().is_some());
        let d = locals.iter_descriptors().next().expect("descriptor");
        assert_eq!(d.name, "completion-mode.active-sources");
        assert_eq!(d.owner_mode, "completion-mode");
    }
}
