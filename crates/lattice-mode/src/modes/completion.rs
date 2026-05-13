//! Completion-mode pair (insert-completion.md §12).
//!
//! The completion machinery surfaces through two minors with
//! different lifecycles:
//!
//! - **`completion-mode`** -- "this buffer participates in
//!   insert-mode completion." Auto-activates on writable buffer
//!   kinds (Document) at buffer creation; stays active for the
//!   buffer's lifetime. Acts as the gate `do_completion_trigger`
//!   checks before opening the popup -- read-only kinds (Help,
//!   FileTree, Oil) never activate it, so `<C-Space>` in those
//!   buffers is a silent no-op.
//!
//! - **`completion-popup-mode`** -- transient. Active iff the
//!   candidate popup is live. Owns the popup-internal keymap
//!   (`<C-n>` / `<C-p>` / `<C-y>` / `<Tab>` / `<CR>` / `<Esc>` /
//!   `<C-e>` / `<C-d>` / `<C-Space>` / `<C-f>` / `<C-b>` plus
//!   the per-source filter chords from CSM.K2). Replaces the
//!   imperative `App.insert_completion.is_some()` flag the v1
//!   wiring gated on (CSM.2's original `completion-mode`).
//!
//! Both modes own no contributed options today. The pairing
//! mirrors `lsp-mode` (umbrella, persistent) + the LSP sub-modes
//! (per-feature, persistent) -- here the lifecycle distinction
//! is "persistent on writable buffers" vs "transient with the
//! popup."
//!
//! Placement: in `lattice-mode::modes` (not `lattice-completion`)
//! to avoid a dep cycle -- `lattice-mode` already depends on
//! `lattice-completion` for the `CompletionSourceContribution`
//! return type on `Mode::completion_sources()` (CSM.1), so
//! reversing direction would require `Mode` itself to live in
//! completion.

use std::sync::Arc;

use lattice_completion::{
    BufferWordsSource, CompletionSourceContribution, CompletionSourceKind, PathCompletionSource,
    SourceId,
};
use lattice_config::OptionOverrideSet;

use crate::{BufferLocal, CapabilitySet, Mode, ModeActivationError, ModeContext, ModeId, ModeKind};

/// The "buffer participates in insert-mode completion" marker.
/// Auto-activates on writable kinds at buffer creation. See
/// module docs.
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
        OptionOverrideSet::default()
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn on_activate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
        Ok(())
    }
    fn on_deactivate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
        Ok(())
    }
}

/// CSM.4 (insert-completion.md §12 first source migration):
/// `buffer-words-mode` -- contributes the buffer-words
/// completion source. Auto-activates on writable buffer kinds
/// (Document) so the popup's all-sources view shows words
/// scraped from the active buffer's text. `popup_filter_chord =
/// Some('b')` ⇒ `<C-b>` inside `completion-popup-mode` narrows
/// the popup to buffer-words only.
///
/// Placement: lives in `lattice-mode::modes::completion`
/// alongside `CompletionMode`. Ideal location would be
/// `lattice-completion::modes` (the "feature crate owns its
/// mode" rule), but `Mode` is defined in `lattice-mode` and the
/// dep direction is `lattice-mode` → `lattice-completion`
/// (CSM.1); the `Mode` impl can't live in `lattice-completion`
/// without a cycle. The source struct (`BufferWordsSource`)
/// stays in `lattice-completion::insert` where it belongs;
/// only the thin `Mode` adapter sits here.
pub struct BufferWordsMode;

impl BufferWordsMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("buffer-words-mode")
    }
}

impl Mode for BufferWordsMode {
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn options(&self) -> OptionOverrideSet {
        OptionOverrideSet::default()
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn completion_sources(&self) -> Vec<CompletionSourceContribution> {
        vec![CompletionSourceContribution {
            id: SourceId::new(BufferWordsSource::ID),
            default_priority: 100,
            auto_trigger: true,
            trigger_chars: Vec::new(),
            popup_filter_chord: Some('b'),
            kind: CompletionSourceKind::Sync(Arc::new(BufferWordsSource::new())),
        }]
    }
    fn on_activate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
        Ok(())
    }
    fn on_deactivate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
        Ok(())
    }
}

/// CSM.7 (insert-completion.md §12 fourth source migration):
/// `path-completion-mode` -- contributes the path-completion
/// source. Auto-activates on writable buffer kinds; the
/// source self-suppresses outside string scopes (gated by
/// `ctx.path_context` which the host sets from its tree-sitter
/// scope detection). `popup_filter_chord = Some('f')` ⇒
/// `<C-f>` inside `completion-popup-mode` narrows the popup
/// to filesystem entries only.
///
/// Same placement note as `BufferWordsMode`: ideal location
/// would be `lattice-completion::modes` per the "feature
/// crate owns its mode" rule, but the
/// `lattice-mode -> lattice-completion` dep direction forces
/// the thin `Mode` adapter to live in `lattice-mode`; the
/// underlying `PathCompletionSource` stays in
/// `lattice-completion::path`.
pub struct PathCompletionMode;

