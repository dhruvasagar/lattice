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

use std::sync::{Arc, OnceLock};

use arc_swap::ArcSwap;
use lattice_completion::{
    CandidateData, CandidateKind, CompletionSourceContribution, CompletionSourceKind,
    InsertContext, RawCandidate, SourceId, SyncCompletionSource,
};
use lattice_config::OptionOverrideSet;
use lattice_grammar::{CommandRegistryHandle, Effect};
use lattice_mode::{
    keymap_entry, ActionContext, ActionHandler, ActionHandlerRegistration,
    ActionHandlerRegistryHandle, ActivationPolicy, BufferStoreHandle, CapabilitySet, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, ModeRegistry,
};
use lattice_protocol::selection::{Selection, SelectionSet};

use crate::activation::SnippetActivationPolicyHandle;
use crate::active::TabstopGroup;
use crate::registry::SnippetRegistry;
use crate::session::SnippetSessionHandle;

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
    type Guard = ();
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
            popup_filter_chord: None,
            kind: CompletionSourceKind::Sync(Arc::new(SnippetCompletionSource {
                registry: Arc::clone(&self.registry),
            })),
        }]
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// `snippet-mode` — the base "snippets enabled for this buffer"
/// minor mode (SN.3). The language-aware feature gate: its
/// [`ActivationPolicy`] decides which buffers get snippets, and it
/// `implies` [`SnippetCompletionMode`] so the completion *source*
/// rides the same gate (activating `snippet-mode` brings the
/// source with it — no separate activation).
///
/// Three-mode decomposition (confirmed with the user 2026-06-14):
/// - **`snippet-mode`** (this) — the gate; owns `<C-x><C-s>`
///   direct-expand (SN.3c; today still host-bound).
/// - **`snippet-completion-mode`** — provides the `gen:snippet`
///   completion source only.
/// - **`active-snippet-mode`** — in-flight placeholder nav
///   (`<Tab>` / `<S-Tab>` / `<Esc>`), lit by the session-backed
///   reconciler when a snippet expands.
///
/// SN.3b makes the policy config-driven: the mode reads a shared
/// [`SnippetActivationPolicyHandle`] that the host folds
/// `snippet.activation` / `snippet.languages` into at boot and on
/// every `:set` of those keys. The cell defaults to
/// `ActivationPolicy::Global` (behavior-preserving — the pre-SN.3
/// language-blind activation on every Document), which is also the
/// folded value when `snippet.activation = global` (the default).
pub struct SnippetMode {
    implies: Vec<ModeId>,
    policy: SnippetActivationPolicyHandle,
}

impl SnippetMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("snippet-mode")
    }

    /// Construct with a caller-owned shared policy cell. The host
    /// (`register_snippet_modes`) creates the cell and keeps a clone
    /// so boot + `:set` can re-fold config into it; this mode reads
    /// the live value in [`activation_policy`](Mode::activation_policy).
    pub fn with_policy(policy: SnippetActivationPolicyHandle) -> Self {
        Self {
            implies: vec![SnippetCompletionMode::mode_id()],
            policy,
        }
    }

    /// Construct with a fresh, default-`Global` policy cell. Used by
    /// tests and any caller that doesn't wire live config folding;
    /// the internal cell is private, so the policy is fixed at
    /// `Global` for the mode's lifetime.
    pub fn new() -> Self {
        Self::with_policy(Arc::new(ArcSwap::from_pointee(ActivationPolicy::Global)))
    }
}

impl Default for SnippetMode {
    fn default() -> Self {
        Self::new()
    }
}

impl Mode for SnippetMode {
    type Guard = ();
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    /// SN.3b: read the live, host-folded policy from the shared
    /// cell. Defaults to `Global` (snippets on every document
    /// buffer); the snippet *source* still self-filters by language,
    /// so `Global` means "each buffer sees its own language's
    /// snippets", not "all snippets everywhere". The resolver calls
    /// this on each `MajorEntered`, so a `:set snippet.activation`
    /// takes effect for buffers opened afterward.
    fn activation_policy(&self) -> ActivationPolicy {
        (**self.policy.load()).clone()
    }
    /// Bring `snippet-completion-mode` (the source provider) along
    /// whenever `snippet-mode` activates — the source is gated to
    /// exactly the buffers where snippets are enabled.
    fn implies(&self) -> &[ModeId] {
        &self.implies
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

fn snippet_active_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry!(
                mode: Insert,
                chord: "<Tab>",
                doc: "Move to the next placeholder in the active snippet session.",
                cmd: "action:snippet-next-placeholder"
            ),
            keymap_entry!(
                mode: Insert,
                chord: "<S-Tab>",
                doc: "Move to the previous placeholder in the active snippet session.",
                cmd: "action:snippet-prev-placeholder"
            ),
            keymap_entry!(
                mode: Insert,
                chord: "<Esc>",
                doc: "Exit the active snippet session and return to Normal mode.",
                cmd: "action:snippet-leave"
            ),
        ]
    })
}

