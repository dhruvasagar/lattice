//! M.6 (2026-06-01): SearchProvider — the worked
//! `MultibufferProvider` example.
//!
//! Architecture: `multibuffer-views.md` §3.7 "Worked example —
//! SearchProvider end-to-end". User runs `:search "TODO"`; the
//! provider:
//!
//! 1. Pulls the `ProjectSearchService` handle from
//!    `activator.services()`.
//! 2. Opens an empty multibuffer view via
//!    `create_multibuffer_view`; the view auto-subscribes to
//!    source events via M.4.
//! 3. Seeds initial provider state (query, options, scanning).
//! 4. Sets the headerline to `InProgress { label: "Searching" }`.
//! 5. Activates `ProjectSearchMultibufferMode` (this minor's
//!    `on_activate` subscribes to `ProjectSearchBatchReady` +
//!    `ProjectSearchCompleted` and forwards them into
//!    `MultibufferDocumentHandle::append_excerpts` /
//!    `set_headerline`).
//! 6. Spawns the async scan task via the service.
//!
//! The scan task runs on tokio worker threads (never the UI
//! thread, per paramount goal #1) — walks the project tree via
//! `ignore::Walk`, matches literal queries against each file,
//! batches hits, publishes `ProjectSearchBatchReady` events.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use lattice_config::OptionOverrideSet;
use lattice_core::{BufferFlags, BufferId};
use lattice_grammar::{CommandRegistry, CommandRegistryHandle};
use lattice_mode::{
    ActionContext, ActionHandlerRegistration, ActionHandlerRegistryHandle, CapabilitySet, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeActivator, ModeContext, ModeId, ModeKind,
    ModeRegistry, ServiceRegistry, keymap_entry,
};
use lattice_runtime::{Document, EventBus, spawn_document};
use tokio::sync::mpsc;

use crate::registry::MultibufferRegistryHandle;
use crate::view::create_multibuffer_view;
use crate::{Excerpt, ExcerptHeader, HeaderlineStatus};

// ─────────────────────────────────────────────────────────────────
// Public state types
// ─────────────────────────────────────────────────────────────────

// ─────────────────────────────────────────────────────────────────
// Typed options (defined by ProjectSearchMultibufferMode)
// K.4.6 follow-up (2026-06-02): per
// [[feedback_mode_owns_its_surface]] the project-search minor
// mode owns its options. Bound to the `search` group registered
// in `lattice-config::group`; user customizes via
// `:set search.context_size=3` or `:customize search`.
// ─────────────────────────────────────────────────────────────────

lattice_config::options! {
    group = lattice_config::Search;

    /// Number of context lines to show above and below each
    /// matched line in `:search` results. Default `0` (no
    /// context — only matched lines appear, matching the
    /// "1 line per hit, 1 header per file" UX).
    ///
    /// Mirrors grep's `-C N` convention
    /// ([[feedback_convention_first]]): `:set search.context_size=3`
    /// yields ±3 lines of context per hit, with adjacent
    /// clusters merged when ranges overlap or touch — the
    /// same shape grep / ripgrep / ag produce.
    ///
    /// The substrate's `compose_header_rows` dedupes consecutive
    /// same-source excerpts to a single header regardless of
    /// `context_size`, so increasing this widens the windows
    /// under each file's header rather than adding new headers.
    #[name("search.context_size")]
    pub SearchContextSize: i64 = 0;
}

/// User-supplied scan parameters.
#[derive(Debug, Clone)]
pub struct ProjectSearchOptions {
    /// Project root the scan walks. Defaults to CWD when the
    /// trigger function isn't given an explicit one.
    pub root: PathBuf,
    /// Whether matches are case-sensitive.
    pub case_sensitive: bool,
    /// Cap on the number of files scanned. `None` = unlimited
    /// (defaults to large; the scan respects `.gitignore`).
    pub max_files: Option<usize>,
    /// Cap on hits per file before moving on (per-file
    /// hit-limit; prevents one huge file from monopolising the
    /// batch budget).
    pub max_hits_per_file: usize,
    /// M.6.3 (2026-06-01): interpret `query` as a `fancy-regex`
    /// pattern instead of a literal substring. `false` (default)
    /// keeps the M.6.0 literal-match path for back-compat.
    /// Case-sensitivity layered on via an injected `(?i)` flag
    /// when `case_sensitive == false`.
    pub regex: bool,
    /// K.4.6 follow-up (2026-06-02): number of context lines to
    /// show above and below each matched line. Resolved from
    /// the `search.context_size` typed option at `:search`
    /// dispatch time. `0` = no context (one excerpt per match
    /// row); `N > 0` = ±N lines around each match, with
    /// adjacent clusters merged when ranges overlap or touch.
    pub context_lines: u32,
}

impl Default for ProjectSearchOptions {
    fn default() -> Self {
        Self {
            root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            case_sensitive: false,
            max_files: None,
            max_hits_per_file: 100,
            regex: false,
            context_lines: 0,
        }
    }
}

/// One file's hits. Returned in `ProjectSearchBatchReady` batches.
#[derive(Debug, Clone)]
pub struct FileHits {
    pub path: PathBuf,
    /// Each hit's 0-based source row. Sorted ascending.
    pub rows: Vec<u32>,
}

/// Status of a scan task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchStatus {
    Scanning,
    Done { total_hits: usize },
    Failed { reason: String },
}

/// Per-view state owned by `ProjectSearchService`.
#[derive(Debug)]
pub struct ProjectSearchState {
    pub query: String,
    pub options: ProjectSearchOptions,
    pub status: SearchStatus,
    pub total_hits: usize,
    pub scan_task: Option<tokio::task::JoinHandle<()>>,
    /// M.6.6 (2026-06-08): cooperative cancellation flag. Set to `true`
    /// by the refresh handler before spawning a replacement task; the
    /// blocking scan loop checks this at each file iteration and exits
    /// early when it fires. `spawn_blocking` tasks ignore `JoinHandle`
    /// abort — only this flag reaches the blocking thread.
    pub cancel_token: Arc<AtomicBool>,
    /// M.6.1: source `BufferId` → on-disk path. Populated by the
    /// provider-minor's forwarder as it loads files into the
    /// view's source map. The mode's `<CR>` handler (M.10.3,
    /// registered via `ActionHandlerRegistry` from
    /// `on_activate`) reads this to resolve the excerpt under
    /// cursor back to a file path.
    pub source_paths: std::collections::HashMap<BufferId, PathBuf>,
}

