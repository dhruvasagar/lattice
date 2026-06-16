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
use lattice_grammar::{CommandRegistryHandle, Effect, ModalState, VisualKind};
use lattice_mode::{
    keymap_entry, ActionContext, ActionHandler, ActionHandlerContribution,
    ActionHandlerRegistration, ActionHandlerRegistryHandle, ActivationPolicy, BufferStoreHandle,
    CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    ModeRegistry,
};
use lattice_protocol::position::{Position, Range};
use lattice_protocol::selection::{Selection, SelectionSet, VisualMode};

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
    // SN.3g: `options()` override removed — it returned the `Mode`
    // trait default (`OptionOverrideSet::default()`), redundant noise.
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn completion_sources(&self) -> Vec<CompletionSourceContribution> {
        vec![CompletionSourceContribution {
            id: SourceId::new(SNIPPET_COMPLETION_SOURCE_ID),
            // SN.3g: single source with the option default so the two
            // can't drift (was a bare `150` literal duplicating
            // `completion.source.snippet.priority`'s default). The
            // option is `i64`; the contribution field is `u32`.
            default_priority: lattice_config::COMPLETION_SOURCE_SNIPPET_DEFAULT_PRIORITY as u32,
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

/// SN.3g: narrow a protocol `BufferId` (u64) to the core `BufferId`
/// (u32) the `BufferStore` is keyed by. Centralizes the unchecked
/// truncation — safe today (ids are small) but a footgun when inlined
/// at each call site.
fn core_buffer_id(id: lattice_protocol::ids::BufferId) -> lattice_core::BufferId {
    lattice_core::BufferId(id.raw() as u32)
}

/// SN.3c.1: word-byte predicate for the `<C-x><C-s>` trigger-token
/// scan. Mirrors the host's `is_word_char_byte` (`*` / `#` family)
/// — `[A-Za-z0-9_]`. Kept local so the mode's expand handler owns
/// its full scan logic without reaching into the host.
fn is_snippet_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// SN.3c.1: pure trigger-token scan. Given the cursor's line text
/// and the cursor position, return the `replace_range` covering the
/// word immediately before the cursor (`token-start..cursor`), or
/// `None` when there is no word prefix. Kept pure (no buffer store)
/// so the scan is unit-testable with a plain `&str` — mirrors how
/// `snippet_group_cursor_effect` is extracted for the nav handlers.
fn snippet_trigger_range(line_text: &str, cursor: Position) -> Option<Range> {
    let bytes = line_text.as_bytes();
    let cursor_byte = (cursor.byte as usize).min(bytes.len());
    let mut start = cursor_byte;
    while start > 0 && is_snippet_word_byte(bytes[start - 1]) {
        start -= 1;
    }
    if start == cursor_byte {
        return None;
    }
    Some(Range::new(Position::new(cursor.line, start as u32), cursor))
}

/// SN.3c.1: `snippet-mode`'s single Insert-mode binding —
/// `<C-x><C-s>` → `action:snippet-expand`. Migrated off the
/// Builtin Insert keymap (`lattice-host::keymap_insert`) so the
/// chord choice lives with the mode that owns the behavior
/// (`feedback_mode_owns_its_surface`). K.1.c scopes it to
/// `snippet-mode`-active buffers.
fn snippet_mode_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![keymap_entry!(
            mode: Insert,
            chord: "<C-x><C-s>",
            doc: "Expand the snippet whose prefix matches the word before the cursor.",
            cmd: "action:snippet-expand"
        )]
    })
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
    /// SN.3c.1: contribute the `<C-x><C-s>` direct-expand chord.
    /// Registered at boot under `KeymapLayer::MinorMode("snippet-mode")`
    /// (Insert mode); K.1.c gates it to `snippet-mode`-active buffers.
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(snippet_mode_keymap_entries())
    }
    /// SN.3c.1: the *global* (buffer-agnostic) expand handler. Bound to
    /// `action:snippet-expand`, registered ONCE at boot by the host's
    /// `register_mode_action_handlers` walk (NOT per-`on_activate` —
    /// `snippet-mode` is active on many buffers at once and the
    /// `ActionHandlerRegistry` is keyed by `CommandId` alone, so a
    /// per-activation registration would let one buffer closing evict
    /// the handler for all the others; see
    /// `feedback_effect_vocabulary_is_host_boundary`).
    ///
    /// The handler does ONLY the word-prefix scan: read the active
    /// buffer's line at `ctx.cursor` via the `BufferStoreHandle`, walk
    /// back over word bytes to the trigger token's start, and emit
    /// `Effect::ExpandSnippet { replace_range: token-start..cursor }`.
    /// The host owns resolution + expansion (language + registry +
    /// variables + splice). Returns `None` (no effect) when there is no
    /// word prefix at the cursor.
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        let handler: ActionHandler = Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
            let store = ctx.services.get::<BufferStoreHandle>()?;
            let buffer_id = core_buffer_id(ctx.buffer_id);
            let handle = store.handle_for(buffer_id)?;
            let line_text = handle.snapshot().buffer.line(ctx.cursor.line).unwrap_or_default();
            // No word prefix → `None` (no effect); otherwise hand the
            // host the trigger range to resolve + expand.
            snippet_trigger_range(&line_text, ctx.cursor)
                .map(|replace_range| Effect::ExpandSnippet { replace_range })
        });
        vec![ActionHandlerContribution {
            action_name: "action:snippet-expand",
            handler,
        }]
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
            // SN.3c.2b: `fall_through: true` — leaving a snippet is
            // augment-and-continue, not replace. The handler clears the
            // session, then the dispatcher continues to whatever `<Esc>`
            // natively does (builtin → exit insert, or the user's
            // rebind). The mode never hardcodes the native meaning.
            keymap_entry!(
                mode: Insert,
                chord: "<Esc>",
                doc: "Leave the active snippet session, then exit insert (falls through to the native <Esc>).",
                cmd: "action:snippet-leave",
                fall_through: true
            ),
            // SN.3d.4: the SAME three bindings in Select mode. A
            // placeholder with a non-empty default is focused in
            // charwise Select (so the next printable overtypes it —
            // `snippet_group_cursor_effect`), and these keep
            // navigation + leave live there too. The host's Select
            // dispatch (`keymap_select::translate_select`) consults
            // active minor-mode layers exactly as Insert does, so a
            // mode that selects a span owns its full chord surface in
            // BOTH modes — no half-migration. `<Esc>` stays
            // `fall_through`: the leave handler clears the session and
            // the dispatcher continues to the native Select `<Esc>`
            // (`ExitSelect` → Normal).
            keymap_entry!(
                mode: Select,
                chord: "<Tab>",
                doc: "Move to the next placeholder in the active snippet session.",
                cmd: "action:snippet-next-placeholder"
            ),
            keymap_entry!(
                mode: Select,
                chord: "<S-Tab>",
                doc: "Move to the previous placeholder in the active snippet session.",
                cmd: "action:snippet-prev-placeholder"
            ),
            keymap_entry!(
                mode: Select,
                chord: "<Esc>",
                doc: "Leave the active snippet session, then exit Select (falls through to the native <Esc>).",
                cmd: "action:snippet-leave",
                fall_through: true
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
/// service and returning a cursor effect through the host's generic
/// effect pipeline (see `snippet_group_cursor_effect`).
///
/// **SN.3d.3 (2026-06-15):** that cursor effect now consumes Select
/// mode. A placeholder with a non-empty default returns
/// `Effect::Many([EnterMode(Select(Charwise)), SelectionChange(span)])`
/// so the default is SELECTED and the next printable key overtypes the
/// whole thing (then drops to Insert). An empty tabstop still returns a
/// bare `Effect::SelectionChange` cursor. The host's initial-expand
/// focus (`Editor::expand_snippet`) mirrors this directly.
///
/// **SN.3c.2 (2026-06-14):** `<Esc>` (`action:snippet-leave`) now
/// owns its body here too — a third per-buffer handler that clears
/// the shared session and returns `Effect::EnterMode(Normal)`. The
/// old `Editor::dispatch` `Action::SnippetLeave` arm (session clear +
/// modal flip) + `Action::SnippetLeave` / `AppEffect::SnippetLeave`
/// are gone (`feedback_mode_owns_its_surface`).
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

/// RAII guard for [`SnippetActiveMode`]. Holds the
/// `ActionHandlerRegistration` tokens for the `<Tab>` / `<S-Tab>` /
/// `<Esc>` handlers (SN.3c.2 added `<Esc>`); dropping it (on mode
/// deactivation) drops the tokens, each of which unregisters its
/// closure from the `ActionHandlerRegistry` so the chord falls
/// through to "unhandled" once no snippet is live.
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
    let start = buffer.byte_to_position(first.start).ok()?;
    if first.end > first.start {
        // SN.3d.3: a non-empty placeholder default is SELECTED so the
        // next printable key overtypes the whole default in one keystroke
        // (the edit then ripples to the group's mirrors through the
        // existing tabstop tracking). Charwise Select with the head on the
        // placeholder's LAST byte (inclusive-head convention) makes
        // `selection_extent` span exactly `[start, end)`. `EnterMode`
        // FIRST: the host's `Effect::SelectionChange` arm only adopts a
        // span (and sets `visual_anchor`) once modal is Visual/Select, so
        // the mode flip must land before the selection.
        let head = buffer.byte_to_position(first.end - 1).ok()?;
        let sel = Selection {
            anchor: start,
            head,
            visual: Some(VisualMode::Charwise),
        };
        Some(Effect::Many(vec![
            Effect::EnterMode(ModalState::Select(VisualKind::Charwise)),
            Effect::SelectionChange(SelectionSet::single(sel)),
        ]))
    } else {
        // Empty tabstop (`$1` / `${1:}`): nothing to overtype, so keep the
        // bare Insert cursor — do NOT enter Select on a zero-width stop.
        Some(Effect::SelectionChange(SelectionSet::single(
            Selection::cursor(start),
        )))
    }
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
                            let buffer_id = core_buffer_id(ctx.buffer_id);
                            let group = session.with_mut(buffer_id, |s| {
                                let active = s.as_mut()?;
                                let next = active.next().cloned();
                                if next.is_none() {
                                    // Walked off `$0`: the session
                                    // ends for this buffer. The host's
                                    // overlay reconciler sees
                                    // `is_active(buffer) == false` next
                                    // cycle and deactivates this mode.
                                    *s = None;
                                }
                                next
                            })?;
                            let store = store.as_ref()?;
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
                            let buffer_id = core_buffer_id(ctx.buffer_id);
                            let group = session
                                .with_mut(buffer_id, |s| s.as_mut().and_then(|a| a.prev().cloned()))?;
                            let store = store.as_ref()?;
                            let handle = store.handle_for(buffer_id)?;
                            snippet_group_cursor_effect(&handle.snapshot().buffer, &group)
                        },
                    );
                    registrations.push(action_handlers.register(id, handler));
                }

                // `<Esc>` — leave the snippet session (SN.3c.2).
                // Per-buffer + session-tied (the binding only fires
                // while a snippet is live on this buffer), so it lives
                // here in `on_activate` alongside the nav handlers, not
                // as a global handler. The body does ONLY the
                // mode-specific part — clear the shared session (the
                // host's overlay reconciler then sees
                // `is_active() == false` and deactivates this mode,
                // dropping these registrations). Exiting insert is NOT
                // hardcoded here: SN.3c.2b marks the `<Esc>` binding
                // `fall_through: true`, so after this handler runs the
                // dispatcher re-resolves `<Esc>` against the layers
                // below and runs the native binding too (builtin
                // `<Esc>` → exit insert, or the user's rebind). The mode
                // owns the augmentation; the host owns the native
                // meaning. No buffer store needed.
                if let Some(id) = cmd_registry.id_by_name("action:snippet-leave") {
                    let session = session.clone();
                    let handler: ActionHandler = Arc::new(
                        move |ctx: &ActionContext<'_>| -> Option<Effect> {
                            session.clear(core_buffer_id(ctx.buffer_id));
                            Some(Effect::None)
                        },
                    );
                    registrations.push(action_handlers.register(id, handler));
                }
            } else {
                // SN.3f: the handlers tolerate missing services (unit
                // harnesses that activate the mode without full boot
                // wiring), but a *production* boot that reaches here has
                // a mis-wired ServiceRegistry and the snippet chords
                // will dead — surface it. `debug!` not `info!` per
                // `feedback_log_levels` (per-activation, opt-in via
                // `--log-level debug`).
                tracing::debug!(
                    target: "lattice_snippet::modes",
                    "active-snippet-mode: nav/leave handlers not registered — \
                     SnippetSession / CommandRegistry / ActionHandlerRegistry \
                     service absent; <Tab>/<S-Tab>/<Esc> will fall through"
                );
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
    fn active_snippet_mode_keymap_has_insert_and_select_entries() {
        use lattice_mode::BindingMode;
        use lattice_mode::Mode as _;
        let km = SnippetActiveMode.keymap();
        // SN.3d.4: <Tab> / <S-Tab> / <Esc> in BOTH Insert and Select
        // (a default-bearing placeholder is focused in Select).
        assert_eq!(km.entries.len(), 6);
        let select_count = km
            .entries
            .iter()
            .filter(|e| e.modes.contains(&BindingMode::Select))
            .count();
        let insert_count = km
            .entries
            .iter()
            .filter(|e| e.modes.contains(&BindingMode::Insert))
            .count();
        assert_eq!(insert_count, 3, "three Insert bindings");
        assert_eq!(select_count, 3, "three Select bindings");
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

    // ---- SN.3c.1: `snippet-mode` owns the `<C-x><C-s>` expand
    //      trigger (chord + word-prefix scan; host owns the
    //      resolution + expansion). ----

    #[test]
    fn snippet_mode_keymap_binds_ctrl_x_ctrl_s_to_expand() {
        use lattice_mode::Mode as _;
        let km = SnippetMode::new().keymap();
        assert_eq!(km.entries.len(), 1);
        let entry = &km.entries[0];
        assert_eq!(entry.chord, "<C-x><C-s>");
        assert_eq!(entry.command, Some("action:snippet-expand"));
        assert_eq!(entry.modes, [lattice_mode::BindingMode::Insert].as_slice());
    }

    #[test]
    fn snippet_mode_contributes_one_global_expand_handler() {
        use lattice_mode::Mode as _;
        let handlers = SnippetMode::new().action_handlers();
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0].action_name, "action:snippet-expand");
    }

    #[test]
    fn trigger_range_covers_word_before_cursor() {
        // "for" with the cursor after the `r` → replace_range
        // (0,0)..(0,3) covering the whole token.
        let r = snippet_trigger_range("for", Position::new(0, 3)).expect("word prefix");
        assert_eq!(r, Range::new(Position::new(0, 0), Position::new(0, 3)));
    }

    #[test]
    fn trigger_range_starts_at_word_boundary_not_line_start() {
        // "let for" cursor after the second word → token starts at
        // byte 4 (`f`), not the line start.
        let r = snippet_trigger_range("let for", Position::new(0, 7)).expect("word prefix");
        assert_eq!(r, Range::new(Position::new(0, 4), Position::new(0, 7)));
    }

    #[test]
    fn trigger_range_is_none_without_a_word_prefix() {
        // Cursor at column 0 → nothing before it.
        assert!(snippet_trigger_range("for", Position::new(0, 0)).is_none());
        // Cursor right after whitespace → no word byte behind it.
        assert!(snippet_trigger_range("a ", Position::new(0, 2)).is_none());
    }

    /// Graceful: when the buffer store can't resolve the active
    /// buffer (`handle_for` returns `None`), the expand handler is a
    /// quiet no-op rather than a panic.
    #[test]
    fn expand_handler_is_a_no_op_when_buffer_unavailable() {
        use lattice_mode::Mode as _;
        let handler = SnippetMode::new().action_handlers().remove(0).handler;
        let store: Arc<dyn lattice_mode::BufferStore> = Arc::new(NullBufferStore);
        let mut services = lattice_mode::ServiceRegistry::new();
        services.register::<BufferStoreHandle>(BufferStoreHandle::new(store));
        let events = lattice_runtime::EventBus::new();
        let ctx = ActionContext {
            buffer_id: lattice_protocol::ids::BufferId::new(1),
            cursor: Position::new(0, 3),
            services: &services,
            events: &events,
        };
        assert!(handler(&ctx).is_none());
    }

    // ---- SN.2b: `active-snippet-mode` placeholder-navigation
    //      handler bodies (relocated off the host). ----

    /// SN.3d.3: a focused tabstop with a non-empty default (`"iter"`
    /// at bytes 9..13 in `"for i in iter {}"`) SELECTS the default and
    /// enters Select so a printable key overtypes it. `EnterMode` is
    /// emitted before the `SelectionChange`. The charwise selection's
    /// head sits on the LAST byte of the default (12), so the host's
    /// `selection_extent` spans exactly `[9, 13)`.
    #[test]
    fn cursor_effect_selects_non_empty_default_and_enters_select() {
        let buffer = Buffer::from_text("for i in iter {}");
        let group = TabstopGroup {
            index: 1,
            ranges: vec![9..13],
            has_default: true,
            is_choice: false,
        };
        match snippet_group_cursor_effect(&buffer, &group).expect("effect") {
            Effect::Many(effects) => {
                assert!(
                    matches!(
                        effects.first(),
                        Some(Effect::EnterMode(ModalState::Select(VisualKind::Charwise)))
                    ),
                    "must enter Select FIRST so the selection arm adopts the span"
                );
                match effects.get(1) {
                    Some(Effect::SelectionChange(set)) => {
                        let p = set.primary();
                        assert_eq!(p.anchor, Position::new(0, 9), "anchor = default start");
                        assert_eq!(p.head, Position::new(0, 12), "head = default's last byte");
                    }
                    other => panic!("expected SelectionChange second, got {other:?}"),
                }
            }
            other => panic!("expected Effect::Many, got {other:?}"),
        }
    }

    /// SN.3d.3: an EMPTY tabstop (`$1`, zero-width range) keeps the bare
    /// Insert cursor — no Select, nothing to overtype.
    #[test]
    fn cursor_effect_empty_tabstop_keeps_bare_cursor() {
        let buffer = Buffer::from_text("for i in iter {}");
        let group = TabstopGroup {
            index: 1,
            ranges: vec![4..4],
            has_default: false,
            is_choice: false,
        };
        match snippet_group_cursor_effect(&buffer, &group).expect("effect") {
            Effect::SelectionChange(set) => {
                let p = set.primary();
                assert_eq!(p.anchor, p.head, "bare cursor: zero-width");
                assert_eq!(p.head, Position::new(0, 4));
            }
            other => panic!("expected a bare SelectionChange, got {other:?}"),
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
        // SN.3c.2: the `<Esc>` leave handler keys on this id.
        let leave_id = cmd.register_action(
            "action:snippet-leave",
            "leave snippet",
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

        (
            Arc::new(services),
            session,
            action_handlers,
            next_id,
            prev_id,
            leave_id,
        )
    }

    /// Install a freshly-expanded `for ${1:i} in ${2:iter} { $0 }`
    /// session focused on `$1` (mirrors `Editor::expand_snippet`'s
    /// `focus_first` step), so `<Tab>` starts from `$1`.
    fn install_active_session(session: &SnippetSessionHandle) {
        // Keyed under the same buffer `fire`'s `ActionContext` uses
        // (`BufferId::new(1)` → `core_buffer_id` → core `BufferId(1)`),
        // so the handlers and the test reads agree on the buffer.
        install_active_session_in(session, lattice_core::BufferId(1));
    }

    /// Install a fresh `for ${1:i} in ${2:iter} { $0 }` session in
    /// `buffer`, focused on `$1` (SN.3e multi-buffer tests).
    fn install_active_session_in(
        session: &SnippetSessionHandle,
        buffer: lattice_core::BufferId,
    ) {
        let body = crate::parse("for ${1:i} in ${2:iter} { $0 }").expect("parse");
        let rendered = crate::render::render(&body, &crate::VariableContext::default());
        let mut active = ActiveSnippet::from_render(&rendered, 0);
        active.focus_first();
        session.set(buffer, active);
    }

    fn fire(
        handlers: &ActionHandlerRegistryHandle,
        id: lattice_protocol::ids::CommandId,
        services: &lattice_mode::ServiceRegistry,
    ) -> Option<Effect> {
        fire_in(handlers, id, services, lattice_protocol::ids::BufferId::new(1))
    }

    /// As [`fire`], but the synthetic `ActionContext` reports `buffer`
    /// — so a handler keys its session lookup on that buffer (SN.3e).
    fn fire_in(
        handlers: &ActionHandlerRegistryHandle,
        id: lattice_protocol::ids::CommandId,
        services: &lattice_mode::ServiceRegistry,
        buffer: lattice_protocol::ids::BufferId,
    ) -> Option<Effect> {
        // The snippet handlers read only `ctx.buffer_id` (captured
        // session + store come from `on_activate`), so a throwaway
        // events bus + the wired services satisfy the context.
        let events = lattice_runtime::EventBus::new();
        let ctx = ActionContext {
            buffer_id: buffer,
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

    /// `on_activate` registers exactly the three handlers (`<Tab>` /
    /// `<S-Tab>` nav + SN.3c.2's `<Esc>` leave), and dropping the
    /// Guard unregisters them all.
    #[test]
    fn on_activate_registers_three_handlers_and_drop_unregisters() {
        let (services, _session, handlers, next_id, prev_id, leave_id) = wire_services();
        let guard = activate_mode(services);
        assert_eq!(handlers.registered_count(), 3);
        assert!(handlers.lookup(next_id).is_some());
        assert!(handlers.lookup(prev_id).is_some());
        assert!(handlers.lookup(leave_id).is_some());
        drop(guard);
        assert_eq!(handlers.registered_count(), 0);
        assert!(handlers.lookup(next_id).is_none());
        assert!(handlers.lookup(leave_id).is_none());
    }

    /// SN.3f: when a required service is absent (here:
    /// `CommandRegistryHandle`), `on_activate` skips handler
    /// registration entirely — the mode still activates (guard
    /// returned, keymap-scoping works) but no handlers land, and it
    /// logs a `debug!` on the skip path. Asserts the skip via the
    /// (present) action-handler registry staying empty.
    #[test]
    fn on_activate_skips_registration_when_a_service_is_absent() {
        let session: SnippetSessionHandle = Arc::new(crate::session::SnippetSession::new());
        let action_handlers: ActionHandlerRegistryHandle =
            Arc::new(lattice_mode::ActionHandlerRegistry::new());
        let mut services = lattice_mode::ServiceRegistry::new();
        services.register::<SnippetSessionHandle>(session);
        services.register::<ActionHandlerRegistryHandle>(action_handlers.clone());
        // No `CommandRegistryHandle` registered → the `if let` fails →
        // the skip branch runs.
        let _guard = activate_mode(Arc::new(services));
        assert_eq!(action_handlers.registered_count(), 0);
    }

    /// `<Tab>` walks `$1 -> $2 -> $0`, then a fourth fire walks off
    /// `$0` and ends the session — the migrated
    /// `do_snippet_next_placeholder` behaviour.
    #[test]
    fn next_handler_walks_through_groups_and_drops_on_zero() {
        let (services, session, handlers, next_id, _prev_id, _leave_id) = wire_services();
        let _guard = activate_mode(services.clone());
        install_active_session(&session);

        let idx = |s: &SnippetSessionHandle| {
            s.with_mut(lattice_core::BufferId(1), |o| o.as_ref().and_then(ActiveSnippet::current_index))
        };
        assert_eq!(idx(&session), Some(1)); // focused on $1
        fire(&handlers, next_id, &services);
        assert_eq!(idx(&session), Some(2));
        fire(&handlers, next_id, &services);
        assert_eq!(idx(&session), Some(0)); // $0 exit tabstop
        fire(&handlers, next_id, &services);
        assert!(!session.is_active(lattice_core::BufferId(1))); // walked off $0 -> session ended
    }

    /// `<S-Tab>` walks back a placeholder and never ends the
    /// session — the migrated `do_snippet_prev_placeholder`.
    #[test]
    fn prev_handler_walks_back() {
        let (services, session, handlers, next_id, prev_id, _leave_id) = wire_services();
        let _guard = activate_mode(services.clone());
        install_active_session(&session);

        let idx = |s: &SnippetSessionHandle| {
            s.with_mut(lattice_core::BufferId(1), |o| o.as_ref().and_then(ActiveSnippet::current_index))
        };
        fire(&handlers, next_id, &services);
        assert_eq!(idx(&session), Some(2));
        fire(&handlers, prev_id, &services);
        assert_eq!(idx(&session), Some(1));
        assert!(session.is_active(lattice_core::BufferId(1)));
    }

    /// With no live session the handlers are inert (no panic, no
    /// effect, session stays empty).
    #[test]
    fn handlers_are_noop_without_an_active_session() {
        let (services, session, handlers, next_id, prev_id, _leave_id) = wire_services();
        let _guard = activate_mode(services.clone());
        assert!(!session.is_active(lattice_core::BufferId(1)));
        assert!(fire(&handlers, next_id, &services).is_none());
        assert!(fire(&handlers, prev_id, &services).is_none());
        assert!(!session.is_active(lattice_core::BufferId(1)));
    }

    /// SN.3c.2b: `<Esc>` (leave) clears the live session and returns
    /// `Effect::None` — the *only* job of the mode handler. Exiting
    /// insert is not its concern: the `<Esc>` binding is
    /// `fall_through: true`, so the dispatcher continues to the native
    /// `<Esc>` after this handler runs (covered by the host-level
    /// `dispatch_insert` fall-through test). The host's overlay
    /// reconciler sees `is_active() == false` next cycle and
    /// deactivates the mode.
    #[test]
    fn leave_handler_clears_session() {
        let (services, session, handlers, _next_id, _prev_id, leave_id) = wire_services();
        let _guard = activate_mode(services.clone());
        install_active_session(&session);
        assert!(session.is_active(lattice_core::BufferId(1)));

        let effect = fire(&handlers, leave_id, &services);
        assert!(matches!(effect, Some(Effect::None)));
        assert!(!session.is_active(lattice_core::BufferId(1))); // session cleared
    }

    /// SN.3e: two buffers each carry their own live session. `<Tab>`
    /// fired in buffer A advances A's tabstops only — B's session is
    /// untouched, and clearing one leaves the other live. Before SN.3e
    /// the single global slot misrouted this: starting a snippet in A
    /// then acting in B drove `<Tab>` against A's tabstops.
    #[test]
    fn sessions_are_isolated_per_buffer() {
        use lattice_core::BufferId as Core;
        use lattice_protocol::ids::BufferId as Proto;

        let (services, session, handlers, next_id, _prev_id, _leave_id) = wire_services();
        let _guard = activate_mode(services.clone());

        // Two buffers, two independent sessions, both focused on `$1`.
        install_active_session_in(&session, Core(1));
        install_active_session_in(&session, Core(2));

        let idx = |s: &SnippetSessionHandle, b: Core| {
            s.with_mut(b, |o| o.as_ref().and_then(ActiveSnippet::current_index))
        };
        assert_eq!(idx(&session, Core(1)), Some(1));
        assert_eq!(idx(&session, Core(2)), Some(1));

        // Advance A (`<Tab>` with `ctx.buffer_id` = proto 1). The
        // handler keys `with_mut` on core 1; B (core 2) is untouched.
        fire_in(&handlers, next_id, &services, Proto::new(1));
        assert_eq!(idx(&session, Core(1)), Some(2), "A advanced");
        assert_eq!(idx(&session, Core(2)), Some(1), "B never moved");

        // Switch to B and advance — A stays put.
        fire_in(&handlers, next_id, &services, Proto::new(2));
        assert_eq!(idx(&session, Core(2)), Some(2), "B advanced");
        assert_eq!(idx(&session, Core(1)), Some(2), "A unchanged");

        // Clearing A leaves B's session live.
        session.clear(Core(1));
        assert!(!session.is_active(Core(1)));
        assert!(session.is_active(Core(2)));
    }
}