/// `active-snippet-mode` — a transient minor mode activated while
/// a snippet session is live on a buffer. Contributes the three
/// Insert-mode bindings (`<Tab>` / `<S-Tab>` / `<Esc>`) via
/// `Mode::keymap()`; K.2.4 registers them at startup under
/// `KeymapLayer::MinorMode("active-snippet-mode")` and K.1.c gates
/// them to buffers where the mode is active.
///
/// **SN.2b (2026-06-12):** the mode now owns the `<Tab>` /
/// `<S-Tab>` placeholder-navigation handler *bodies*, not just the
/// chord choice. The bodies were `Editor::do_snippet_next/prev_
/// placeholder` in `lattice-host` — the half-migration
/// `feedback_mode_owns_its_surface` forbids (keymap in the mode,
/// handler in the host). They register here as
/// `ActionContext -> Effect` closures on the `ActionHandlerRegistry`
/// substrate (the path the project-search provider already uses),
/// advancing the shared [`SnippetSession`](crate::SnippetSession)
/// service and returning `Effect::SelectionChange` so the host's
/// generic effect pipeline moves the cursor. `<Esc>`
/// (`action:snippet-leave`) stays a host-side action for now (it
/// migrates with `SnippetCompletionMode` in SN.3).
///
/// Replaces the old `push_layer` / `pop_layer` push mechanism
/// (MO.3). The host's `sync_keymap_overlays` calls
/// `activate_minor` / `deactivate_minor` by polling the shared
/// session's `is_active()` — when the next-placeholder handler
/// walks off `$0` and clears the session, the reconciler
/// deactivates this mode, dropping the Guard (and with it the two
/// `ActionHandlerRegistration` tokens). (That host poll is itself
/// a snippet-specific seam slated to move into this crate as a
/// typed session-lifecycle event — see the snippet activation
/// slice plan.)
pub struct SnippetActiveMode;

impl SnippetActiveMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("active-snippet-mode")
    }
}

/// RAII guard for [`SnippetActiveMode`]. Holds the two
/// `ActionHandlerRegistration` tokens for the `<Tab>` / `<S-Tab>`
/// handlers; dropping it (on mode deactivation) drops the tokens,
/// each of which unregisters its closure from the
/// `ActionHandlerRegistry` so the chord falls through to
/// "unhandled" once no snippet is live.
pub struct SnippetActiveModeGuard {
    _action_handler_registrations: Vec<ActionHandlerRegistration>,
}

/// Resolve the cursor effect for a newly-focused tabstop group:
/// move the cursor to the start of the group's first mirror range.
/// Mirrors the host's pre-SN.2b `move_cursor_to_snippet_group`
/// (`byte_to_position` against the active document's snapshot),
/// but returns an `Effect::SelectionChange` for the generic host
/// pipeline instead of writing `editor.cursor` directly. `None`
/// (no effect) when the group has no ranges or the byte offset
/// doesn't map to a position. Takes the snapshot `buffer` (not the
/// store) so the cursor math is unit-testable with a plain
/// [`lattice_core::Buffer`].
fn snippet_group_cursor_effect(buffer: &lattice_core::Buffer, group: &TabstopGroup) -> Option<Effect> {
    let first = group.ranges.first()?;
    let pos = buffer.byte_to_position(first.start).ok()?;
    Some(Effect::SelectionChange(SelectionSet::single(
        Selection::cursor(pos),
    )))
}