impl ProjectSearchState {
    pub fn scanning(query: String, options: ProjectSearchOptions) -> Self {
        Self {
            query,
            options,
            status: SearchStatus::Scanning,
            total_hits: 0,
            scan_task: None,
            source_paths: std::collections::HashMap::new(),
            cancel_token: Arc::new(AtomicBool::new(false)),
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// Service trait + InMemory impl
// ─────────────────────────────────────────────────────────────────

/// Per-view provider state lookup. Registered in
/// `ServiceRegistry` at boot; minor mode + scan-task glue go
/// through this trait.
pub trait ProjectSearchService: Send + Sync + std::fmt::Debug {
    fn state(&self, view: BufferId) -> Option<Arc<RwLock<ProjectSearchState>>>;
    fn set_state(&self, view: BufferId, state: ProjectSearchState);
    fn clear(&self, view: BufferId);
    fn attach_task(&self, view: BufferId, handle: tokio::task::JoinHandle<()>);
    /// Update the running tally of hits as a scan progresses.
    fn add_hits(&self, view: BufferId, add: usize);
    fn set_status(&self, view: BufferId, status: SearchStatus);
    fn len(&self) -> usize;
    /// M.6.1: record the on-disk path for a source buffer the
    /// forwarder just attached to the view. Jump-to-source reads
    /// this map back to resolve excerpt → path.
    fn record_source_path(&self, view: BufferId, source: BufferId, path: PathBuf);
    /// M.6.1: look up the path for a source buffer (used by
    /// jump-to-source after finding the excerpt under cursor).
    fn source_path(&self, view: BufferId, source: BufferId) -> Option<PathBuf>;
    /// M.6.2 (2026-06-01): reverse lookup — find an existing
    /// source buffer for a path inside a view. Forwarder uses
    /// this to dedup file loads across batches: a file with
    /// hits split across two batches reuses the same source
    /// buffer instead of spawning a second `RopeDocumentHandle`.
    fn find_source_for_path(&self, view: BufferId, path: &Path) -> Option<BufferId>;
}

pub type ProjectSearchServiceHandle = Arc<dyn ProjectSearchService>;

/// Default in-memory `ProjectSearchService`.
#[derive(Debug, Default)]
pub struct InMemoryProjectSearchService {
    inner: RwLock<std::collections::HashMap<BufferId, Arc<RwLock<ProjectSearchState>>>>,
}

impl InMemoryProjectSearchService {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn handle() -> ProjectSearchServiceHandle {
        Arc::new(Self::new())
    }
}

impl ProjectSearchService for InMemoryProjectSearchService {
    fn state(&self, view: BufferId) -> Option<Arc<RwLock<ProjectSearchState>>> {
        self.inner.read().ok()?.get(&view).cloned()
    }
    fn set_state(&self, view: BufferId, state: ProjectSearchState) {
        if let Ok(mut map) = self.inner.write() {
            map.insert(view, Arc::new(RwLock::new(state)));
        }
    }
    fn clear(&self, view: BufferId) {
        if let Ok(mut map) = self.inner.write() {
            if let Some(state) = map.remove(&view) {
                if let Ok(mut s) = state.write() {
                    if let Some(h) = s.scan_task.take() {
                        h.abort();
                    }
                }
            }
        }
    }
    fn attach_task(&self, view: BufferId, handle: tokio::task::JoinHandle<()>) {
        let Some(state) = self.state(view) else {
            handle.abort();
            return;
        };
        if let Ok(mut s) = state.write() {
            if let Some(prior) = s.scan_task.take() {
                prior.abort();
            }
            s.scan_task = Some(handle);
        }
    }
    fn add_hits(&self, view: BufferId, add: usize) {
        if let Some(state) = self.state(view) {
            if let Ok(mut s) = state.write() {
                s.total_hits = s.total_hits.saturating_add(add);
            }
        }
    }
    fn set_status(&self, view: BufferId, status: SearchStatus) {
        if let Some(state) = self.state(view) {
            if let Ok(mut s) = state.write() {
                s.status = status;
            }
        }
    }
    fn len(&self) -> usize {
        self.inner.read().map(|m| m.len()).unwrap_or(0)
    }
    fn record_source_path(&self, view: BufferId, source: BufferId, path: PathBuf) {
        if let Some(state) = self.state(view) {
            if let Ok(mut s) = state.write() {
                s.source_paths.insert(source, path);
            }
        }
    }
    fn source_path(&self, view: BufferId, source: BufferId) -> Option<PathBuf> {
        self.state(view)?.read().ok()?.source_paths.get(&source).cloned()
    }
    fn find_source_for_path(&self, view: BufferId, path: &Path) -> Option<BufferId> {
        let state = self.state(view)?;
        let guard = state.read().ok()?;
        guard
            .source_paths
            .iter()
            .find(|(_, p)| p.as_path() == path)
            .map(|(id, _)| *id)
    }
}

// ─────────────────────────────────────────────────────────────────
// Typed events
// ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ProjectSearchBatchReady {
    pub view: BufferId,
    pub files: Vec<FileHits>,
    /// K.4.6 follow-up (2026-06-02): per-batch context-lines
    /// value resolved from `search.context_size` at scan
    /// dispatch time. The forwarder reads this off each batch
    /// (it isn't part of the forwarder's persistent state)
    /// because the forwarder runs in the mode's activation
    /// scope, not the scan's. Mirroring the option onto each
    /// batch is cheaper than wiring a side channel for one
    /// `u32` and keeps each batch self-describing.
    pub context_lines: u32,
}

lattice_protocol::register_event!(
    ProjectSearchBatchReady,
    "project-search.batch-ready",
    "One batch of file hits from a running project-search scan.",
    "lattice-multibuffer",
);

#[derive(Debug, Clone)]
pub struct ProjectSearchCompleted {
    pub view: BufferId,
    pub total_hits: usize,
    pub files_scanned: usize,
}

lattice_protocol::register_event!(
    ProjectSearchCompleted,
    "project-search.completed",
    "A project-search scan finished. Carries totals for the headerline.",
    "lattice-multibuffer",
);

#[derive(Debug, Clone)]
pub struct ProjectSearchRefreshed {
    pub view: BufferId,
    pub new_query: String,
}

lattice_protocol::register_event!(
    ProjectSearchRefreshed,
    "project-search.refreshed",
    "User refreshed a project-search view's query.",
    "lattice-multibuffer",
);

#[derive(Debug, Clone)]
pub struct ProjectSearchProgressUpdated {
    pub view: BufferId,
    pub files_scanned: usize,
}

lattice_protocol::register_event!(
    ProjectSearchProgressUpdated,
    "project-search.progress-updated",
    "Mid-scan progress update for headerline status.",
    "lattice-multibuffer",
);

/// Fired by the forwarder task after `append_excerpts` so the
/// cells worker rebuilds the display matrix without waiting for
/// the next keystroke — fixes the blank-results-until-keypress
/// symptom on `:search spawn`.
#[derive(Debug, Clone)]
pub struct MultibufferExcerptsReady {
    pub view: BufferId,
}

lattice_protocol::register_event!(
    MultibufferExcerptsReady,
    "multibuffer.excerpts-ready",
    "New excerpts appended to a multibuffer view.",
    "lattice-multibuffer",
);

// ─────────────────────────────────────────────────────────────────
// Provider-minor mode
// ─────────────────────────────────────────────────────────────────

/// `project-search-multibuffer-mode` — provider-minor for
/// project-search views. Contributes `ReadOnly = true` in M.6.0
/// (wgrep-style editable results land in a follow-up).
pub struct ProjectSearchMultibufferMode;

impl ProjectSearchMultibufferMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("project-search-multibuffer-mode")
    }
}