impl PathCompletionMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("path-completion-mode")
    }
}

impl Mode for PathCompletionMode {
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn options(&self) -> OptionOverrideSet {
        OptionOverrideSet::default()
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn completion_sources(&self) -> Vec<CompletionSourceContribution> {
        vec![CompletionSourceContribution {
            id: SourceId::new(lattice_completion::PATH_SOURCE_ID),
            // 90 per insert-completion.md §3.4. The source self-
            // suppresses outside string scopes via
            // `ctx.path_context`; when active, paths sort below
            // buffer-words (100) and snippet (150) but above
            // tree-sitter symbols (80) because they're the
            // user's typed-context-correct candidates.
            default_priority: 90,
            // The historic behaviour treats `<C-x><C-f>` (vim
            // file-name completion) as the explicit trigger;
            // CSM.K2 makes `<C-f>` the in-popup filter chord.
            // No trigger char in the host-fired flow today --
            // the source fires via the context's
            // `path_context` flag, not by trigger char.
            auto_trigger: true,
            trigger_chars: vec!['/'],
            popup_filter_chord: Some('f'),
            kind: CompletionSourceKind::Sync(Arc::new(PathCompletionSource)),
        }]
    }
    fn on_activate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
        Ok(())
    }
    fn on_deactivate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
        Ok(())
    }
}

/// The popup-is-live transient minor. See module docs.
pub struct CompletionPopupMode;

impl CompletionPopupMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("completion-popup-mode")
    }
}

impl Mode for CompletionPopupMode {
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn options(&self) -> OptionOverrideSet {
        OptionOverrideSet::default()
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn on_activate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
        Ok(())
    }
    fn on_deactivate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
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
/// `OWNER_MODE` is `"completion-mode"` because that's the
/// persistent gate; the cache is meaningless when
/// `completion-mode` is inactive (read-only buffers never
/// trigger the popup, never need the cache).
#[derive(Debug, Clone, Default)]
pub struct ActiveCompletionSources(pub Vec<CompletionSourceContribution>);

impl BufferLocal for ActiveCompletionSources {
    const NAME: &'static str = "completion-mode.active-sources";
    const DOC: &'static str = "Cached active insert-completion source set for this \
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
    fn completion_popup_mode_is_a_minor() {
        assert_eq!(CompletionPopupMode.kind(), ModeKind::Minor);
        assert_eq!(CompletionPopupMode.id(), CompletionPopupMode::mode_id());
        assert_eq!(
            CompletionPopupMode::mode_id().as_str(),
            "completion-popup-mode",
        );
    }

    #[test]
    fn both_modes_register_and_activate() {
        let mut registry = ModeRegistry::new();
        registry
            .register(CompletionMode)
            .expect("register completion-mode");
        registry
            .register(CompletionPopupMode)
            .expect("register completion-popup-mode");
        let mut active = ActiveModes::new();
        let mut locals = BufferLocals::new();
        let cfg = lattice_config::ConfigRegistry::new();
        let evt = std::sync::Arc::new(lattice_runtime::EventBus::new());
        let svc = crate::services::ServiceRegistry::new();
        registry
            .activate_minor(
                &mut active,
                &mut locals,
                &cfg,
                &evt,
                &svc,
                BufferId::new(0),
                CompletionMode::mode_id(),
                CapabilitySet::empty(),
            )
            .expect("activate completion-mode");
        registry
            .activate_minor(
                &mut active,
                &mut locals,
                &cfg,
                &evt,
                &svc,
                BufferId::new(0),
                CompletionPopupMode::mode_id(),
                CapabilitySet::empty(),
            )
            .expect("activate completion-popup-mode");
        assert!(active.has_minor(CompletionMode::mode_id()));
        assert!(active.has_minor(CompletionPopupMode::mode_id()));
    }

    #[test]
    fn both_modes_default_completion_sources_is_empty() {
        // Neither mode is a source-contributor. Sources come
        // from feature-owned minors (`lsp-completion-mode`,
        // `buffer-words-mode`, ...).
        assert!(CompletionMode.completion_sources().is_empty());
        assert!(CompletionPopupMode.completion_sources().is_empty());
    }

    #[test]
    fn active_completion_sources_describes_count() {
        let empty = ActiveCompletionSources::default();
        assert_eq!(empty.describe(), "0 source(s)");
    }

    #[test]
    fn active_completion_sources_is_a_buffer_local() {
        let mut locals = BufferLocals::new();
        locals.insert(ActiveCompletionSources(Vec::new()));
        assert!(locals.get::<ActiveCompletionSources>().is_some());
        let d = locals.iter_descriptors().next().expect("descriptor");
        assert_eq!(d.name, "completion-mode.active-sources");
        assert_eq!(d.owner_mode, "completion-mode");
    }
}