impl Mode for SnippetActiveMode {
    type Guard = SnippetActiveModeGuard;
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(snippet_active_keymap_entries())
    }
    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let mut registrations: Vec<ActionHandlerRegistration> = Vec::new();

            // Resolve the shared session + buffer-store +
            // command-registry + handler-registry services.
            // Tolerate them missing (unit-test harnesses that
            // activate the mode without full boot wiring): the
            // mode is still "active" for keymap-scoping, the
            // handlers just don't register and the chords fall
            // through. The session handle is required for the
            // handlers to do anything, so its absence skips
            // registration entirely.
            let session = ctx.service::<SnippetSessionHandle>().map(|a| (*a).clone());
            let buffer_store = ctx.service::<BufferStoreHandle>().map(|a| (*a).clone());

            if let (Some(session), Some(cmd_registry_arc), Some(action_handlers_arc)) = (
                session,
                ctx.service::<CommandRegistryHandle>(),
                ctx.service::<ActionHandlerRegistryHandle>(),
            ) {
                let cmd_registry = &**cmd_registry_arc;
                let action_handlers: ActionHandlerRegistryHandle =
                    (*action_handlers_arc).clone();

                // `<Tab>` — step to the next placeholder; clear the
                // session (ending it) on walk-off-`$0`.
                if let Some(id) = cmd_registry.id_by_name("action:snippet-next-placeholder") {
                    let session = session.clone();
                    let store = buffer_store.clone();
                    let handler: ActionHandler = Arc::new(
                        move |ctx: &ActionContext<'_>| -> Option<Effect> {
                            let group = session.with_mut(|s| {
                                let active = s.as_mut()?;
                                let next = active.next().cloned();
                                if next.is_none() {
                                    // Walked off `$0`: the session
                                    // ends. The host's overlay
                                    // reconciler sees `is_active()
                                    // == false` next cycle and
                                    // deactivates this mode.
                                    *s = None;
                                }
                                next
                            })?;
                            let store = store.as_ref()?;
                            let buffer_id = lattice_core::BufferId(ctx.buffer_id.raw() as u32);
                            let handle = store.handle_for(buffer_id)?;
                            snippet_group_cursor_effect(&handle.snapshot().buffer, &group)
                        },
                    );
                    registrations.push(action_handlers.register(id, handler));
                }

                // `<S-Tab>` — step to the previous placeholder.
                // Never ends the session (`prev()` past the first
                // group returns `None` → no-op).
                if let Some(id) = cmd_registry.id_by_name("action:snippet-prev-placeholder") {
                    let session = session.clone();
                    let store = buffer_store.clone();
                    let handler: ActionHandler = Arc::new(
                        move |ctx: &ActionContext<'_>| -> Option<Effect> {
                            let group = session
                                .with_mut(|s| s.as_mut().and_then(|a| a.prev().cloned()))?;
                            let store = store.as_ref()?;
                            let buffer_id = lattice_core::BufferId(ctx.buffer_id.raw() as u32);
                            let handle = store.handle_for(buffer_id)?;
                            snippet_group_cursor_effect(&handle.snapshot().buffer, &group)
                        },
                    );
                    registrations.push(action_handlers.register(id, handler));
                }
            }

            Ok(SnippetActiveModeGuard {
                _action_handler_registrations: registrations,
            })
        })
    }
}