/// K.2.5 (2026-06-02): static keymap catalog for
/// `ProjectSearchMultibufferMode`.
///
/// Two action chords:
/// - `<CR>` → `action:search-jump-to-source` (jump to the file/row
///   of the excerpt under the cursor)
/// - `g` `r` → `action:search-refresh` (re-run scan with the
///   view's current query)
///
/// Action names registered by
/// `crates/lattice-host/src/actions.rs:populate` against the
/// host's `CommandRegistry`. The K.2.4 host translation pass
/// resolves the names at registration time.
///
/// Replaces `crates/lattice-host/src/multibuffer_keymap.rs`'s
/// `project_search_mode_layer_bindings` which built the trie by
/// hand and was pushed explicitly via `KeymapHandle::push_layer`
/// at boot.
fn project_search_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! {
                mode: Normal, chord: "<CR>",
                doc: "Jump to source file/row of the excerpt under cursor",
                cmd: "action:search-jump-to-source"
            },
            keymap_entry! {
                mode: Normal, chord: "gr",
                doc: "Re-run the project-search scan with the view's current query",
                cmd: "action:search-refresh"
            },
        ]
    })
}

pub struct ProjectSearchMultibufferModeGuard {
    forwarder: Option<tokio::task::JoinHandle<()>>,
    subs: Vec<lattice_runtime::SubscriptionId>,
    bus: Arc<EventBus>,
    /// M.10.3 (2026-06-03): RAII tokens for action-handler
    /// registrations made in `on_activate`. Dropping the Guard
    /// drops these, which in turn unregister the closures from
    /// `ActionHandlerRegistry`. Currently: `<CR>` jump-to-source.
    /// M.10.5 will add `gr` refresh to this Vec.
    _action_handler_registrations: Vec<ActionHandlerRegistration>,
}

impl Drop for ProjectSearchMultibufferModeGuard {
    fn drop(&mut self) {
        if let Some(h) = self.forwarder.take() {
            h.abort();
        }
        for id in self.subs.drain(..) {
            let _ = self.bus.unsubscribe(id);
        }
        // `_action_handler_registrations` drops in field order;
        // each registration's `Drop` impl unregisters its handler
        // from the ActionHandlerRegistry. No explicit work
        // needed here.
    }
}

impl Mode for ProjectSearchMultibufferMode {
    type Guard = ProjectSearchMultibufferModeGuard;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn options(&self) -> OptionOverrideSet {
        // 2026-06-02: search-result multibuffer is EDITABLE. The
        // M.3 substrate (MultibufferDocumentHandle::apply_edit)
        // translates composed-coordinate edits to source coords
        // and forwards to the source documents — the user typing
        // in the multibuffer refactors every matched file
        // in-place. Differentiating UX vs vim quickfix
        // (read-only) and consistent with Zed-style
        // "search-and-replace across project" workflow. The
        // major mode already dropped ReadOnly in M.3; this
        // minor previously layered `ReadOnly = true` for the
        // M.6.0 read-only milestone. Dropped now per
        // paramount-#2 — substrate supports it, honor the
        // substrate's intent.
        OptionOverrideSet::new()
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    /// K.2.5 (2026-06-02): action chord bindings for
    /// project-search views — `<CR>` jumps to the source file
    /// and `gr` re-runs the scan. Resolved at host translation
    /// time via `CommandRegistry` against the action names
    /// registered by `lattice-host::actions::populate`.
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(project_search_keymap_entries())
    }
    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            // ModeContext::buffer_id() returns lattice_protocol::BufferId;
            // registry handles + events use lattice_core::BufferId. Convert.
            let proto_view_id = ctx.buffer_id();
            let view_id = lattice_core::BufferId(proto_view_id.raw() as u32);
            let mb_registry_arc =
                ctx.service::<MultibufferRegistryHandle>().ok_or_else(|| {
                    lattice_mode::ModeActivationError::MissingCapability {
                        mode: ProjectSearchMultibufferMode::mode_id(),
                        missing: CapabilitySet::empty(),
                    }
                })?;
            // Unwrap the outer Arc that `ServiceRegistry::get`
            // wraps the handle in.
            let mb_registry: MultibufferRegistryHandle = (*mb_registry_arc).clone();
            let bus = ctx.events_handle();

            let (batch_tx, mut batch_rx) =
                mpsc::unbounded_channel::<ProjectSearchBatchReady>();
            let (done_tx, mut done_rx) = mpsc::unbounded_channel::<ProjectSearchCompleted>();
            let (progress_tx, mut progress_rx) =
                mpsc::unbounded_channel::<ProjectSearchProgressUpdated>();

            let mut subs = Vec::new();
            subs.push(bus.subscribe_typed::<ProjectSearchBatchReady>(batch_tx));
            subs.push(bus.subscribe_typed::<ProjectSearchCompleted>(done_tx));
            subs.push(bus.subscribe_typed::<ProjectSearchProgressUpdated>(progress_tx));

            // Pull the search service so the forwarder can record
            // source-path mappings as it loads files. Provider-
            // minor activation runs in a context where the service
            // is registered; we tolerate it being missing (test
            // paths) by skipping the record.
            let search_svc_arc = ctx.service::<ProjectSearchServiceHandle>();

