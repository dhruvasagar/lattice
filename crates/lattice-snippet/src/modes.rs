//! `snippet-completion-mode` (insert-completion.md §12, CSM.5).
//!
//! The `Mode` adapter + `SyncCompletionSource` impl that turn
//! the snippet registry into a first-class completion source.
//! Auto-activates on writable buffer kinds via
//! `auto_activated_minors_for_buffer_kind` in
//! `lattice-ui-tui::modes`; the source's contribution flows
//! through CSM.3's `ActiveCompletionSources` cache and
//! populates the popup alongside buffer-words / tree-sitter /
//! LSP candidates.
//!
//! Placement: `lattice-snippet` is a leaf in the dep graph
//! (neither `lattice-mode` nor `lattice-completion` depend on
//! it), so adding `lattice-mode` + `lattice-completion` as
//! upstream deps doesn't create a cycle. The mode + source
//! live together in the feature crate -- the placement the
//! mode-architecture rule asks for.

use std::sync::Arc;

use arc_swap::ArcSwap;
use lattice_completion::{
    CandidateData, CandidateKind, CompletionSourceContribution, CompletionSourceKind,
    InsertContext, RawCandidate, SourceId, SyncCompletionSource,
};
use lattice_config::OptionOverrideSet;
use lattice_mode::{
    CapabilitySet, Mode, ModeActivationError, ModeContext, ModeId, ModeKind, ModeRegistry,
};

use crate::registry::SnippetRegistry;

/// Shared handle to a swappable snippet registry. The App owns
/// one `Arc<SharedSnippetRegistry>`; the mode + source capture
/// clones of the outer `Arc`. `:reload-snippets` calls `.store()`
/// on the inner `ArcSwap` -- the mode keeps reading via the
/// outer handle and sees the fresh data on the next `produce()`.
pub type SharedSnippetRegistry = ArcSwap<SnippetRegistry>;

/// Stable id for the snippet completion source. Must match
/// `lattice_completion::SNIPPET_SOURCE_ID` -- the host's
/// per-language allowlist + `:set
/// completion.source.<id>.priority` key off this string.
pub const SNIPPET_COMPLETION_SOURCE_ID: &str = lattice_completion::SNIPPET_SOURCE_ID;

/// Extension-payload kind tag for snippet candidates. Carried
/// in `RawCandidate::data` so the host's `snippet_meta_for`
/// reader can route the payload back through
/// `SnippetRegistry::by_name`. Value matches the host's
/// historic `SNIPPET_COMPLETION_KIND_ID = 2` so the existing
/// accept path keeps recognising snippet candidates without a
/// flag day. Stable u32 -- changing it breaks every snippet
/// candidate's accept path.
pub const SNIPPET_PAYLOAD_KIND_ID: u32 = 2;

/// The `SyncCompletionSource` impl that emits snippet
/// candidates. Captures the registry as an `Arc` so cloning
/// the contribution stays O(1); the registry itself is shared
/// with the host's `App.snippet_registry`.
#[derive(Debug, Clone)]
pub struct SnippetCompletionSource {
    pub registry: Arc<SharedSnippetRegistry>,
}

impl SyncCompletionSource for SnippetCompletionSource {
    fn produce(&self, ctx: &InsertContext<'_>) -> Vec<RawCandidate> {
        // Walk `matching_prefix` per language + the `"*"`
        // bucket, build one candidate per matching snippet. The
        // payload carries the snippet's name (a stable handle);
        // the host's accept path resolves the body via
        // `SnippetRegistry::by_name`.
        let registry = self.registry.load();
        let mut out: Vec<RawCandidate> = Vec::new();
        for snip in registry.matching_prefix(ctx.language, ctx.query) {
            let prefix = snip
                .prefixes
                .first()
                .cloned()
                .unwrap_or_else(|| snip.name.clone());
            let display = match snip.description.as_deref() {
                Some(d) if !d.is_empty() => format!("{prefix}  {d}"),
                _ => prefix.clone(),
            };
            let mut cand = RawCandidate::plain(prefix, CandidateKind::Plain)
                .with_source(SourceId::new(SNIPPET_COMPLETION_SOURCE_ID));
            cand.display = display;
            cand.data = CandidateData::Extension {
                kind_id: SNIPPET_PAYLOAD_KIND_ID,
                payload: snip.name.as_bytes().to_vec(),
            };
            out.push(cand);
        }
        out
    }
}