/// Register `snippet-completion-mode` against `registry` using
/// `snippet_registry` as the contribution-side handle. The App
/// shares the same `Arc<SnippetRegistry>` with this mode so
/// source produce + host accept-path read the same data.
/// Returns the shared [`SnippetActivationPolicyHandle`] the
/// `snippet-mode` gate reads. The host stores a clone on `Editor`
/// and folds `snippet.activation` / `snippet.languages` into it at
/// boot + on `:set` (SN.3b); the default cell value is
/// `ActivationPolicy::Global`.
pub fn register_snippet_modes(
    registry: &mut ModeRegistry,
    snippet_registry: Arc<SharedSnippetRegistry>,
) -> SnippetActivationPolicyHandle {
    registry
        .register(SnippetCompletionMode {
            registry: snippet_registry,
        })
        .expect("snippet-completion-mode must register without conflict");
    registry
        .register(SnippetActiveMode)
        .expect("active-snippet-mode must register without conflict");
    // SN.3b: the `snippet-mode` gate. Reads a shared, host-folded
    // policy cell (default `Global`); `implies snippet-completion-mode`
    // so the source rides the gate. Registered AFTER its implied mode
    // so the implies dependency resolves (the registry validates the
    // implies tree at activation, but registering the dep first keeps
    // the ordering obvious).
    let policy: SnippetActivationPolicyHandle =
        Arc::new(ArcSwap::from_pointee(ActivationPolicy::Global));
    registry
        .register(SnippetMode::with_policy(policy.clone()))
        .expect("snippet-mode must register without conflict");
    policy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;
    use crate::registry::Snippet;
    use crate::ActiveSnippet;
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
            path_context: false,
            buffer_dir: None,
            uri: None,
            lsp_position: None,
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

    #[test]
    fn active_snippet_mode_id_is_active_snippet_mode() {
        assert_eq!(SnippetActiveMode::mode_id().as_str(), "active-snippet-mode");
        assert_eq!(SnippetActiveMode.id(), SnippetActiveMode::mode_id());
        assert_eq!(SnippetActiveMode.kind(), ModeKind::Minor);
    }

    #[test]
    fn active_snippet_mode_keymap_has_three_entries() {
        use lattice_mode::Mode as _;
        let km = SnippetActiveMode.keymap();
        assert_eq!(km.entries.len(), 3);
    }

    #[test]
    fn active_snippet_mode_keymap_entries_have_expected_commands() {
        use lattice_mode::Mode as _;
        let km = SnippetActiveMode.keymap();
        let cmds: Vec<_> = km.entries.iter().filter_map(|e| e.command).collect();
        assert!(cmds.contains(&"action:snippet-next-placeholder"));
        assert!(cmds.contains(&"action:snippet-prev-placeholder"));
        assert!(cmds.contains(&"action:snippet-leave"));
    }

    #[test]
    fn register_snippet_modes_registers_active_snippet_mode() {
        let mut registry = ModeRegistry::new();
        let snippet_registry = make_registry();
        register_snippet_modes(&mut registry, snippet_registry);
        assert!(registry.is_registered(SnippetActiveMode::mode_id()));
    }

    // ---- SN.3a: `snippet-mode` (the language-aware gate). ----

    #[test]
    fn snippet_mode_id_and_kind() {
        assert_eq!(SnippetMode::mode_id().as_str(), "snippet-mode");
        assert_eq!(SnippetMode::new().id(), SnippetMode::mode_id());
        assert_eq!(SnippetMode::new().kind(), ModeKind::Minor);
    }

    #[test]
    fn snippet_mode_default_policy_is_global() {
        // The default cell is Global (behavior-preserving) — also
        // the folded value for `snippet.activation = global`.
        assert_eq!(SnippetMode::new().activation_policy(), ActivationPolicy::Global);
    }

    #[test]
    fn snippet_mode_reads_live_policy_from_shared_cell() {
        // SN.3b: swapping the shared cell (as the host's boot /
        // `:set` re-fold does) is reflected on the next
        // `activation_policy()` read — no re-registration needed.
        let cell: SnippetActivationPolicyHandle =
            Arc::new(ArcSwap::from_pointee(ActivationPolicy::Global));
        let mode = SnippetMode::with_policy(cell.clone());
        assert_eq!(mode.activation_policy(), ActivationPolicy::Global);
        cell.store(Arc::new(ActivationPolicy::Majors(vec![ModeId::new("rust-mode")])));
        assert_eq!(
            mode.activation_policy(),
            ActivationPolicy::Majors(vec![ModeId::new("rust-mode")]),
        );
        cell.store(Arc::new(ActivationPolicy::Manual));
        assert_eq!(mode.activation_policy(), ActivationPolicy::Manual);
    }

    #[test]
    fn register_snippet_modes_returns_a_global_default_cell() {
        let mut registry = ModeRegistry::new();
        let policy = register_snippet_modes(&mut registry, make_registry());
        assert_eq!(**policy.load(), ActivationPolicy::Global);
    }

    #[test]
    fn snippet_mode_implies_the_completion_source_mode() {
        let mode = SnippetMode::new();
        assert_eq!(mode.implies(), &[SnippetCompletionMode::mode_id()][..]);
    }

    #[test]
    fn register_snippet_modes_registers_snippet_mode() {
        let mut registry = ModeRegistry::new();
        register_snippet_modes(&mut registry, make_registry());
        assert!(registry.is_registered(SnippetMode::mode_id()));
        // Its implied dependency is registered too, so activation
        // won't fail the implies-tree validation.
        assert!(registry.is_registered(SnippetCompletionMode::mode_id()));
    }

    // ---- SN.2b: `active-snippet-mode` placeholder-navigation
    //      handler bodies (relocated off the host). ----

    /// Pure unit test of the cursor-effect helper: a focused
    /// tabstop group whose first mirror range starts at byte 4 in
    /// `"for i in iter {}"` yields a `SelectionChange` whose
    /// primary head is `(line 0, byte 4)` — the `i`.
    #[test]
    fn cursor_effect_targets_first_range_start() {
        let buffer = Buffer::from_text("for i in iter {}");
        let group = TabstopGroup {
            index: 1,
            ranges: vec![4..5],
            has_default: true,
            is_choice: false,
        };
        match snippet_group_cursor_effect(&buffer, &group).expect("effect") {
            Effect::SelectionChange(set) => {
                assert_eq!(set.primary().head, Position::new(0, 4));
            }
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    /// A group with no ranges produces no cursor effect.
    #[test]
    fn cursor_effect_is_none_for_empty_group() {
        let buffer = Buffer::from_text("abc");
        let group = TabstopGroup {
            index: 0,
            ranges: vec![],
            has_default: false,
            is_choice: false,
        };
        assert!(snippet_group_cursor_effect(&buffer, &group).is_none());
    }

    /// Minimal `BufferStore` for the handler-dispatch test. The
    /// nav handlers advance the session *before* consulting the
    /// store, so a `None`-returning `handle_for` still exercises
    /// the full session state-machine (the migrated decision
    /// logic); only the cursor `Effect` short-circuits — which is
    /// covered separately by `cursor_effect_targets_first_range_start`.
    #[derive(Debug)]
    struct NullBufferStore;

    impl lattice_mode::BufferStore for NullBufferStore {
        fn find_by_name(&self, _name: &str) -> Option<lattice_core::BufferId> {
            None
        }
        fn ensure_named_document(
            &self,
            _name: &str,
            _major: ModeId,
            _flags: lattice_core::BufferFlags,
        ) -> lattice_core::BufferId {
            lattice_core::BufferId(0)
        }
        fn name_for(&self, _id: lattice_core::BufferId) -> Option<String> {
            None
        }
        fn handle_for(
            &self,
            _id: lattice_core::BufferId,
        ) -> Option<Arc<dyn lattice_runtime::Document>> {
            None
        }
        fn insert_document_buffer(
            &self,
            _id: lattice_core::BufferId,
            _kind: lattice_core::BufferKind,
            _handle: Arc<dyn lattice_runtime::Document>,
            _flags: lattice_core::BufferFlags,
            _name: Option<String>,
        ) {
        }
    }

    /// Build a `ServiceRegistry` wired the way the host boot does
    /// for the snippet surface: shared session, null buffer store,
    /// a command registry carrying the two nav action commands
    /// (so `id_by_name` resolves), and a fresh action-handler
    /// registry. Returns the registry plus the session +
    /// action-handler + command-id handles the test inspects.
    #[allow(clippy::type_complexity)]
    fn wire_services() -> (
        Arc<lattice_mode::ServiceRegistry>,
        SnippetSessionHandle,
        ActionHandlerRegistryHandle,
        lattice_protocol::ids::CommandId,
        lattice_protocol::ids::CommandId,
    ) {
        use lattice_grammar::{ActionSpec, CommandRegistry};

        let session: SnippetSessionHandle = Arc::new(crate::session::SnippetSession::new());
        let store: Arc<dyn lattice_mode::BufferStore> = Arc::new(NullBufferStore);
        let action_handlers: ActionHandlerRegistryHandle =
            Arc::new(lattice_mode::ActionHandlerRegistry::new());

        let mut cmd = CommandRegistry::new();
        let next_id = cmd.register_action(
            "action:snippet-next-placeholder",
            "next placeholder",
            ActionSpec {
                apply: Box::new(|_| Ok(Effect::None)),
                args_schema: vec![],
            },
        );
        let prev_id = cmd.register_action(
            "action:snippet-prev-placeholder",
            "prev placeholder",
            ActionSpec {
                apply: Box::new(|_| Ok(Effect::None)),
                args_schema: vec![],
            },
        );

        let mut services = lattice_mode::ServiceRegistry::new();
        services.register::<SnippetSessionHandle>(session.clone());
        services.register::<BufferStoreHandle>(BufferStoreHandle::new(store));
        services.register::<CommandRegistryHandle>(Arc::new(cmd));
        services.register::<ActionHandlerRegistryHandle>(action_handlers.clone());

        (Arc::new(services), session, action_handlers, next_id, prev_id)
    }

    /// Install a freshly-expanded `for ${1:i} in ${2:iter} { $0 }`
    /// session focused on `$1` (mirrors `Editor::expand_snippet`'s
    /// `focus_first` step), so `<Tab>` starts from `$1`.
    fn install_active_session(session: &SnippetSessionHandle) {
        let body = crate::parse("for ${1:i} in ${2:iter} { $0 }").expect("parse");
        let rendered = crate::render::render(&body, &crate::VariableContext::default());
        let mut active = ActiveSnippet::from_render(&rendered, 0);
        active.focus_first();
        session.set(active);
    }

    fn fire(
        handlers: &ActionHandlerRegistryHandle,
        id: lattice_protocol::ids::CommandId,
        services: &lattice_mode::ServiceRegistry,
    ) -> Option<Effect> {
        // The snippet handlers read only `ctx.buffer_id` (captured
        // session + store come from `on_activate`), so a throwaway
        // events bus + the wired services satisfy the context.
        let events = lattice_runtime::EventBus::new();
        let ctx = ActionContext {
            buffer_id: lattice_protocol::ids::BufferId::new(1),
            cursor: Position::new(0, 0),
            services,
            events: &events,
        };
        let handler = handlers.lookup(id).expect("handler registered");
        handler(&ctx)
    }

    fn activate_mode(services: Arc<lattice_mode::ServiceRegistry>) -> SnippetActiveModeGuard {
        let ctx = lattice_mode::ModeContext::new(
            lattice_protocol::ids::BufferId::new(1),
            SnippetActiveMode::mode_id(),
            Arc::new(lattice_config::ConfigRegistry::new()),
            Arc::new(lattice_runtime::EventBus::new()),
            services,
        );
        lattice_runtime::block_on(SnippetActiveMode.on_activate(ctx)).expect("activate")
    }

    /// `on_activate` registers exactly the two nav handlers, and
    /// dropping the Guard unregisters them.
    #[test]
    fn on_activate_registers_two_handlers_and_drop_unregisters() {
        let (services, _session, handlers, next_id, prev_id) = wire_services();
        let guard = activate_mode(services);
        assert_eq!(handlers.registered_count(), 2);
        assert!(handlers.lookup(next_id).is_some());
        assert!(handlers.lookup(prev_id).is_some());
        drop(guard);
        assert_eq!(handlers.registered_count(), 0);
        assert!(handlers.lookup(next_id).is_none());
    }

    /// `<Tab>` walks `$1 -> $2 -> $0`, then a fourth fire walks off
    /// `$0` and ends the session — the migrated
    /// `do_snippet_next_placeholder` behaviour.
    #[test]
    fn next_handler_walks_through_groups_and_drops_on_zero() {
        let (services, session, handlers, next_id, _prev_id) = wire_services();
        let _guard = activate_mode(services.clone());
        install_active_session(&session);

        let idx = |s: &SnippetSessionHandle| {
            s.with_mut(|o| o.as_ref().and_then(ActiveSnippet::current_index))
        };
        assert_eq!(idx(&session), Some(1)); // focused on $1
        fire(&handlers, next_id, &services);
        assert_eq!(idx(&session), Some(2));
        fire(&handlers, next_id, &services);
        assert_eq!(idx(&session), Some(0)); // $0 exit tabstop
        fire(&handlers, next_id, &services);
        assert!(!session.is_active()); // walked off $0 -> session ended
    }

    /// `<S-Tab>` walks back a placeholder and never ends the
    /// session — the migrated `do_snippet_prev_placeholder`.
    #[test]
    fn prev_handler_walks_back() {
        let (services, session, handlers, next_id, prev_id) = wire_services();
        let _guard = activate_mode(services.clone());
        install_active_session(&session);

        let idx = |s: &SnippetSessionHandle| {
            s.with_mut(|o| o.as_ref().and_then(ActiveSnippet::current_index))
        };
        fire(&handlers, next_id, &services);
        assert_eq!(idx(&session), Some(2));
        fire(&handlers, prev_id, &services);
        assert_eq!(idx(&session), Some(1));
        assert!(session.is_active());
    }

    /// With no live session the handlers are inert (no panic, no
    /// effect, session stays empty).
    #[test]
    fn handlers_are_noop_without_an_active_session() {
        let (services, session, handlers, next_id, prev_id) = wire_services();
        let _guard = activate_mode(services.clone());
        assert!(!session.is_active());
        assert!(fire(&handlers, next_id, &services).is_none());
        assert!(fire(&handlers, prev_id, &services).is_none());
        assert!(!session.is_active());
    }
}