            // M.10.3 (2026-06-03): register `<CR>` jump-to-source
            // handler on the ActionHandlerRegistry. The handler
            // closure captures the mb_registry + search_svc +
            // view_id, computes the source target on every fire
            // via `translate_composed_to_source` (M.10.2), and
            // returns an `Effect::Many([OpenBuffer,
            // SelectionChange])` that the host applies through
            // the existing apply-effect pipeline. Replaces the
            // pre-M.10.3 `Editor::do_search_jump_to_source` path
            // that lived in `lattice-host::dispatch`.
            //
            // The chord `<CR>` is already bound to the action
            // name `action:search-jump-to-source` via
            // `project_search_keymap_entries()`. Here we resolve
            // that name to its CommandId and register the
            // handler under that key.
            //
            // Per `feedback_mode_owns_its_surface` +
            // `mode-architecture.md` §5.3: mode owns chord
            // choice (already done) AND handler body (this
            // registration). The host owns the generic
            // chord-dispatch + effect-apply machinery and
            // nothing provider-specific.
            let mut action_registrations: Vec<ActionHandlerRegistration> = Vec::new();
            if let (Some(cmd_registry_arc), Some(action_handlers_arc)) = (
                ctx.service::<CommandRegistryHandle>(),
                ctx.service::<ActionHandlerRegistryHandle>(),
            ) {
                let cmd_registry: &CommandRegistry = &**cmd_registry_arc;
                let action_handlers: ActionHandlerRegistryHandle =
                    (*action_handlers_arc).clone();
                if let Some(jump_command_id) =
                    cmd_registry.id_by_name("action:search-jump-to-source")
                {
                    let mb_registry_for_handler = mb_registry.clone();
                    let search_svc_for_handler: Option<ProjectSearchServiceHandle> =
                        search_svc_arc.as_ref().map(|s| (**s).clone());
                    let view_id_for_handler = view_id;
                    let handler: lattice_mode::ActionHandler = Arc::new(
                        move |ctx: &ActionContext<'_>| -> Option<lattice_grammar::Effect> {
                            // Look up the view from the registry.
                            let view = mb_registry_for_handler.handle(view_id_for_handler)?;
                            // Translate composed cursor → source.
                            let (source_buffer_id, source_position) =
                                view.translate_composed_to_source(ctx.cursor)?;
                            // Resolve source path. v1 uses the
                            // search service's per-view map; if
                            // the service isn't registered (test
                            // harness without boot wiring), the
                            // handler no-ops.
                            let search_svc = search_svc_for_handler.as_ref()?;
                            let path =
                                search_svc.source_path(view_id_for_handler, source_buffer_id)?;
                            // Return the three-step Effect:
                            // (1) record the current
                            //     multibuffer-view cursor +
                            //     buffer-id onto the jump-list
                            //     so `<C-o>` walks the user
                            //     back to where they were before
                            //     they hit `<CR>` (matches
                            //     vim's "big motion" jump-list
                            //     semantics for `gd` / `*` /
                            //     `<C-]>` / etc.);
                            // (2) open the source file;
                            // (3) position the cursor in the
                            //     newly-active source doc.
                            //
                            // `Effect::Many` applies sub-effects
                            // in order. `RecordJump` MUST come
                            // first because it captures the
                            // editor's CURRENT cursor +
                            // active-buffer-id; after
                            // `OpenBuffer` the cursor lives in
                            // the new doc and recording would
                            // capture the wrong location.
                            // M.10.3 bug fix (2026-06-03): use the
                            // atomic `OpenBufferAt` instead of
                            // splitting `OpenBuffer` +
                            // `SelectionChange`. The host's
                            // SelectionChange arm runs synchronously
                            // against the still-active multibuffer,
                            // BEFORE the TUI processes the
                            // OpenBuffer (which switches the active
                            // doc to the source file). Splitting
                            // landed cursor at (0, 0) of the
                            // freshly-opened buffer on first visit;
                            // subsequent visits hit the cached
                            // active-doc cursor preserved from the
                            // earlier (broken) write. `OpenBufferAt`
                            // performs do_edit + set_selections
                            // atomically against the post-do_edit
                            // active doc.
                            Some(lattice_grammar::Effect::Many(vec![
                                lattice_grammar::Effect::RecordJump,
                                lattice_grammar::Effect::OpenBufferAt {
                                    path: Some(path),
                                    position: source_position,
                                    force: false,
                                },
                            ]))
                        },
                    );
                    action_registrations.push(action_handlers.register(jump_command_id, handler));
                }