/// `snippet-completion-mode` -- the `Mode` adapter that
/// contributes [`SnippetCompletionSource`] when active.
/// Marker mode otherwise; the contribution is the whole
/// point.
pub struct SnippetCompletionMode {
    pub registry: Arc<SharedSnippetRegistry>,
}

impl SnippetCompletionMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("snippet-completion-mode")
    }
}

impl Mode for SnippetCompletionMode {
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
            id: SourceId::new(SNIPPET_COMPLETION_SOURCE_ID),
            default_priority: 150,
            auto_trigger: true,
            trigger_chars: Vec::new(),
            // Snippets are prefix-matched and read best alongside
            // their prose context -- no dedicated filter chord
            // (per insert-completion.md §12 chord palette
            // decision).
            popup_filter_chord: None,
            kind: CompletionSourceKind::Sync(Arc::new(SnippetCompletionSource {
                registry: Arc::clone(&self.registry),
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

/// Register `snippet-completion-mode` against `registry` using
/// `snippet_registry` as the contribution-side handle. The App
/// shares the same `Arc<SnippetRegistry>` with this mode so
/// source produce + host accept-path read the same data.
pub fn register_snippet_modes(
    registry: &mut ModeRegistry,
    snippet_registry: Arc<SharedSnippetRegistry>,
) {
    registry
        .register(SnippetCompletionMode {
            registry: snippet_registry,
        })
        .expect("snippet-completion-mode must register without conflict");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;
    use crate::registry::Snippet;
    use lattice_core::Buffer;
    use lattice_protocol::Position;

    fn make_registry() -> Arc<SharedSnippetRegistry> {
        let mut r = SnippetRegistry::new();
        r.insert(
            "rust",
            Snippet {
                name: "for-loop".into(),
                prefixes: vec!["for".into()],
                body: parse("for ${1:i} in ${2:iter} {}").expect("parse"),
                description: Some("for loop".into()),
                scope: String::new(),
            },
        );
        Arc::new(ArcSwap::from_pointee(r))
    }

    #[test]
    fn source_produces_one_candidate_per_matching_snippet() {
        let registry = make_registry();
        let source = SnippetCompletionSource { registry };
        let buffer = Buffer::empty();
        let ctx = InsertContext {
            buffer: &buffer,
            cursor: Position::new(0, 3),
            anchor: Position::new(0, 0),
            query: "for",
            trigger: &lattice_completion::CompletionTrigger::Manual,
            case_sensitive: false,
            language: "rust",
            tree_sitter_symbols: &[],
        };
        let candidates = source.produce(&ctx);
        assert_eq!(candidates.len(), 1);
        let c = &candidates[0];
        assert_eq!(c.text, "for");
        assert!(c.display.contains("for loop"));
        match &c.data {
            CandidateData::Extension { kind_id, payload } => {
                assert_eq!(*kind_id, SNIPPET_PAYLOAD_KIND_ID);
                assert_eq!(std::str::from_utf8(payload).unwrap(), "for-loop");
            }
            other => panic!("unexpected data: {other:?}"),
        }
    }

    #[test]
    fn mode_contributes_the_source_with_expected_metadata() {
        let registry = make_registry();
        let mode = SnippetCompletionMode { registry };
        let contributions = mode.completion_sources();
        assert_eq!(contributions.len(), 1);
        let c = &contributions[0];
        assert_eq!(c.id.as_str(), SNIPPET_COMPLETION_SOURCE_ID);
        assert_eq!(c.default_priority, 150);
        assert!(c.popup_filter_chord.is_none(), "no filter chord per §12");
        assert_eq!(c.kind.kind_label(), "sync");
    }

    #[test]
    fn registers_under_canonical_mode_id() {
        let mut registry = ModeRegistry::new();
        let snippet_registry = make_registry();
        register_snippet_modes(&mut registry, snippet_registry);
        assert!(registry.is_registered(SnippetCompletionMode::mode_id()));
    }
}
