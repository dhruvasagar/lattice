//! OM.A1 (2026-08-25): the **agenda provider** — a date-grouped multibuffer
//! of rows contributed by plugin `agenda-source` producers.
//!
//! Design: [`org-mode.md`](../../../../docs/dev/architecture/org-mode.md) §6.
//! Slice plan: `slice-plans/org-mode.md` OM.A1–A3.
//!
//! ## Nothing here knows what an org file is
//!
//! That is the point. The provider walks the project, asks the
//! [`AgendaSourceRegistry`](lattice_mode::AgendaSourceRegistry) which sources
//! claim each file's extension, and hands the matching ones its text. Which
//! extensions those are, what a headline is, what `SCHEDULED:` means, how a
//! date sorts — all of it lives in the guest. The host contributes the walk,
//! the ordering primitive, and the multibuffer.
//!
//! ## Why a multibuffer and not a rendered list
//!
//! An agenda row IS an excerpt: `Excerpt { source, start_line, end_line,
//! header }` (§6.1). Taking that seriously buys jump-to-source,
//! edit-propagates-to-source, headerline status and refresh from machinery
//! that already ships. The second of those is the one that decided it — org's
//! agenda is a place you change TODO states *from*, and an agenda you can
//! only read is a lesser feature wearing the name.
//!
//! ## Why the whole scan finishes before anything is appended
//!
//! Unlike `providers::search`, the agenda's row order is **global**: a row
//! from the last file scanned may belong at the top. So the scan collects,
//! stable-sorts on the guest's `sort_key`, and appends once. Progress is not
//! lost — it moves to the headerline, which reports files scanned as the walk
//! runs (§6.2). Appending per file and re-sorting per batch was rejected:
//! rewriting every row on each batch is a whole-viewport restyle, which the
//! UX rules veto outright.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use lattice_core::{BufferFlags, BufferId, DocumentBuilder};
use lattice_grammar::{Args, CommandRegistry, CommandRegistryHandle};
use lattice_mode::{
    AgendaEntry, AgendaSourceRegistryHandle, AsyncAgendaSource, ModeActivator, ProviderViewOutcome,
    ServiceRegistry,
};
use lattice_runtime::{Document, EventBus, spawn_document};

use crate::events::MultibufferExcerptsReady;
use crate::registry::MultibufferRegistryHandle;
use crate::view::create_multibuffer_view;
use crate::{Excerpt, ExcerptHeader, HeaderlineStatus};

/// The name the provider-view seam registers under. Both front-ends (the
/// `:agenda` ex-command and, from OM.A3, the view's own `gr`) name this
/// constant so they cannot drift apart.
pub const PROVIDER_NAME: &str = "agenda";

/// The view's buffer name. One agenda, reused across triggers — a second
/// `:agenda` re-scans into the same buffer rather than accumulating views.
pub const VIEW_NAME: &str = "*agenda*";

/// How many files one `spawn_blocking` hop reads. Big enough that the
/// blocking-pool round-trip is amortised, small enough that a huge project
/// still updates the headerline while it walks.
const READ_BATCH: usize = 32;

/// Update the headerline every this many files.
const PROGRESS_INTERVAL: usize = 50;

// ─────────────────────────────────────────────────────────────────
// Scan parameters + per-view state
// ─────────────────────────────────────────────────────────────────

/// What a scan walks.
#[derive(Debug, Clone)]
pub struct AgendaOptions {
    /// Project root the walk starts from.
    pub root: PathBuf,
    /// Cap on files *offered to a source* (not files walked). `None` =
    /// unlimited. A bound exists because the walk is unattended: a user who
    /// runs `:agenda` from `$HOME` should get a slow answer, not a hung one.
    pub max_files: Option<usize>,
}

impl Default for AgendaOptions {
    fn default() -> Self {
        Self {
            root: lattice_core::project::root_from_cwd().unwrap_or_else(|| PathBuf::from(".")),
            max_files: None,
        }
    }
}

/// Per-view scan state. OM.A3's `gr` reads the root back out of this so a
/// refresh re-scans what the view already shows rather than resetting to the
/// current working directory.
#[derive(Debug, Clone)]
pub struct AgendaState {
    pub options: AgendaOptions,
}

/// Per-view state, shared between the trigger and the scan task.
pub trait AgendaService: Send + Sync + std::fmt::Debug {
    fn state(&self, view: BufferId) -> Option<Arc<RwLock<AgendaState>>>;
    fn set_state(&self, view: BufferId, state: AgendaState);
    fn clear(&self, view: BufferId);
}

/// Register **and** look up with this exact alias (the `ServiceRegistry`
/// `TypeId` rule).
pub type AgendaServiceHandle = Arc<dyn AgendaService>;

#[derive(Debug, Default)]
pub struct InMemoryAgendaService {
    views: RwLock<HashMap<BufferId, Arc<RwLock<AgendaState>>>>,
}

impl InMemoryAgendaService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn handle() -> AgendaServiceHandle {
        Arc::new(Self::new())
    }
}

impl AgendaService for InMemoryAgendaService {
    fn state(&self, view: BufferId) -> Option<Arc<RwLock<AgendaState>>> {
        self.views.read().ok()?.get(&view).cloned()
    }

    fn set_state(&self, view: BufferId, state: AgendaState) {
        if let Ok(mut m) = self.views.write() {
            m.insert(view, Arc::new(RwLock::new(state)));
        }
    }