                // M.10.5 (2026-06-03): register `gr` refresh handler.
                // Pre-M.10.5 the chord routed through
                // `AppEffect::SearchRefresh` → `Action::SearchRefresh`
                // → `Editor::do_search_refresh` in
                // `lattice-host::dispatch`. Now mode-owned: the
                // handler closure captures view_id + the
                // multibuffer registry + the search service +
                // the event bus and performs the refresh
                // in-place (clear excerpts, reset headerline,
                // spawn fresh scan task, publish
                // `ProjectSearchRefreshed`). Returns `None` —
                // no Effect needed since the work is done
                // synchronously inside the closure.
                if let Some(refresh_command_id) =
                    cmd_registry.id_by_name("action:search-refresh")
                {
                    let mb_registry_for_refresh = mb_registry.clone();
                    let search_svc_for_refresh: Option<ProjectSearchServiceHandle> =
                        search_svc_arc.as_ref().map(|s| (**s).clone());
                    let bus_for_refresh = bus.clone();
                    let view_id_for_refresh = view_id;
                    let handler: lattice_mode::ActionHandler = Arc::new(
                        move |_ctx: &ActionContext<'_>| -> Option<lattice_grammar::Effect> {
                            // Tolerate missing service (test
                            // harness without boot wiring).
                            let search_svc = search_svc_for_refresh.as_ref()?;
                            let state = search_svc.state(view_id_for_refresh)?;
                            let (query, options) = {
                                let s = state.read().ok()?;
                                (s.query.clone(), s.options.clone())
                            };
                            let view = mb_registry_for_refresh.handle(view_id_for_refresh)?;
                            // M.6.6: cancel the prior scan before replacing state.
                            if let Some(old) = search_svc.state(view_id_for_refresh) {
                                if let Ok(s) = old.read() {
                                    s.cancel_token.store(true, Ordering::Relaxed);
                                }
                            }
                            // Clear + reset.
                            view.replace_excerpts(
                                std::collections::HashMap::new(),
                                Vec::new(),
                            );
                            view.set_headerline(HeaderlineStatus::InProgress {
                                label: "Refreshing search".into(),
                                count: Some(0),
                            });
                            search_svc.set_state(
                                view_id_for_refresh,
                                ProjectSearchState::scanning(query.clone(), options.clone()),
                            );
                            // Spawn fresh scan task with a fresh cancel token.
                            let cancel = search_svc
                                .state(view_id_for_refresh)
                                .and_then(|s| s.read().ok().map(|s| Arc::clone(&s.cancel_token)))
                                .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
                            let task = spawn_scan_task(
                                view_id_for_refresh,
                                query.clone(),
                                options,
                                search_svc.clone(),
                                bus_for_refresh.clone(),
                                cancel,
                            );
                            search_svc.attach_task(view_id_for_refresh, task);
                            // Publish refresh event so any
                            // other subscribers (e.g. headerline
                            // listeners) can react.
                            bus_for_refresh.publish_typed(ProjectSearchRefreshed {
                                view: view_id_for_refresh,
                                new_query: query,
                            });
                            None
                        },
                    );
                    action_registrations
                        .push(action_handlers.register(refresh_command_id, handler));
                }
            }

            let mb_for_task = mb_registry.clone();
            let view_id_for_task = view_id;
            let search_svc_for_task: Option<ProjectSearchServiceHandle> =
                search_svc_arc.as_ref().map(|s| (**s).clone());
            let bus_for_task = bus.clone();
            let forwarder = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        Some(batch) = batch_rx.recv() => {
                            if batch.view != view_id_for_task { continue; }
                            let Some(view) = mb_for_task.handle(view_id_for_task) else { break; };
                            // M.6.1: load each hit's source file as a
                            // fresh RopeDocumentHandle, add to the
                            // view's source map, append 1-row excerpts.
                            // No dedup across batches in M.6.1.0 — a
                            // file with hits split across batches loads
                            // twice. Documented; M.6.2 adds dedup via
                            // service's source_paths map.
                            let mut hit_count_in_batch = 0usize;
                            for fh in batch.files {
                                let path = fh.path.clone();
                                // M.6.2 (2026-06-01): dedup —
                                // if a previous batch already
                                // loaded this file as a source,
                                // reuse the existing source
                                // buffer instead of spawning a
                                // second one.
                                let existing = search_svc_for_task
                                    .as_ref()
                                    .and_then(|svc| {
                                        svc.find_source_for_path(view_id_for_task, &path)
                                    });
                                let source_id = if let Some(existing) = existing {
                                    existing
                                } else {
                                    let Ok(text_res) = tokio::task::spawn_blocking({
                                        let p = path.clone();
                                        move || std::fs::read_to_string(&p)
                                    })
                                    .await
                                    else { continue; };
                                    let Ok(text) = text_res else { continue; };

                                    let id = BufferId::next();
                                    let document = lattice_core::DocumentBuilder::default()
                                        .with_text(&text)
                                        .with_path(path.clone())
                                        .build();
                                    let registry = Arc::new(CommandRegistry::new());
                                    let handle = spawn_document(id, document, registry);
                                    let dyn_handle: Arc<dyn Document> = Arc::new(handle);
                                    view.add_source(id, dyn_handle);
                                    if let Some(svc) = &search_svc_for_task {
                                        svc.record_source_path(
                                            view_id_for_task,
                                            id,
                                            path.clone(),
                                        );
                                    }
                                    id
                                };

                                // K.4.6 follow-up v4 (2026-06-02):
                                // emit excerpts driven by
                                // `options.context_lines` (resolved
                                // from the `search.context_size`
                                // typed option, defined by
                                // `ProjectSearchMultibufferMode`).
                                //
                                // - context_lines == 0 (default):
                                //   one single-row excerpt per
                                //   matched line (row..=row). The
                                //   substrate's `compose_header_rows`
                                //   (lib.rs) dedupes consecutive
                                //   same-source excerpts so the user
                                //   sees ONE header per file + one
                                //   row per match.
                                //
                                // - context_lines > 0 (e.g.
                                //   `:set search.context_size=3`):
                                //   one excerpt per hit cluster,
                                //   each hit expanded to ±N context
                                //   lines, adjacent clusters merged
                                //   when ranges overlap or touch.
                                //   Mirrors grep `-C N`.
                                let context_lines = batch.context_lines;
                                let excerpts: Vec<Excerpt> = if fh.rows.is_empty() {
                                    Vec::new()
                                } else if context_lines == 0 {
                                    fh.rows
                                        .iter()
                                        .map(|&row| {
                                            Excerpt::new(source_id, row, row).with_header(
                                                ExcerptHeader::new(format!("{}", path.display())),
                                            )
                                        })
                                        .collect()
                                } else {
                                    let mut sorted_rows = fh.rows.clone();
                                    sorted_rows.sort_unstable();
                                    let mut clusters: Vec<(u32, u32)> = Vec::new();
                                    for &row in &sorted_rows {
                                        let start = row.saturating_sub(context_lines);
                                        let end = row.saturating_add(context_lines);
                                        match clusters.last_mut() {
                                            // Merge if the new
                                            // cluster's start touches
                                            // or overlaps the previous
                                            // cluster's end (+1 =
                                            // "touches, no gap").
                                            Some(last) if start <= last.1.saturating_add(1) => {
                                                last.1 = last.1.max(end);
                                            }
                                            _ => clusters.push((start, end)),
                                        }
                                    }
                                    clusters
                                        .into_iter()
                                        .map(|(start, end)| {
                                            Excerpt::new(source_id, start, end).with_header(
                                                ExcerptHeader::new(format!("{}", path.display())),
                                            )
                                        })
                                        .collect()
                                };
                                hit_count_in_batch += fh.rows.len();
                                view.append_excerpts(excerpts);
                            }
                            bus_for_task.publish_typed(MultibufferExcerptsReady {
                                view: view_id_for_task,
                            });

                            view.set_headerline(HeaderlineStatus::InProgress {
                                label: "Searching".into(),
                                count: Some(view.excerpt_count()),
                            });
                            let _ = hit_count_in_batch;
                        }
                        Some(prog) = progress_rx.recv() => {
                            if prog.view != view_id_for_task { continue; }
                            let Some(view) = mb_for_task.handle(view_id_for_task) else { break; };
                            view.set_headerline(HeaderlineStatus::InProgress {
                                label: format!("Searching ({} files)", prog.files_scanned),
                                count: Some(view.excerpt_count()),
                            });
                        }
                        Some(done) = done_rx.recv() => {
                            if done.view != view_id_for_task { continue; }
                            let Some(view) = mb_for_task.handle(view_id_for_task) else { break; };
                            view.set_headerline(HeaderlineStatus::Complete {
                                summary: format!(
                                    "{} hit(s) in {} files",
                                    done.total_hits,
                                    done.files_scanned,
                                ),
                            });
                            // M.10.5 bug fix (2026-06-03): do NOT
                            // break here. Pre-fix the forwarder
                            // exited on Done, so any subsequent
                            // `gr` refresh spawned a new scan but
                            // had no subscriber for its batches —
                            // the buffer stayed blank forever.
                            // Now the forwarder stays subscribed
                            // through refresh cycles; the only
                            // exit path is via the mode's Guard
                            // drop (deactivation), which aborts
                            // this task. `continue` re-enters
                            // `select!` for the next batch /
                            // progress / done event from a
                            // future scan.
                            continue;
                        }
                        else => break,
                    }
                }
            });

            Ok(ProjectSearchMultibufferModeGuard {
                forwarder: Some(forwarder),
                subs,
                bus,
                _action_handler_registrations: action_registrations,
            })
        })
    }
}

// ─────────────────────────────────────────────────────────────────
// Public trigger + scan task
// ─────────────────────────────────────────────────────────────────

