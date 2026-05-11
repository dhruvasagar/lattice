//! LSP completion source (CSM.8a).
//!
//! [`LspCompletionSource`] is the `AsyncCompletionSource` impl
//! that the `lsp-completion-mode` contributes. It owns an
//! `LspSupervisorHandle` and is wired into the
//! `ActiveCompletionSources` cache via the mode's
//! `completion_sources()` contribution.
//!
//! **Slice scope** -- CSM.8a registers the source surface and
//! its `popup_filter_chord: Some('o')` so the cache + CSM.K2
//! filter chord plumbing are complete. `produce_async` is a
//! placeholder that resolves immediately with no candidates;
//! the production LSP fan-out stays in
//! `lattice-ui-tui::app::lsp::do_lsp_insert_completion_request`
//! until CSM.8b moves it here. The split keeps the slice's
//! blast radius manageable -- the LSP-typed metadata sidecar
//! (`App.insert_completion_lsp_meta`), the accept-path
//! `lsp_completion_meta_for` decoder, and the multi-server
//! dedup + isIncomplete refresh all stay where they are.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use lattice_completion::{
    AsyncCompletionSource, CandidateSink, CompletionSourceContribution, CompletionSourceKind,
    InsertContextSnapshot, SourceId,
};
use lattice_config::OptionOverrideSet;
use lattice_mode::{
    CapabilitySet, Mode, ModeActivationError, ModeContext, ModeId, ModeKind, ModeRegistry,
};
use lattice_protocol::CancellationToken;

use crate::supervisor::LspSupervisorHandle;

/// CSM.8a stub. Holds the supervisor handle so CSM.8b can move
/// the fan-out into `produce_async` without changing the mode's
/// public surface. Today the future is a no-op.
#[derive(Debug, Clone)]
pub struct LspCompletionSource {
    pub lsp: LspSupervisorHandle,
}

impl AsyncCompletionSource for LspCompletionSource {
    fn produce_async(
        &self,
        _ctx: InsertContextSnapshot,
        _sink: Arc<dyn CandidateSink>,
        _token: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        // CSM.8a placeholder. CSM.8b populates this with the
        // multi-server fan-out currently in
        // `lattice-ui-tui::app::lsp::do_lsp_insert_completion_request`,
        // pushing one `RawCandidate` per LSP item into `sink`
        // with the item's serde-encoded payload so the accept
        // path can decode without a sidecar.
        Box::pin(async {})
    }
}

/// `lsp-completion-mode` -- LSP-driven insert-mode completion
/// (CSM.8a). M.6.0 declared it as a marker minor; CSM.8a makes
/// it source-contributing. Holds the [`LspSupervisorHandle`]
/// so the contributed source can fan out to attached servers.
/// `popup_filter_chord = Some('o')` ⇒ `<C-o>` inside
/// `completion-popup-mode` narrows the popup to LSP only
/// (CSM.K2).
#[derive(Debug, Clone)]
pub struct LspCompletionMode {
    pub lsp: LspSupervisorHandle,
}

impl LspCompletionMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("lsp-completion-mode")
    }
}

impl Mode for LspCompletionMode {
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
            id: SourceId::new(lattice_completion::LSP_COMPLETION_SOURCE_ID),
            // 200 per insert-completion.md §3.4 -- LSP outranks
            // every sync source (snippets 150, buffer-words 100,
            // path 90, tree-sitter 80) because language-server
            // candidates are the most contextually accurate.
            default_priority: 200,
            auto_trigger: true,
            // Server-advertised triggers (`.`, `::`, ...) are
            // populated dynamically post-CSM.8b when the source
            // reads the attached server's
            // `completionProvider.triggerCharacters`. CSM.8a
            // ships an empty list; sync triggers and manual
            // `<C-Space>` fire the source regardless.
            trigger_chars: Vec::new(),
            popup_filter_chord: Some('o'),
            kind: CompletionSourceKind::Async(Arc::new(LspCompletionSource {
                lsp: self.lsp.clone(),
            })),
        }]
    }
    fn on_activate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
        Ok(())
    }
    fn on_deactivate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
        Ok(())
    }
}

/// Register `lsp-completion-mode` against `registry`. Called
/// from the App's boot path alongside
/// [`crate::modes::register_lsp_log_modes`]; the supervisor
/// handle is shared so source produce + host accept-path read
/// the same data.
pub fn register_lsp_completion_mode(
    registry: &mut ModeRegistry,
    lsp: LspSupervisorHandle,
) {
    registry
        .register(LspCompletionMode { lsp })
        .expect("lsp-completion-mode must register without conflict");
}