    fn clear(&self, view: BufferId) {
        if let Ok(mut m) = self.views.write() {
            m.remove(&view);
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// The trigger
// ─────────────────────────────────────────────────────────────────

/// Open (or re-drive) the agenda view.
///
/// The closure registered on the generic provider-view seam, and therefore
/// the whole of what the host does for this feature: the host arm looks the
/// name up, calls this with itself as the activator, and applies the returned
/// [`ProviderViewOutcome`]. No `Editor::` method, no host `Action` variant,
/// no dispatch arm.
///
/// The view opens **empty and immediately**; the scan runs off-thread. On a
/// re-trigger the excerpts are cleared here rather than in the task, so the
/// clear is visible before the first rows land instead of the two scans
/// briefly showing together.
pub fn open_agenda(activator: &mut dyn ModeActivator, args: &Args) -> ProviderViewOutcome {
    let services = activator.services();

    let Some(sources) = services.get::<AgendaSourceRegistryHandle>() else {
        return ProviderViewOutcome::Declined {
            message: "agenda: no agenda-source registry; the plugin host is not wired".to_string(),
        };
    };
    let snapshot = sources.load();
    if snapshot.is_empty() {
        // Not an error — it is the honest state of an editor with no agenda
        // plugin installed. Opening an empty view and leaving the user to
        // guess why is the worse UX (the `Declined` contract).
        return ProviderViewOutcome::Declined {
            message: "agenda: no plugin provides agenda rows".to_string(),
        };
    }

    let Some(registry) = services.get::<CommandRegistryHandle>() else {
        return ProviderViewOutcome::Declined {
            message: "agenda: command registry unavailable; cannot open the view".to_string(),
        };
    };
    let lang_registry = services
        .get::<Arc<lattice_syntax::LangRegistry>>()
        .map(|h| (*h).clone());

    // Read before any `&mut` use of the activator: `snapshot` borrows the
    // registry's ArcSwap guard, and the activation calls below need the
    // activator mutably.
    let view_modes = snapshot.view_modes();

    let existing = existing_view(&services);

    // The root, in precedence order: an explicit argument, else the root the
    // OPEN view already shows, else the active buffer's project.
    //
    // `:agenda ~/notes` from a code checkout scans the notes, which is the
    // point of accepting an argument at all. The middle case is what makes
    // OM.A3's `gr` a refresh rather than a reset: refreshing an agenda over
    // `~/notes` must not silently turn it into an agenda over the current
    // checkout, which is the mistake magit's `gr` documents at PD.9.
    let mut options = AgendaOptions::default();
    if let Some(view) = existing
        && let Some(svc) = services.get::<AgendaServiceHandle>()
        && let Some(state) = svc.state(view)
        && let Ok(state) = state.read()
    {
        options = state.options.clone();
    }
    if let Args::String(s) = args {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            options.root = PathBuf::from(shellexpand_tilde(trimmed));
        }
    }

    let view = match existing {
        Some(view) => view,
        None => create_multibuffer_view(
            activator,
            HashMap::new(),
            Vec::new(),
            Some(VIEW_NAME.to_string()),
            BufferFlags::default(),
            (*registry).clone(),
            lang_registry,
        ),
    };

    let Some(mb_registry) = services.get::<MultibufferRegistryHandle>() else {
        return ProviderViewOutcome::Declined {
            message: "agenda: multibuffer registry unavailable; cannot open the view".to_string(),
        };
    };
    let Some(handle) = mb_registry.handle(view) else {
        return ProviderViewOutcome::Declined {
            message: "agenda: the view failed to open".to_string(),
        };
    };

    if let Some(svc) = services.get::<AgendaServiceHandle>() {
        svc.set_state(
            view,
            AgendaState {
                options: options.clone(),
            },
        );
    } else {
        tracing::debug!("agenda: service not registered; the view's root will not be tracked");
    }

    handle.replace_excerpts(HashMap::new(), Vec::new());
    handle.set_headerline(HeaderlineStatus::InProgress {
        label: "Building agenda".to_string(),
        count: Some(0),
        emphasis: None,
    });

    // OM.A3: the view's own minor. `gr` arrives through the implies cascade
    // (`refresh_action` returns `Some`), so this one call is the whole of the
    // wiring — the same shape `project_search` uses for `ProjectSearchMode`.
    activator.activate_minor_by_id(view, AgendaViewMode::mode_id());

    // …and each SOURCE's own minor, so a producer can act on its own rows.
    // The host stays generic: it activates a mode by the name the source
    // declared and never learns what the chords do. A name that is not
    // registered echoes a warning through the ordinary activation path rather
    // than failing the open — the rows are still worth showing.
    for mode in view_modes {
        activator.activate_minor_by_id(view, lattice_mode::ModeId::new(&mode));
    }

    let events = services.get::<Arc<EventBus>>().map(|b| (*b).clone());
    let sources_for_scan = snapshot.sources();
    drop(snapshot);
    spawn_agenda_scan(
        view,
        options,
        sources_for_scan,
        (*mb_registry).clone(),
        events,
    );

    ProviderViewOutcome::Opened {
        view,
        message: Some("agenda: scanning…".to_string()),
    }
}

/// The already-open agenda view, if there is one.
fn existing_view(services: &ServiceRegistry) -> Option<BufferId> {
    let buffers = services.get::<lattice_mode::BufferStoreHandle>()?;
    let id = buffers.find_by_name(VIEW_NAME)?;
    let registry = services.get::<MultibufferRegistryHandle>()?;
    registry.handle(id).map(|_| id)
}

/// `~` expansion for a hand-typed root. Not a full shell expansion — the
/// argument is a path, and the one thing a user types by hand that a
/// `PathBuf` will not resolve is a leading tilde.
fn shellexpand_tilde(raw: &str) -> String {
    let Some(rest) = raw.strip_prefix('~') else {
        return raw.to_string();
    };
    let Some(home) = std::env::var_os("HOME") else {
        return raw.to_string();
    };
    let mut out = PathBuf::from(home);
    let rest = rest.trim_start_matches('/');
    if !rest.is_empty() {
        out.push(rest);
    }
    out.display().to_string()
}

// ─────────────────────────────────────────────────────────────────
// The scan
// ─────────────────────────────────────────────────────────────────

/// One file's contribution: its text (kept so the source `Document` is built
/// from the SAME bytes the guest saw) and the rows found in it.
struct FileRows {
    path: PathBuf,
    text: String,
    entries: Vec<AgendaEntry>,
}

/// A row after the cross-file sort, carrying the index of its file.
struct SortedRow {
    file: usize,
    entry: AgendaEntry,
}

/// Run the scan off-thread and populate `view`.
///
/// Shape, and why:
///
/// - The `ignore::Walk` and every `read_to_string` run under
///   **`spawn_blocking`**. The editor actor is a `current_thread` runtime, so
///   a bare `tokio::spawn` would land the whole walk on the actor thread —
///   paramount goal #1's forbidden pattern.
/// - The guest `scan` calls are `await`ed on the async runtime between those
///   hops, because a wasmtime async call must not be made from a blocking
///   task.
/// - **A file no source claims is never read.** The extension test happens
///   before the read, so an agenda over a Rust checkout costs a directory
///   walk and nothing else.
/// - The append publishes [`MultibufferExcerptsReady`] — the registered
///   off-keystroke wake. Without it the rows would sit invisible until the
///   user pressed a key, which reads as a rendering fault and is not one.
///
/// `pub` for the same reason `search::spawn_scan_task` is: OM.A3's `gr`
/// handler respawns without going back through the trigger, and the
/// throughput bench drives it without an activator.
pub fn spawn_agenda_scan(
    view: BufferId,
    options: AgendaOptions,
    sources: Vec<Arc<dyn AsyncAgendaSource>>,
    mb_registry: MultibufferRegistryHandle,
    events: Option<Arc<EventBus>>,
) {
    tokio::spawn(async move {
        // `begin` first, and a source that refuses is dropped from THIS scan
        // rather than failing it: its per-scan state is now unknown, so its
        // rows would be untrustworthy, but the other sources' are fine.
        let mut live: Vec<Arc<dyn AsyncAgendaSource>> = Vec::with_capacity(sources.len());
        for source in sources {
            match source.begin().await {
                Ok(()) => live.push(source),
                Err(e) => tracing::debug!(
                    source = source.source_id(),
                    error = %e,
                    "agenda: a source failed to begin; skipping it this scan"
                ),
            }
        }
        if live.is_empty() {
            finish_empty(&mb_registry, &events, view, &ScanOutcome::default());
            return;
        }

        let root = options.root.clone();
        let max_files = options.max_files.unwrap_or(usize::MAX);
        let extensions: Vec<String> = live
            .iter()
            .flat_map(|s| s.extensions().iter().cloned())
            .collect();

        let candidates = match tokio::task::spawn_blocking(move || {
            walk_candidates(&root, &extensions, max_files)
        })
        .await
        {
            Ok(paths) => paths,
            Err(e) => {
                tracing::warn!(error = %e, "agenda: the walk task failed");
                Vec::new()
            }
        };

        let total = candidates.len();
        let mut files: Vec<FileRows> = Vec::new();
        let mut scanned = 0usize;
        // OM.A3: a source that stops answering must not cost the user the rows
        // already collected, and must not be silently absent either.
        // `Health` counts a source's consecutive failures; `dropped` is the
        // set that ran out of them, and it is what makes the terminal
        // headerline say "partial" instead of implying a complete agenda.
        let mut health: HashMap<u64, u32> = HashMap::new();
        let mut dropped: Vec<u64> = Vec::new();
        let mut skipped_files = 0usize;

        for chunk in candidates.chunks(READ_BATCH) {
            // The view may have been closed while the scan ran.
            if mb_registry.handle(view).is_none() {
                return;
            }
            let owned: Vec<PathBuf> = chunk.to_vec();
            let read = match tokio::task::spawn_blocking(move || read_batch(&owned)).await {
                Ok(read) => read,
                Err(e) => {
                    tracing::warn!(error = %e, "agenda: a read batch failed; skipping it");
                    continue;
                }
            };

            for (path, text) in read {
                scanned += 1;
                for source in &live {
                    let id = source.source_id();
                    if dropped.contains(&id) || !source.claims(&path) {
                        continue;
                    }
                    match source.scan(path.clone(), text.clone()).await {
                        Ok(entries) => {
                            // A file it could read resets the counter: the
                            // budget is for a source that has STOPPED
                            // answering, not one with a few bad files in a
                            // large project.
                            health.insert(id, 0);
                            if !entries.is_empty() {
                                files.push(FileRows {
                                    path: path.clone(),
                                    text: text.clone(),
                                    entries,
                                });
                            }
                        }
                        // One malformed file must not fail the agenda —
                        // `error-parser`'s rule, same failure class. `debug!`
                        // because a project-wide scan would flood `info!`.
                        Err(e) => {
                            skipped_files += 1;
                            let strikes = health.entry(id).or_insert(0);
                            *strikes += 1;
                            tracing::debug!(
                                path = %path.display(),
                                error = %e,
                                strikes = *strikes,
                                "agenda: a source could not scan a file; skipping it"
                            );
                            // A quarantined plugin errors on EVERY later call,
                            // so continuing to ask costs a channel round-trip
                            // per remaining file to learn nothing. Drop it and
                            // keep walking for the other sources.
                            if *strikes >= SOURCE_FAILURE_BUDGET {
                                tracing::warn!(
                                    source = id,
                                    "agenda: a source failed {SOURCE_FAILURE_BUDGET} files in a \
                                     row; dropping it from this scan (the agenda will be partial)"
                                );
                                dropped.push(id);
                            }
                        }
                    }
                }
            }

            if scanned % PROGRESS_INTERVAL < READ_BATCH
                && let Some(handle) = mb_registry.handle(view)
            {
                handle.set_headerline(HeaderlineStatus::InProgress {
                    label: format!("Building agenda ({scanned}/{total} files)"),
                    count: Some(files.iter().map(|f| f.entries.len()).sum()),
                    emphasis: None,
                });
            }
        }

        let outcome = ScanOutcome {
            files_scanned: scanned,
            skipped_files,
            dropped_sources: dropped.len(),
        };
        let sorted = sort_rows(&files);
        if sorted.is_empty() {
            finish_empty(&mb_registry, &events, view, &outcome);
        } else {
            append_sorted(&mb_registry, &events, view, &files, sorted, &outcome);
        }
    });
}

/// How many consecutive files a source may fail before the scan stops asking.
///
/// Small on purpose. The failure this defends against is a QUARANTINED plugin,
/// which errors on every call forever — three strikes distinguishes it from a
/// handful of malformed files without making a large project pay a channel
/// round-trip per file to keep confirming the same answer.
const SOURCE_FAILURE_BUDGET: u32 = 3;

/// What a finished scan has to be honest about.
///
/// Partial-and-honest beats empty-and-silent (`org-mode.md` §8) — but it also
/// beats *partial-and-silent*, which is what a bare row count would be: an
/// agenda missing a source's rows looks exactly like an agenda that had none.
#[derive(Debug, Default, Clone, Copy)]
struct ScanOutcome {
    files_scanned: usize,
    skipped_files: usize,
    dropped_sources: usize,
}

impl ScanOutcome {
    /// The `— partial: …` suffix, or empty when the scan was clean.
    fn caveat(&self) -> String {
        if self.dropped_sources > 0 {
            format!(
                " — partial: {} source(s) stopped responding",
                self.dropped_sources
            )
        } else if self.skipped_files > 0 {
            format!(" ({} file(s) skipped)", self.skipped_files)
        } else {
            String::new()
        }
    }
}

/// Collect the paths at least one source claims. Blocking by construction.
///
/// `ignore::Walk` respects `.gitignore` / `.ignore`, which is what stops an
/// agenda over a checkout from scanning `target/`.
fn walk_candidates(root: &Path, extensions: &[String], max_files: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in ignore::Walk::new(root) {
        if out.len() >= max_files {
            break;
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.into_path();
        if claims_any(&path, extensions) {
            out.push(path);
        }
    }
    out
}

/// Does any registered extension match `path`? The union test, so the walk
/// reads a file once even when two sources claim it.
fn claims_any(path: &Path, extensions: &[String]) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let lowered = ext.to_ascii_lowercase();
    extensions.iter().any(|e| *e == lowered)
}

/// Read a batch of files, dropping the ones that cannot be read.
///
/// A file that vanished between the walk and the read, or one that is not
/// UTF-8, is skipped rather than failing the batch.
fn read_batch(paths: &[PathBuf]) -> Vec<(PathBuf, String)> {
    paths
        .iter()
        .filter_map(|p| match std::fs::read_to_string(p) {
            Ok(text) => Some((p.clone(), text)),
            Err(e) => {
                tracing::debug!(path = %p.display(), error = %e, "agenda: could not read a file");
                None
            }
        })
        .collect()
}

/// Stable-sort every file's rows together on `sort_key`.
///
/// **Stable** matters: rows with the same key keep walk order, so two
/// same-day headlines in one file stay in the order they appear in the file
/// rather than shuffling between scans.
fn sort_rows(files: &[FileRows]) -> Vec<SortedRow> {
    let mut rows: Vec<SortedRow> = files
        .iter()
        .enumerate()
        .flat_map(|(i, f)| {
            f.entries
                .iter()
                .cloned()
                .map(move |entry| SortedRow { file: i, entry })
        })
        .collect();
    rows.sort_by_key(|r| r.entry.sort_key);
    rows
}

/// Build one source `Document` per contributing file and append the rows in
/// sorted order, titling the first row of each group run.
fn append_sorted(
    mb_registry: &MultibufferRegistryHandle,
    events: &Option<Arc<EventBus>>,
    view: BufferId,
    files: &[FileRows],
    rows: Vec<SortedRow>,
    outcome: &ScanOutcome,
) {
    let Some(handle) = mb_registry.handle(view) else {
        return;
    };

    // One source document per file, however many rows it contributed —
    // otherwise a file with five agenda entries would be opened five times
    // and an edit through one row would not be visible through the others.
    let mut source_ids: HashMap<usize, BufferId> = HashMap::new();
    for row in &rows {
        source_ids.entry(row.file).or_insert_with(|| {
            let f = &files[row.file];
            let id = BufferId::next();
            let document = DocumentBuilder::default()
                .with_text(&f.text)
                .with_path(f.path.clone())
                .build();
            let registry = Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
            let doc_handle = spawn_document(id, document, registry);
            handle.add_source(id, Arc::new(doc_handle) as Arc<dyn Document>);
            id
        });
    }

    let excerpts = build_excerpts(&rows, &source_ids);
    let count = excerpts.len();
    handle.append_excerpts(excerpts);

    handle.set_headerline(HeaderlineStatus::Complete {
        summary: format!(
            "[agenda] {count} row(s) in {} file(s){}",
            outcome.files_scanned,
            outcome.caveat()
        ),
        emphasis: None,
    });
    if let Some(events) = events {
        events.publish_typed(MultibufferExcerptsReady { view });
    }
}

/// Turn sorted rows into excerpts, giving the FIRST row of each group run its
/// `label` as a header and the rest an empty one.
///
/// An empty `ExcerptHeader.title` renders no header row, which is what makes
/// a date group show one header for N rows drawn from N different files
/// (§6.1). `path` is left `None` deliberately: the header renderer falls back
/// to the path when it has one, and an agenda groups by date, not by file.
fn build_excerpts(rows: &[SortedRow], source_ids: &HashMap<usize, BufferId>) -> Vec<Excerpt> {
    let mut out = Vec::with_capacity(rows.len());
    let mut current_group: Option<&str> = None;
    for row in rows {
        let Some(&source) = source_ids.get(&row.file) else {
            continue;
        };
        let starts_group = current_group != Some(row.entry.group.as_str());
        if starts_group {
            current_group = Some(row.entry.group.as_str());
        }
        let title = if starts_group {
            row.entry.label.clone()
        } else {
            String::new()
        };
        out.push(
            Excerpt::new(source, row.entry.line, row.entry.end_line)
                .with_header(ExcerptHeader::new(title)),
        );
    }
    out
}

/// The terminal headerline for a scan that produced nothing.
///
/// It still publishes [`MultibufferExcerptsReady`]: an empty agenda has to
/// repaint too, or the view keeps showing "Building agenda…" forever.
fn finish_empty(
    mb_registry: &MultibufferRegistryHandle,
    events: &Option<Arc<EventBus>>,
    view: BufferId,
    outcome: &ScanOutcome,
) {
    let Some(handle) = mb_registry.handle(view) else {
        return;
    };
    handle.set_headerline(HeaderlineStatus::Complete {
        summary: format!(
            "[agenda] nothing scheduled ({} file(s) scanned){}",
            outcome.files_scanned,
            outcome.caveat()
        ),
        emphasis: None,
    });
    if let Some(events) = events {
        events.publish_typed(MultibufferExcerptsReady { view });
    }
}

// ─────────────────────────────────────────────────────────────────
// The view's own mode
// ─────────────────────────────────────────────────────────────────

/// `agenda-view-mode` — the minor the provider activates on the agenda view.
///
/// The `ProjectSearchMode` shape (`org-mode.md` §4.2): `multibuffer-mode` is
/// the view's major, and the provider contributes a minor carrying what is
/// specific to *this* view.
///
/// ## Why this is native and not the plugin's `org-agenda-mode`
///
/// The design fragment gave `gr` to `org-agenda-mode`, which the plugin owns.
/// It cannot have it, for a reason that is structural rather than a matter of
/// taste: refreshing the agenda means re-running the HOST's walk, which is
/// `AppEffect::OpenProviderView` — and that effect's plugin surface is
/// **deliberately withheld** (`boundary_app_effect.rs`), pending the
/// capability model for which providers a plugin may trigger. A plugin
/// `gr` could bind the chord and not do the work.
///
/// It is also the better split on merit. Refreshing a host-built view is
/// host machinery: the second agenda-source plugin — the markdown TODO
/// scanner the whole `extensions()` design exists for — inherits `gr` here,
/// where under the fragment's version every agenda plugin would re-derive it.
/// That is the copied-keymap failure the minor-mode rule forbids, one layer
/// up. What stays the plugin's is what is genuinely org: acting on a TODO
/// state from the agenda, through `org-agenda-mode`'s own chords and its own
/// handler bodies. Both modes are active on the view at once; neither is a
/// half-migration.
pub struct AgendaViewMode;

/// The refresh body this view declares. Named, not anonymous, because
/// `refresh_action` returns a *target* and the handler below must supply it —
/// declaring one without the other is the gap `magit-project-diff` shipped
/// with and PD.9 had to come back for.
pub const REFRESH_ACTION: &str = "action:agenda-refresh";

impl AgendaViewMode {
    pub fn mode_id() -> lattice_mode::ModeId {
        lattice_mode::ModeId::new("agenda-view-mode")
    }
}

impl lattice_mode::Mode for AgendaViewMode {
    type Guard = ();

    fn id(&self) -> lattice_mode::ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> lattice_mode::ModeKind {
        lattice_mode::ModeKind::Minor
    }

    /// Manual: the provider activates it on the view it just built. An
    /// activation policy could not express "the buffer this provider made",
    /// and a policy keyed on `BufferKind::Multibuffer` would attach it to
    /// every search and diff view too.
    fn activation_policy(&self) -> lattice_mode::ActivationPolicy {
        lattice_mode::ActivationPolicy::Manual
    }

    /// Returning `Some` is the whole of the `gr` wiring: it pulls
    /// `refreshable-view-mode` in through the implies cascade, and that mode
    /// owns the chord. One line, no second thing to remember — which matters,
    /// because forgetting the second thing kills the chord silently.
    fn refresh_action(&self) -> Option<&'static str> {
        Some(REFRESH_ACTION)
    }

    /// The mode that declares the target also supplies the body. Leaving the
    /// handler to the host would be the half-migration the standing rule
    /// forbids.
    fn action_handlers(&self) -> Vec<lattice_mode::ActionHandlerContribution> {
        vec![lattice_mode::ActionHandlerContribution {
            action_name: REFRESH_ACTION,
            handler: Arc::new(|ctx: &lattice_mode::ActionContext<'_>| {
                let view = BufferId(ctx.buffer_id.0 as u32);
                // Re-open with the root this view already shows. Reading it
                // back matters: `gr` in an agenda over `~/notes` must not
                // silently turn it into an agenda over the current checkout.
                //
                // `Args::None` is not a fallback to "the project root" — the
                // opener itself prefers the open view's stored state, so an
                // absent service degrades to the same answer by one path
                // instead of two that can disagree.
                let args = ctx
                    .services
                    .get::<AgendaServiceHandle>()
                    .and_then(|svc| svc.state(view))
                    .and_then(|state| {
                        state
                            .read()
                            .ok()
                            .map(|s| Args::String(s.options.root.display().to_string()))
                    })
                    .unwrap_or(Args::None);
                Some(lattice_grammar::Effect::AppAction(
                    lattice_grammar::app_effect::AppEffect::OpenProviderView {
                        provider: PROVIDER_NAME.to_string(),
                        args,
                    },
                ))
            }),
        }]
    }