/// Open a project-search multibuffer view for `query` under
/// `options.root`. Returns the view's BufferId immediately;
/// scan runs on a spawned tokio task and streams results via
/// typed events.
///
/// Returns `None` when `ProjectSearchService` or `EventBus`
/// isn't registered (boot path didn't wire the provider) —
/// caller logs + recovers.
pub fn project_search(
    activator: &mut dyn ModeActivator,
    query: String,
    options: ProjectSearchOptions,
    registry: Arc<lattice_grammar::CommandRegistry>,
    // K.4.7 (2026-06-07): when `Some`, passed to
    // `create_multibuffer_view` so per-source SyntaxHandles are
    // created as hits arrive via `add_source`.
    lang_registry: Option<Arc<lattice_syntax::LangRegistry>>,
) -> Option<BufferId> {
    let services = activator.services();
    // `services.get::<T>()` wraps the registered value in an
    // outer Arc; our service type is itself
    // `Arc<dyn ProjectSearchService>`, so we get `Arc<Arc<…>>` —
    // unwrap once.
    let search_svc_outer = services.get::<ProjectSearchServiceHandle>()?;
    let search_svc: ProjectSearchServiceHandle = (*search_svc_outer).clone();
    // EventBus is registered as `Arc<EventBus>` in
    // `editor_boot.rs` (`s.register(event_bus.clone())` where
    // `event_bus: Arc<EventBus>`). Lookup therefore queries
    // `Arc<EventBus>` and unwraps one Arc layer to get a usable
    // `Arc<EventBus>` — same shape as the `ProjectSearchServiceHandle`
    // unwrap above. Earlier sites that queried `EventBus`
    // directly silently returned None.
    let events_outer = services.get::<Arc<EventBus>>()?;
    let events: Arc<EventBus> = (*events_outer).clone();

    let view_id = create_multibuffer_view(
        activator,
        std::collections::HashMap::new(),
        Vec::new(),
        Some(format!("*search:{query}*")),
        BufferFlags::default(),
        registry,
        lang_registry,
    );

    search_svc.set_state(
        view_id,
        ProjectSearchState::scanning(query.clone(), options.clone()),
    );

    if let Some(mb_reg) = services.get::<MultibufferRegistryHandle>() {
        if let Some(view) = mb_reg.handle(view_id) {
            view.set_headerline(HeaderlineStatus::InProgress {
                label: "Searching".into(),
                count: Some(0),
            });
        }
    }

    activator.activate_minor_by_id(view_id, ProjectSearchMultibufferMode::mode_id());

    let cancel = search_svc
        .state(view_id)
        .and_then(|s| s.read().ok().map(|s| Arc::clone(&s.cancel_token)))
        .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
    let task = spawn_scan_task(view_id, query, options, search_svc.clone(), events.clone(), cancel);
    search_svc.attach_task(view_id, task);

    Some(view_id)
}

/// Public so the mode's `gr` refresh handler (M.10.5,
/// registered via `ActionHandlerRegistry` from `on_activate`)
/// can respawn after cancelling the prior task.
pub fn spawn_scan_task(
    view: BufferId,
    query: String,
    options: ProjectSearchOptions,
    service: ProjectSearchServiceHandle,
    events: Arc<EventBus>,
    cancel: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        run_scan(view, query, options, service, events, cancel).await;
    })
}

async fn run_scan(
    view: BufferId,
    query: String,
    options: ProjectSearchOptions,
    service: ProjectSearchServiceHandle,
    events: Arc<EventBus>,
    cancel: Arc<AtomicBool>,
) {
    // M.6.3: compile the matcher up-front. Literal mode stores
    // the (possibly lowercased) needle; regex mode compiles a
    // `fancy-regex::Regex` with case-sensitivity baked into the
    // pattern via an injected `(?i)` flag. Bad regex aborts the
    // scan early with `SearchStatus::Failed` so the headerline
    // surfaces the error.
    let matcher = match build_matcher(&query, &options) {
        Ok(m) => m,
        Err(e) => {
            service.set_status(
                view,
                SearchStatus::Failed {
                    reason: e.clone(),
                },
            );
            events.publish_typed(ProjectSearchCompleted {
                view,
                total_hits: 0,
                files_scanned: 0,
            });
            tracing::warn!(error = %e, "project-search: failed to compile matcher");
            return;
        }
    };

    // M.6.X (2026-06-01) UI-discipline retrofit. The editor
    // actor runs on a `current_thread` tokio runtime
    // (`editor_actor.rs:575`); `tokio::spawn` inside
    // `spawn_scan_task` lands on that same single-threaded
    // runtime. `ignore::Walk` + `std::fs::read_to_string` are
    // synchronous blocking calls with no `.await` between
    // syscalls — a `yield_now().await` per file is not nearly
    // enough to keep the actor's command loop responsive
    // (paramount-goal-1: keystroke → glyph within the one-frame ceiling, ≤ 8.3 ms at 120 Hz).
    //
    // Architectural relocation per `feedback_no_ui_thread_work`:
    // wrap the entire walk + match + publish loop in
    // `tokio::task::spawn_blocking` so the work runs on
    // tokio's dedicated blocking-task pool, leaving the
    // current_thread runtime free for the actor + the
    // forwarder. `EventBus::publish_typed` is sync-safe (brief
    // `Mutex<Inner>` acquisition; subscribers use unbounded
    // mpsc senders, see `subscribe_typed(...)` at line 376),
    // so publishes from the blocking task remain correct.
    let view_for_task = view;
    let service_for_task = service.clone();
    let events_for_task = events.clone();
    let cancel_for_task = cancel;
    let _ = tokio::task::spawn_blocking(move || {
        run_scan_blocking(
            view_for_task,
            matcher,
            options,
            service_for_task,
            events_for_task,
            cancel_for_task,
        );
    })
    .await;
}

