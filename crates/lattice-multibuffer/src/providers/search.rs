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
use std::sync::{Arc, RwLock};

use lattice_config::{OptionOverrideSet, overrides};
use lattice_core::{BufferFlags, BufferId};
use lattice_grammar::CommandRegistry;
use lattice_mode::{
    CapabilitySet, LifecycleFuture, Mode, ModeActivator, ModeContext, ModeId, ModeKind,
    ModeRegistry, ServiceRegistry,
};
use lattice_runtime::{Document, EventBus, spawn_document};
use tokio::sync::mpsc;

use crate::registry::MultibufferRegistryHandle;
use crate::view::create_multibuffer_view;
use crate::{Excerpt, ExcerptHeader, HeaderlineStatus};

// ─────────────────────────────────────────────────────────────────
// Public state types
// ─────────────────────────────────────────────────────────────────

/// User-supplied scan parameters.
#[derive(Debug, Clone)]
pub struct ProjectSearchOptions {
    /// Project root the scan walks. Defaults to CWD when the
    /// trigger function isn't given an explicit one.
    pub root: PathBuf,
    /// Whether matches are case-sensitive. M.6.0 is literal
    /// substring; M.6.1 adds regex.
    pub case_sensitive: bool,
    /// Cap on the number of files scanned. `None` = unlimited
    /// (defaults to large; the scan respects `.gitignore`).
    pub max_files: Option<usize>,
    /// Cap on hits per file before moving on (per-file
    /// hit-limit; prevents one huge file from monopolising the
    /// batch budget).
    pub max_hits_per_file: usize,
}

impl Default for ProjectSearchOptions {
    fn default() -> Self {
        Self {
            root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            case_sensitive: false,
            max_files: None,
            max_hits_per_file: 100,
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
    /// M.6.1: source `BufferId` → on-disk path. Populated by the
    /// provider-minor's forwarder as it loads files into the
    /// view's source map. `do_search_jump_to_source` reads this
    /// to resolve the excerpt under cursor back to a file path.
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

pub struct ProjectSearchMultibufferModeGuard {
    forwarder: Option<tokio::task::JoinHandle<()>>,
    subs: Vec<lattice_runtime::SubscriptionId>,
    bus: Arc<EventBus>,
}

impl Drop for ProjectSearchMultibufferModeGuard {
    fn drop(&mut self) {
        if let Some(h) = self.forwarder.take() {
            h.abort();
        }
        for id in self.subs.drain(..) {
            let _ = self.bus.unsubscribe(id);
        }
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
        overrides! {
            lattice_config::ReadOnly = true,
        }
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
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

            let mb_for_task = mb_registry.clone();
            let view_id_for_task = view_id;
            let search_svc_for_task: Option<ProjectSearchServiceHandle> =
                search_svc_arc.as_ref().map(|s| (**s).clone());
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
                                    let document = lattice_core::Document::from_text(&text);
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

                                let excerpts: Vec<Excerpt> = fh
                                    .rows
                                    .iter()
                                    .map(|&row| {
                                        Excerpt::new(source_id, row, row).with_header(
                                            ExcerptHeader::new(format!("{}", path.display())),
                                        )
                                    })
                                    .collect();
                                hit_count_in_batch += excerpts.len();
                                view.append_excerpts(excerpts);
                            }

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
                            break;
                        }
                        else => break,
                    }
                }
            });

            Ok(ProjectSearchMultibufferModeGuard {
                forwarder: Some(forwarder),
                subs,
                bus,
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
) -> Option<BufferId> {
    let services = activator.services();
    // `services.get::<T>()` wraps the registered value in an
    // outer Arc; our service type is itself
    // `Arc<dyn ProjectSearchService>`, so we get `Arc<Arc<…>>` —
    // unwrap once.
    let search_svc_outer = services.get::<ProjectSearchServiceHandle>()?;
    let search_svc: ProjectSearchServiceHandle = (*search_svc_outer).clone();
    let events_outer = services.get::<EventBus>()?;
    let events: Arc<EventBus> = events_outer;

    let view_id = create_multibuffer_view(
        activator,
        std::collections::HashMap::new(),
        Vec::new(),
        Some(format!("*search:{query}*")),
        BufferFlags::default(),
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

    let task = spawn_scan_task(view_id, query, options, search_svc.clone(), events.clone());
    search_svc.attach_task(view_id, task);

    Some(view_id)
}

/// Public so the host's `do_search_refresh` can respawn after
/// cancelling the prior task.
pub fn spawn_scan_task(
    view: BufferId,
    query: String,
    options: ProjectSearchOptions,
    service: ProjectSearchServiceHandle,
    events: Arc<EventBus>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        run_scan(view, query, options, service, events).await;
    })
}

async fn run_scan(
    view: BufferId,
    query: String,
    options: ProjectSearchOptions,
    service: ProjectSearchServiceHandle,
    events: Arc<EventBus>,
) {
    let needle = if options.case_sensitive {
        query.clone()
    } else {
        query.to_lowercase()
    };

    let mut walker = ignore::Walk::new(&options.root);
    let mut files_scanned: usize = 0;
    let mut total_hits: usize = 0;
    let mut batch: Vec<FileHits> = Vec::new();
    let batch_files = 50usize;
    let progress_interval = 200usize;
    let max_files = options.max_files.unwrap_or(usize::MAX);

    while let Some(entry) = walker.next() {
        let Ok(entry) = entry else { continue; };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        if files_scanned >= max_files {
            break;
        }
        let path = entry.into_path();
        let hits = scan_file(&path, &needle, options.case_sensitive, options.max_hits_per_file);
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
            });
        }
        if files_scanned % progress_interval == 0 {
            events.publish_typed(ProjectSearchProgressUpdated { view, files_scanned });
        }
        tokio::task::yield_now().await;
    }

    if !batch.is_empty() {
        let add: usize = batch.iter().map(|f| f.rows.len()).sum();
        service.add_hits(view, add);
        events.publish_typed(ProjectSearchBatchReady { view, files: batch });
    }
    service.set_status(view, SearchStatus::Done { total_hits });
    events.publish_typed(ProjectSearchCompleted {
        view,
        total_hits,
        files_scanned,
    });
}

fn scan_file(path: &Path, needle: &str, case_sensitive: bool, max_hits: usize) -> Vec<u32> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut hits = Vec::new();
    for (row, line) in text.lines().enumerate() {
        let found = if case_sensitive {
            line.contains(needle)
        } else {
            line.to_lowercase().contains(needle)
        };
        if found {
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

    #[test]
    fn scan_file_finds_literal_matches() {
        let tmp = tempfile_path();
        std::fs::write(&tmp, "alpha\nBetA Foo\ngamma\nfoo bar\n").unwrap();
        let hits = scan_file(&tmp, "foo", false, 100);
        assert_eq!(hits, vec![1, 3]);

        let hits_case = scan_file(&tmp, "foo", true, 100);
        assert_eq!(hits_case, vec![3]);

        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn scan_file_respects_max_hits() {
        let tmp = tempfile_path();
        std::fs::write(&tmp, "foo\nfoo\nfoo\nfoo\n").unwrap();
        let hits = scan_file(&tmp, "foo", true, 2);
        assert_eq!(hits, vec![0, 1]);
        std::fs::remove_file(&tmp).ok();
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
            "x",
            true,
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
}