    fn on_activate(
        &self,
        _ctx: lattice_mode::ModeContext,
    ) -> lattice_mode::LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

// ─────────────────────────────────────────────────────────────────
// Boot integration
// ─────────────────────────────────────────────────────────────────

/// Register the view's minor mode.
pub fn register_agenda_mode(modes: &mut lattice_mode::ModeRegistry) {
    modes
        .register(AgendaViewMode)
        .expect("agenda-view-mode registers without conflict at boot");
}

/// Register `action:agenda-refresh` so the mode's `refresh_action` target
/// resolves. The body lives on the mode; this is the registry entry the host
/// looks the name up in.
pub fn register_agenda_actions(registry: &mut CommandRegistry) {
    use lattice_grammar::effect::Effect;
    use lattice_grammar::registry::ActionSpec;
    registry.register_action(
        REFRESH_ACTION,
        "Re-scan the agenda over the root this view already shows.",
        ActionSpec {
            // A dead body, like `refreshable-view-mode`'s own: the mode's
            // registered handler is what runs. This exists so the name
            // resolves.
            apply: Arc::new(|_| Ok(Effect::None)),
            args_schema: vec![],
        },
    );
}

/// Register the per-view state service.
pub fn register_agenda_service(services: &mut ServiceRegistry) {
    services.register(InMemoryAgendaService::handle());
}

/// Register the view opener on the generic provider-view seam.
///
/// This plus the ex-command below is the whole trigger surface. A missing
/// registry means the host did not publish the seam (an older boot, or a test
/// harness); logged and skipped, because refusing to boot over an unavailable
/// optional surface is the worse failure.
pub fn register_agenda_provider(services: &ServiceRegistry) {
    let Some(registry) = services.get::<lattice_mode::ProviderViewRegistryHandle>() else {
        tracing::debug!("agenda: no ProviderViewRegistry; `:agenda` will not be available");
        return;
    };
    if !registry.register(
        PROVIDER_NAME,
        Arc::new(|activator: &mut dyn ModeActivator, args: &Args| open_agenda(activator, args)),
    ) {
        tracing::warn!(
            provider = PROVIDER_NAME,
            "agenda: a provider view is already registered under this name"
        );
    }
}

/// Register the `:agenda [root]` ex-command.
///
/// One dashed name, no collapsed spelling, no new 1–2 letter short (the
/// ex-command naming rule). It is `agenda` rather than `org-agenda` because
/// nothing about the host's half is org: a markdown TODO scanner registering
/// an `agenda-source` appears in the same view.
pub fn register_agenda_ex_command(registry: &mut CommandRegistry) {
    use lattice_grammar::app_effect::AppEffect;
    use lattice_grammar::args::{ArgKind, ArgSpec};
    use lattice_grammar::command::LatencyClass;
    use lattice_grammar::effect::Effect;
    use lattice_grammar::registry::{ExCommandSpec, SurfaceForm};

    registry.register_ex_command(
        "agenda",
        "Open the agenda: every dated row plugin agenda-sources find under the project root, \
         grouped and ordered by the source, as editable excerpts. Pass a path to scan elsewhere.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            // The root rides as a plain string so the opener owns the parse —
            // the ex-command must not learn the provider's vocabulary.
            parse_args: Arc::new(|line: &str, _bang: bool| {
                let trimmed = line.trim();
                Ok(if trimmed.is_empty() {
                    Args::None
                } else {
                    Args::String(trimmed.to_string())
                })
            }),
            apply: Arc::new(|ctx| {
                Ok(Effect::AppAction(AppEffect::OpenProviderView {
                    provider: PROVIDER_NAME.to_string(),
                    args: ctx.args.clone(),
                }))
            }),
            args_schema: vec![ArgSpec::optional(
                "root",
                ArgKind::String,
                "directory to scan; defaults to the active buffer's project",
            )],
            surface_form: SurfaceForm::Keyword,
        },
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn entry(line: u32, group: &str, label: &str, sort_key: i64) -> AgendaEntry {
        AgendaEntry {
            line,
            end_line: line,
            group: group.to_string(),
            label: label.to_string(),
            sort_key,
        }
    }

    fn file(path: &str, entries: Vec<AgendaEntry>) -> FileRows {
        FileRows {
            path: PathBuf::from(path),
            text: "x\n".repeat(20),
            entries,
        }
    }

    /// The property the whole design turns on: rows from DIFFERENT files
    /// interleave by `sort_key`, so a date group can span files.
    #[test]
    fn rows_sort_across_files_not_within_them() {
        let files = vec![
            file("/p/a.org", vec![entry(1, "wed", "Wed", 30)]),
            file("/p/b.org", vec![entry(2, "mon", "Mon", 10)]),
            file("/p/c.org", vec![entry(3, "tue", "Tue", 20)]),
        ];
        let rows = sort_rows(&files);
        assert_eq!(
            rows.iter().map(|r| r.entry.sort_key).collect::<Vec<_>>(),
            vec![10, 20, 30]
        );
    }

    /// Equal keys keep walk order, so two same-day rows in one file do not
    /// shuffle between scans.
    #[test]
    fn equal_sort_keys_keep_walk_order() {
        let files = vec![file(
            "/p/a.org",
            vec![
                entry(5, "mon", "Mon", 10),
                entry(2, "mon", "Mon", 10),
                entry(9, "mon", "Mon", 10),
            ],
        )];
        let rows = sort_rows(&files);
        assert_eq!(
            rows.iter().map(|r| r.entry.line).collect::<Vec<_>>(),
            vec![5, 2, 9]
        );
    }

    fn ids(n: usize) -> HashMap<usize, BufferId> {
        (0..n).map(|i| (i, BufferId(i as u32 + 1))).collect()
    }

    /// §6.1's grouping mechanism: one header per run, the rest empty — which
    /// is how a date group drawn from three files renders one header.
    #[test]
    fn only_the_first_row_of_a_group_carries_a_header() {
        let files = vec![
            file("/p/a.org", vec![entry(1, "mon", "Monday", 10)]),
            file("/p/b.org", vec![entry(2, "mon", "Monday", 11)]),
            file("/p/c.org", vec![entry(3, "tue", "Tuesday", 20)]),
        ];
        let rows = sort_rows(&files);
        let excerpts = build_excerpts(&rows, &ids(3));
        let titles: Vec<&str> = excerpts.iter().map(|e| e.header.title.as_str()).collect();
        assert_eq!(titles, vec!["Monday", "", "Tuesday"]);
    }

    /// A group that reappears after another group gets a fresh header. It is
    /// a *run* test, not a seen-set test: a source is free to interleave, and
    /// suppressing the second "Monday" would silently merge two runs.
    #[test]
    fn a_group_that_recurs_after_another_gets_a_new_header() {
        let files = vec![file(
            "/p/a.org",
            vec![
                entry(1, "mon", "Monday", 10),
                entry(2, "tue", "Tuesday", 20),
                entry(3, "mon", "Monday", 30),
            ],
        )];
        let rows = sort_rows(&files);
        let excerpts = build_excerpts(&rows, &ids(1));
        let titles: Vec<&str> = excerpts.iter().map(|e| e.header.title.as_str()).collect();
        assert_eq!(titles, vec!["Monday", "Tuesday", "Monday"]);
    }

    /// Every row of one file points at ONE source document, or an edit made
    /// through one row would not be visible through its neighbour.
    #[test]
    fn rows_from_one_file_share_a_source() {
        let files = vec![file(
            "/p/a.org",
            vec![entry(1, "mon", "Monday", 10), entry(7, "mon", "Monday", 11)],
        )];
        let rows = sort_rows(&files);
        let excerpts = build_excerpts(&rows, &ids(1));
        assert_eq!(excerpts.len(), 2);
        assert_eq!(excerpts[0].source, excerpts[1].source);
    }

    /// The walk's cheapness guarantee: a file nobody claims is never even a
    /// candidate, so `:agenda` in a Rust checkout costs a directory walk.
    #[test]
    fn only_claimed_extensions_become_candidates() {
        let exts = vec!["org".to_string()];
        assert!(claims_any(Path::new("/p/notes.org"), &exts));
        assert!(claims_any(Path::new("/p/NOTES.ORG"), &exts));
        assert!(!claims_any(Path::new("/p/main.rs"), &exts));
        assert!(!claims_any(Path::new("/p/Makefile"), &exts));
    }

    #[test]
    fn a_tilde_root_expands_against_home() {
        // SAFETY-adjacent: only reads the var, and the assertion is relative
        // to whatever it is, so this does not depend on the test environment.
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let home = PathBuf::from(home);
        assert_eq!(
            shellexpand_tilde("~/notes"),
            home.join("notes").display().to_string()
        );
        assert_eq!(shellexpand_tilde("~"), home.display().to_string());
        assert_eq!(shellexpand_tilde("/abs/path"), "/abs/path");
    }

    // ─────────────────────────────────────────────────────────────
    // The scan, end to end against a fake producer
    // ─────────────────────────────────────────────────────────────

    /// Records the paths it was offered, so a test can assert what the walk
    /// did NOT read as well as what it did.
    #[derive(Debug)]
    struct FakeSource {
        id: u64,
        exts: Vec<String>,
        begins: Arc<std::sync::atomic::AtomicUsize>,
        offered: Arc<std::sync::Mutex<Vec<PathBuf>>>,
    }

    impl FakeSource {
        fn new(id: u64, exts: &[&str]) -> Self {
            Self {
                id,
                exts: exts.iter().map(|e| e.to_string()).collect(),
                begins: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                offered: Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }
    }

    impl lattice_mode::AsyncAgendaSource for FakeSource {
        fn source_id(&self) -> u64 {
            self.id
        }
        fn extensions(&self) -> &[String] {
            &self.exts
        }
        fn begin(&self) -> lattice_mode::AgendaBeginFuture<'_> {
            self.begins
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
        fn scan(&self, path: PathBuf, text: String) -> lattice_mode::AgendaFuture<'_> {
            if let Ok(mut o) = self.offered.lock() {
                o.push(path.clone());
            }
            Box::pin(async move {
                if text.contains("BROKEN") {
                    return Err("fake: malformed".to_string());
                }
                // One row per `* TODO <key>` line, keyed off the file so the
                // sort assertion is testing the producer's data.
                Ok(text
                    .lines()
                    .enumerate()
                    .filter_map(|(i, line)| {
                        let rest = line.strip_prefix("* TODO ")?;
                        let key: i64 = rest.trim().parse().ok()?;
                        Some(AgendaEntry {
                            line: i as u32,
                            end_line: i as u32,
                            group: format!("day-{key}"),
                            label: format!("Day {key}"),
                            sort_key: key,
                        })
                    })
                    .collect())
            })
        }
    }

    fn view_handle() -> (MultibufferRegistryHandle, BufferId) {
        let registry = crate::registry::InMemoryMultibufferRegistry::handle();
        let handle = Arc::new(crate::MultibufferDocumentHandle::empty(Arc::new(
            arc_swap::ArcSwap::from_pointee(CommandRegistry::new()),
        )));
        let view = handle.buffer_id();
        registry.insert(view, handle);
        (registry, view)
    }

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).expect("fixture write");
    }

    /// Poll until the spawned scan reaches a terminal headerline. The scan
    /// hops through `spawn_blocking` twice, so there is no single future to
    /// await from here.
    async fn settle_agenda(
        registry: &MultibufferRegistryHandle,
        view: BufferId,
    ) -> HeaderlineStatus {
        for _ in 0..400 {
            if let Some(h) = registry.handle(view) {
                let status = (*h.headerline()).clone();
                if matches!(
                    status,
                    HeaderlineStatus::Complete { .. } | HeaderlineStatus::Failed { .. }
                ) {
                    return status;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("the agenda scan never reached a terminal headerline");
    }

    fn tempdir() -> PathBuf {
        // Unique per call: a timestamp alone collides under parallel `cargo
        // test`, so a counter rides along ([[tempdir-helpers-need-a-counter]]).
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("lattice-agenda-{nanos}-{n}"));
        std::fs::create_dir_all(&dir).expect("tempdir");
        dir
    }

    /// The headline assertion: rows from three files land in one view, in the
    /// producer's global order, with one header per date group.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_scan_interleaves_rows_from_every_file_in_sort_order() {
        let dir = tempdir();
        write(&dir, "a.org", "* TODO 30\n");
        write(&dir, "b.org", "* TODO 10\n* TODO 30\n");
        write(&dir, "c.org", "* TODO 20\n");
        // Never offered: nothing claims `.rs`.
        write(&dir, "main.rs", "* TODO 1\n");

        let (registry, view) = view_handle();
        let source = Arc::new(FakeSource::new(1, &["org"]));
        let offered = source.offered.clone();

        spawn_agenda_scan(
            view,
            AgendaOptions {
                root: dir.clone(),
                max_files: None,
            },
            vec![source],
            registry.clone(),
            None,
        );

        let status = settle_agenda(&registry, view).await;
        let handle = registry.handle(view).unwrap();
        let excerpts = handle.excerpts();

        assert_eq!(
            excerpts.len(),
            4,
            "one row per `* TODO` line, got {status:?}"
        );
        // Sorted across files: 10 (b), 20 (c), 30 (a), 30 (b).
        assert_eq!(
            excerpts
                .iter()
                .map(|e| e.header.title.clone())
                .collect::<Vec<_>>(),
            vec![
                "Day 10".to_string(),
                "Day 20".to_string(),
                "Day 30".to_string(),
                // Same group as the row above it, drawn from a DIFFERENT file
                // — the property §6.1 turns on.
                String::new(),
            ]
        );

        let offered = offered.lock().unwrap().clone();
        assert!(
            !offered
                .iter()
                .any(|p| p.extension().and_then(|e| e.to_str()) == Some("rs")),
            "a file no source claims is never read, let alone crossed: {offered:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One malformed file must not fail the agenda: the other files' rows
    /// still land. `error-parser`'s rule, same failure class.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_file_the_source_rejects_is_skipped_and_the_scan_continues() {
        let dir = tempdir();
        write(&dir, "bad.org", "BROKEN\n");
        write(&dir, "good.org", "* TODO 5\n");

        let (registry, view) = view_handle();
        spawn_agenda_scan(
            view,
            AgendaOptions {
                root: dir.clone(),
                max_files: None,
            },
            vec![Arc::new(FakeSource::new(1, &["org"]))],
            registry.clone(),
            None,
        );

        settle_agenda(&registry, view).await;
        let excerpts = registry.handle(view).unwrap().excerpts();
        assert_eq!(excerpts.len(), 1);
        assert_eq!(excerpts[0].header.title, "Day 5");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An empty result still reaches a terminal headerline. Leaving it on
    /// "Building agenda…" forever is the failure this pins — the view would
    /// look permanently mid-scan.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_empty_agenda_still_finishes_its_headerline() {
        let dir = tempdir();
        write(&dir, "prose.org", "just some prose\n");

        let (registry, view) = view_handle();
        spawn_agenda_scan(
            view,
            AgendaOptions {
                root: dir.clone(),
                max_files: None,
            },
            vec![Arc::new(FakeSource::new(1, &["org"]))],
            registry.clone(),
            None,
        );

        match settle_agenda(&registry, view).await {
            HeaderlineStatus::Complete { summary, .. } => {
                assert!(summary.contains("nothing scheduled"), "got {summary}");
            }
            other => panic!("expected a Complete headerline, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `begin` runs once per scan, before any file — the contract the guest's
    /// per-scan state depends on.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn every_source_begins_exactly_once_per_scan() {
        let dir = tempdir();
        write(&dir, "a.org", "* TODO 1\n");
        write(&dir, "b.org", "* TODO 2\n");

        let (registry, view) = view_handle();
        let source = Arc::new(FakeSource::new(1, &["org"]));
        let begins = source.begins.clone();

        spawn_agenda_scan(
            view,
            AgendaOptions {
                root: dir.clone(),
                max_files: None,
            },
            vec![source],
            registry.clone(),
            None,
        );

        settle_agenda(&registry, view).await;
        assert_eq!(begins.load(std::sync::atomic::Ordering::SeqCst), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ─────────────────────────────────────────────────────────────
    // OM.A3 — the view mode and the partial-scan contract
    // ─────────────────────────────────────────────────────────────

    /// A source that errors on EVERY file — a quarantined plugin, which is
    /// the failure this defends against.
    #[derive(Debug)]
    struct DeadSource {
        exts: Vec<String>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl lattice_mode::AsyncAgendaSource for DeadSource {
        fn source_id(&self) -> u64 {
            99
        }
        fn extensions(&self) -> &[String] {
            &self.exts
        }
        fn begin(&self) -> lattice_mode::AgendaBeginFuture<'_> {
            Box::pin(async { Ok(()) })
        }
        fn scan(&self, _p: PathBuf, _t: String) -> lattice_mode::AgendaFuture<'_> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async { Err("quarantined".to_string()) })
        }
    }

    /// §8's partial-and-honest rule, with the second half that a bare row
    /// count would lose: an agenda missing a source's rows looks exactly like
    /// an agenda that never had any, so the headerline has to SAY it is
    /// partial.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_source_that_stops_answering_leaves_partial_rows_and_an_honest_headerline() {
        let dir = tempdir();
        for i in 0..10 {
            write(&dir, &format!("n{i}.org"), "* TODO 1\n");
        }

        let (registry, view) = view_handle();
        let dead = Arc::new(DeadSource {
            exts: vec!["org".to_string()],
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        let calls = dead.calls.clone();

        spawn_agenda_scan(
            view,
            AgendaOptions {
                root: dir.clone(),
                max_files: None,
            },
            // The healthy source still contributes: one bad producer must not
            // cost the user the other's rows.
            vec![Arc::new(FakeSource::new(1, &["org"])), dead],
            registry.clone(),
            None,
        );

        let status = settle_agenda(&registry, view).await;
        assert_eq!(
            registry.handle(view).unwrap().excerpts().len(),
            10,
            "the healthy source's rows survived"
        );
        match status {
            HeaderlineStatus::Complete { summary, .. } => assert!(
                summary.contains("partial") && summary.contains("stopped responding"),
                "the headerline must say the agenda is incomplete, got {summary}"
            ),
            other => panic!("expected a Complete headerline, got {other:?}"),
        }

        // …and it stopped asking. A quarantined plugin answers the same way
        // forever, so continuing costs a channel round-trip per remaining
        // file to learn nothing.
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            SOURCE_FAILURE_BUDGET as usize,
            "the scan gave up after the budget rather than asking all ten"
        );
    }

    /// A few bad files in a large project is NOT a dead source. The budget
    /// counts CONSECUTIVE failures, so a good file in between resets it —
    /// otherwise a project with three malformed org files anywhere in it
    /// would silently lose its agenda.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scattered_bad_files_do_not_drop_a_healthy_source() {
        let dir = tempdir();
        // Alternating, so no three failures ever land in a row.
        for i in 0..8 {
            if i % 2 == 0 {
                write(&dir, &format!("bad{i}.org"), "BROKEN\n");
            } else {
                write(&dir, &format!("good{i}.org"), "* TODO 1\n");
            }
        }

        let (registry, view) = view_handle();
        spawn_agenda_scan(
            view,
            AgendaOptions {
                root: dir.clone(),
                max_files: None,
            },
            vec![Arc::new(FakeSource::new(1, &["org"]))],
            registry.clone(),
            None,
        );

        let status = settle_agenda(&registry, view).await;
        assert_eq!(
            registry.handle(view).unwrap().excerpts().len(),
            4,
            "every good file still contributed"
        );
        match status {
            HeaderlineStatus::Complete { summary, .. } => {
                assert!(
                    !summary.contains("stopped responding"),
                    "a source with scattered bad files is alive, got {summary}"
                );
                assert!(
                    summary.contains("4 file(s) skipped"),
                    "…but the skips are still reported, got {summary}"
                );
            }
            other => panic!("expected a Complete headerline, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `gr` has to have a target AND a body. Declaring the first and leaving
    /// the second to the host is the half-migration the standing rule
    /// forbids — and it is exactly what `magit-project-diff` shipped with,
    /// where the chord resolved to nothing and failed silently.
    #[test]
    fn gr_resolves_to_this_views_own_refresh() {
        use lattice_mode::Mode;
        let m = AgendaViewMode;
        assert_eq!(m.kind(), lattice_mode::ModeKind::Minor);
        assert!(
            matches!(
                m.activation_policy(),
                lattice_mode::ActivationPolicy::Manual
            ),
            "the provider activates it on the view it built; no policy can say that"
        );
        // The cascade keys on `refresh_action` being `Some`, NOT on an
        // `implies()` entry — deliberately, so a forgotten list entry cannot
        // kill the chord as silently as the three copied `gr` keymaps it
        // replaced (RV.1). One line is the whole contract, and this is it.
        assert_eq!(
            m.refresh_action(),
            Some(REFRESH_ACTION),
            "the chord arrives through the cascade because this is Some"
        );
        assert!(
            m.action_handlers()
                .iter()
                .any(|c| c.action_name == REFRESH_ACTION),
            "…whose body this mode supplies"
        );
    }

    #[test]
    fn service_state_roundtrips_and_clears() {
        let svc = InMemoryAgendaService::new();
        let view = BufferId(3);
        assert!(svc.state(view).is_none());
        svc.set_state(
            view,
            AgendaState {
                options: AgendaOptions {
                    root: PathBuf::from("/p"),
                    max_files: Some(10),
                },
            },
        );
        let got = svc.state(view).unwrap();
        assert_eq!(got.read().unwrap().options.root, PathBuf::from("/p"));
        svc.clear(view);
        assert!(svc.state(view).is_none());
    }
}