/// Synchronous body of the scan. Runs on tokio's blocking
/// pool via `spawn_blocking`; never touches the current_thread
/// runtime that drives the editor actor.
fn run_scan_blocking(
    view: BufferId,
    matcher: Matcher,
    options: ProjectSearchOptions,
    service: ProjectSearchServiceHandle,
    events: Arc<EventBus>,
    cancel: Arc<AtomicBool>,
) {
    let mut walker = ignore::Walk::new(&options.root);
    let mut files_scanned: usize = 0;
    let mut total_hits: usize = 0;
    let mut batch: Vec<FileHits> = Vec::new();
    let batch_files = 50usize;
    let progress_interval = 200usize;
    let max_files = options.max_files.unwrap_or(usize::MAX);

    while let Some(entry) = walker.next() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let Ok(entry) = entry else { continue; };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        if files_scanned >= max_files {
            break;
        }
        let path = entry.into_path();
        let hits = scan_file(&path, &matcher, options.max_hits_per_file);
        files_scanned += 1;

        if !hits.is_empty() {
            total_hits += hits.len();
            batch.push(FileHits { path, rows: hits });
        }

        if batch.len() >= batch_files {
            let add: usize = batch.iter().map(|f| f.rows.len()).sum();
            service.add_hits(view, add);
            events.publish_typed(ProjectSearchBatchReady {
                view,
                files: std::mem::take(&mut batch),
                context_lines: options.context_lines,
            });
        }
        if files_scanned % progress_interval == 0 {
            events.publish_typed(ProjectSearchProgressUpdated { view, files_scanned });
        }
    }

    if !batch.is_empty() {
        let add: usize = batch.iter().map(|f| f.rows.len()).sum();
        service.add_hits(view, add);
        events.publish_typed(ProjectSearchBatchReady {
            view,
            files: batch,
            context_lines: options.context_lines,
        });
    }
    service.set_status(view, SearchStatus::Done { total_hits });
    events.publish_typed(ProjectSearchCompleted {
        view,
        total_hits,
        files_scanned,
    });
}

/// M.6.3 (2026-06-01): compiled matcher. Literal mode stores the
/// (possibly lowercased) needle; regex mode wraps a compiled
/// `fancy-regex::Regex`.
#[derive(Debug)]
enum Matcher {
    Literal {
        needle: String,
        case_sensitive: bool,
    },
    Regex(fancy_regex::Regex),
}

impl Matcher {
    fn line_matches(&self, line: &str) -> bool {
        match self {
            Matcher::Literal {
                needle,
                case_sensitive: true,
            } => line.contains(needle.as_str()),
            Matcher::Literal {
                needle,
                case_sensitive: false,
            } => line.to_lowercase().contains(needle.as_str()),
            Matcher::Regex(re) => re.is_match(line).unwrap_or(false),
        }
    }
}

fn build_matcher(query: &str, options: &ProjectSearchOptions) -> Result<Matcher, String> {
    if options.regex {
        // Inject `(?i)` when case-insensitive so the compiled
        // pattern handles the casing — leaves the user's
        // pattern verbatim otherwise.
        let pattern = if options.case_sensitive {
            query.to_string()
        } else {
            format!("(?i){query}")
        };
        fancy_regex::Regex::new(&pattern)
            .map(Matcher::Regex)
            .map_err(|e| format!("invalid regex `{query}`: {e}"))
    } else {
        let needle = if options.case_sensitive {
            query.to_string()
        } else {
            query.to_lowercase()
        };
        Ok(Matcher::Literal {
            needle,
            case_sensitive: options.case_sensitive,
        })
    }
}

fn scan_file(path: &Path, matcher: &Matcher, max_hits: usize) -> Vec<u32> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut hits = Vec::new();
    for (row, line) in text.lines().enumerate() {
        if matcher.line_matches(line) {
            hits.push(row as u32);
            if hits.len() >= max_hits {
                break;
            }
        }
    }
    hits
}

// ─────────────────────────────────────────────────────────────────
// Boot integration
// ─────────────────────────────────────────────────────────────────

/// M.6 boot helper — register the provider-minor mode. Mode
/// registry is constructed before services in the host's boot
/// path, so the two registrations are split for ordering
/// flexibility.
pub fn register_project_search_mode(mode_registry: &mut ModeRegistry) {
    mode_registry
        .register(ProjectSearchMultibufferMode)
        .expect("project-search-multibuffer-mode registers without conflict at boot");
}

/// M.6 boot helper — register the service handle. Call in the
/// host's `ServiceRegistry` construction block.
pub fn register_project_search_service(services: &mut ServiceRegistry) {
    let svc: ProjectSearchServiceHandle = Arc::new(InMemoryProjectSearchService::new());
    services.register(svc);
}

/// K.2.5 (2026-06-02): register the `:search <query>` ex-command.
///
/// Relocated from `crates/lattice-host/src/multibuffer_keymap.rs::register_search_ex_command`
/// as part of the K.2.5 migration. Boot path in `editor_boot.rs`
/// calls this directly now; behaviour preserved verbatim.
///
/// Stashes the query as `Args::String` and routes through
/// `AppEffect::SearchTrigger { query }`. M.10.6 (2026-06-03)
/// inlined the work into the host's apply_effect arm; the
/// previous `Action::SearchTrigger` + `Editor::do_search` hops
/// are gone. Empty query is rejected with `BadArgs` — opening
/// an empty search view doesn't make sense.
pub fn register_search_ex_command(registry: &mut CommandRegistry) {
    use lattice_grammar::app_effect::AppEffect;
    use lattice_grammar::args::{ArgSpec, Args};
    use lattice_grammar::command::LatencyClass;
    use lattice_grammar::effect::Effect;
    use lattice_grammar::error::CommandError;
    use lattice_grammar::registry::{ExCommandSpec, SurfaceForm};

    registry.register_ex_command(
        "search",
        "Project-wide search for the literal query. Opens a multibuffer view that streams results as the scan runs.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(|s: &str, _bang: bool| {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    return Err(CommandError::BadArgs(
                        ":search requires a non-empty query".into(),
                    ));
                }
                Ok(Args::String(trimmed.to_string()))
            }),
            apply: Box::new(|ctx| {
                let query = match &ctx.args {
                    Args::String(s) => s.clone(),
                    _ => String::new(),
                };
                Ok(Effect::AppAction(AppEffect::SearchTrigger { query }))
            }),
            args_schema: vec![ArgSpec::required(
                "query",
                lattice_grammar::args::ArgKind::String,
                "search query",
            )],
            surface_form: SurfaceForm::Keyword,
        },
    );
}

/// Convenience wrapper that calls both helpers. Useful for
/// tests that wire mode + service together; production boot
/// uses the split helpers.
pub fn register_project_search(
    mode_registry: &mut ModeRegistry,
    services: &mut ServiceRegistry,
) {
    register_project_search_mode(mode_registry);
    register_project_search_service(services);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn service_state_roundtrip() {
        let svc = InMemoryProjectSearchService::new();
        let view = BufferId(42);
        svc.set_state(
            view,
            ProjectSearchState::scanning("foo".into(), ProjectSearchOptions::default()),
        );
        assert_eq!(svc.len(), 1);
        let state = svc.state(view).unwrap();
        assert_eq!(state.read().unwrap().query, "foo");

        svc.add_hits(view, 5);
        assert_eq!(svc.state(view).unwrap().read().unwrap().total_hits, 5);

        svc.set_status(view, SearchStatus::Done { total_hits: 5 });
        match &svc.state(view).unwrap().read().unwrap().status {
            SearchStatus::Done { total_hits } => assert_eq!(*total_hits, 5),
            other => panic!("expected Done, got {other:?}"),
        }

        svc.clear(view);
        assert_eq!(svc.len(), 0);
        assert!(svc.state(view).is_none());
    }

    fn literal_matcher(needle: &str, case_sensitive: bool) -> Matcher {
        build_matcher(
            needle,
            &ProjectSearchOptions {
                regex: false,
                case_sensitive,
                ..ProjectSearchOptions::default()
            },
        )
        .unwrap()
    }

    fn regex_matcher(pattern: &str, case_sensitive: bool) -> Matcher {
        build_matcher(
            pattern,
            &ProjectSearchOptions {
                regex: true,
                case_sensitive,
                ..ProjectSearchOptions::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn scan_file_finds_literal_matches() {
        let tmp = tempfile_path();
        std::fs::write(&tmp, "alpha\nBetA Foo\ngamma\nfoo bar\n").unwrap();
        let hits = scan_file(&tmp, &literal_matcher("foo", false), 100);
        assert_eq!(hits, vec![1, 3]);

        let hits_case = scan_file(&tmp, &literal_matcher("foo", true), 100);
        assert_eq!(hits_case, vec![3]);

        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn scan_file_respects_max_hits() {
        let tmp = tempfile_path();
        std::fs::write(&tmp, "foo\nfoo\nfoo\nfoo\n").unwrap();
        let hits = scan_file(&tmp, &literal_matcher("foo", true), 2);
        assert_eq!(hits, vec![0, 1]);
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn scan_file_regex_mode_matches_pattern() {
        let tmp = tempfile_path();
        std::fs::write(
            &tmp,
            "TODO: fix\nDONE: nothing\ntodo: lowercase\nFIXME: also\n",
        )
        .unwrap();

        // Case-sensitive regex: only literal "TODO".
        let hits = scan_file(&tmp, &regex_matcher(r"^TODO", true), 100);
        assert_eq!(hits, vec![0]);

        // Case-insensitive regex: TODO + todo lines match.
        let hits_ci = scan_file(&tmp, &regex_matcher(r"^TODO", false), 100);
        assert_eq!(hits_ci, vec![0, 2]);

        // Alternation: TODO or FIXME.
        let hits_alt = scan_file(&tmp, &regex_matcher(r"^(TODO|FIXME)", true), 100);
        assert_eq!(hits_alt, vec![0, 3]);

        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn build_matcher_rejects_invalid_regex() {
        let err = build_matcher(
            "(unclosed",
            &ProjectSearchOptions {
                regex: true,
                ..ProjectSearchOptions::default()
            },
        )
        .unwrap_err();
        assert!(err.contains("invalid regex"));
    }

    #[test]
    fn find_source_for_path_reverse_lookup_roundtrips() {
        // M.6.2: the dedup hook in the forwarder consults
        // `find_source_for_path` before spawning a fresh
        // RopeDocumentHandle. Verify the lookup returns the
        // first-recorded source for a path.
        let svc = InMemoryProjectSearchService::new();
        let view = BufferId(1);
        svc.set_state(
            view,
            ProjectSearchState::scanning("q".into(), ProjectSearchOptions::default()),
        );

        let path_a = PathBuf::from("/tmp/a.rs");
        let path_b = PathBuf::from("/tmp/b.rs");
        let src_a = BufferId(10);
        let src_b = BufferId(11);
        svc.record_source_path(view, src_a, path_a.clone());
        svc.record_source_path(view, src_b, path_b.clone());

        assert_eq!(svc.find_source_for_path(view, &path_a), Some(src_a));
        assert_eq!(svc.find_source_for_path(view, &path_b), Some(src_b));
        assert_eq!(
            svc.find_source_for_path(view, &PathBuf::from("/tmp/missing.rs")),
            None,
        );
        // A second view with the same path doesn't see source-a.
        assert_eq!(
            svc.find_source_for_path(BufferId(2), &path_a),
            None,
        );
    }

    #[test]
    fn scan_file_missing_returns_empty() {
        let hits = scan_file(
            Path::new("/tmp/__lattice_definitely_missing__"),
            &literal_matcher("x", true),
            100,
        );
        assert!(hits.is_empty());
    }

    fn tempfile_path() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "lattice-search-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        p
    }

    // ── M.6.6: cooperative cancellation ──────────────────────────

    /// A cancelled scan must exit without publishing
    /// `ProjectSearchCompleted`. Verifies the token-check at
    /// the top of each `walker.next()` iteration fires before
    /// any file is processed when the token is pre-set.
    #[test]
    fn cancelled_scan_exits_without_publishing_completed() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let events = Arc::new(lattice_runtime::EventBus::new());
        let view = BufferId(77);

        // Track completions via an mpsc channel.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ProjectSearchCompleted>();
        events.subscribe_typed(tx);

        let svc = InMemoryProjectSearchService::handle();
        svc.set_state(view, ProjectSearchState::scanning("x".into(), ProjectSearchOptions::default()));
        let cancel = svc
            .state(view)
            .and_then(|s| s.read().ok().map(|s| Arc::clone(&s.cancel_token)))
            .unwrap();

        // Pre-cancel before the task can process any files.
        cancel.store(true, Ordering::Relaxed);

        rt.block_on(async move {
            let handle = spawn_scan_task(
                view,
                "x".into(),
                ProjectSearchOptions::default(),
                svc,
                events,
                cancel,
            );
            handle.await.unwrap();
        });

        assert!(
            rx.try_recv().is_err(),
            "cancelled scan must not publish ProjectSearchCompleted"
        );
    }

    /// Refreshing a running scan: the old token fires, the new
    /// task gets a fresh (unset) token and runs to completion.
    #[tokio::test(flavor = "current_thread")]
    async fn refresh_cancels_old_token_and_issues_fresh_one() {
        let svc = InMemoryProjectSearchService::handle();
        let view = BufferId(88);

        svc.set_state(view, ProjectSearchState::scanning("x".into(), ProjectSearchOptions::default()));
        let old_cancel = svc
            .state(view)
            .and_then(|s| s.read().ok().map(|s| Arc::clone(&s.cancel_token)))
            .unwrap();

        // Simulate the refresh: flip old token, install fresh state.
        old_cancel.store(true, Ordering::Relaxed);
        svc.set_state(view, ProjectSearchState::scanning("x".into(), ProjectSearchOptions::default()));
        let new_cancel = svc
            .state(view)
            .and_then(|s| s.read().ok().map(|s| Arc::clone(&s.cancel_token)))
            .unwrap();

        assert!(old_cancel.load(Ordering::Relaxed), "old token must be set");
        assert!(!new_cancel.load(Ordering::Relaxed), "fresh token must start unset");
        assert!(
            !Arc::ptr_eq(&old_cancel, &new_cancel),
            "refresh must allocate a distinct cancel token"
        );
    }
}
