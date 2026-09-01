//! OM.A1 (2026-08-25): the **agenda provider** — a date-grouped multibuffer
//! of rows contributed by plugin `scanned-excerpt-source` producers.
//!
//! Design: [`org-mode.md`](../../../../docs/dev/architecture/org-mode.md) §6.
//! Slice plan: `slice-plans/org-mode.md` OM.A1–A3.
//!
//! ## Nothing here knows what an org file is
//!
//! That is the point. The provider walks the project, asks the
//! [`ScannedExcerptSourceRegistry`](lattice_mode::ScannedExcerptSourceRegistry) which sources
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
    ModeActivator, ProviderViewOutcome, ScannedExcerpt, ScannedExcerptSource,
    ScannedExcerptSourceRegistryHandle, ServiceRegistry,
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
#[derive(Debug, Clone, Default)]
pub struct AgendaOptions {
    /// AF.2: the paths to scan — each a FILE or a DIRECTORY. A directory is
    /// walked; a file is taken as given, without asking whether any source
    /// claimed its extension, because naming a file IS the claim.
    ///
    /// **Empty means "not resolved yet", not "scan nothing".** The scan then
    /// asks every live source for its own roots and falls back to the project
    /// root if none answers. It cannot be resolved here, because a source's
    /// answer comes from the guest and the opener runs on the dispatch path.
    ///
    /// That is why `Default` is now empty where it used to be the project
    /// root: the fallback moved to the one place that can see every source's
    /// answer, rather than being baked in before any of them was asked.
    pub roots: Vec<PathBuf>,
    /// OA.11a: the view's own scan arguments, handed to every source's `begin`
    /// **uninterpreted**. The host routes these; it never reads them.
    ///
    /// Separate from `roots` because they have separate owners. The host must
    /// understand a root — it does the walk — and must not need to understand
    /// this: it is the source's own vocabulary (org's agenda dispatcher names
    /// which custom command to run). Folding the two together means a command
    /// key gets taken for a directory path and the scan silently covers
    /// nothing.
    ///
    /// Sticky across a refresh exactly as `roots` is, and for the same reason:
    /// `gr` re-enters through this path, so an agenda opened for one command
    /// must not quietly revert to the default one. Replaced only when a new
    /// open supplies its own.
    pub scan_args: Vec<String>,
    /// Cap on files *offered to a source* (not files walked). `None` =
    /// unlimited. A bound exists because the walk is unattended: a user who
    /// runs the agenda from `$HOME` should get a slow answer, not a hung one.
    ///
    /// Applies to the UNION across roots, not per root — a cap that reset for
    /// each configured path would not be a bound.
    pub max_files: Option<usize>,
}

impl AgendaOptions {
    /// The directory a view's `:files` / `:search` should answer for.
    ///
    /// The first configured root when there is one, else the project root. A
    /// source-supplied root cannot land here: the scope is set when the view
    /// opens and those are resolved in the scan. Recorded rather than papered
    /// over — the consequence is that `:files` from an agenda opened with no
    /// argument answers for the project, which is also what it did before AF.2.
    fn scope_dir(&self) -> PathBuf {
        self.roots
            .first()
            .cloned()
            .unwrap_or_else(project_root_from_cwd)
    }
}

/// The root the agenda falls back to when nothing else names one.
fn project_root_from_cwd() -> PathBuf {
    lattice_core::project::root_from_cwd().unwrap_or_else(|| PathBuf::from("."))
}

/// Per-view scan state. OM.A3's `gr` reads the root back out of this so a
/// refresh re-scans what the view already shows rather than resetting to the
/// current working directory.
#[derive(Debug, Clone)]
pub struct AgendaState {
    pub options: AgendaOptions,
    /// OA.14b: every clocked span the last completed scan saw, across every
    /// file and source, unfiltered by date.
    ///
    /// Held per view rather than recomputed because the report's RANGE is a
    /// display choice — `gD` switches between day, week, month and year — and
    /// re-walking the corpus to answer a question the data already contains
    /// would make a toggle cost a scan. `org-agenda-clockreport-mode` (OA.16)
    /// filters on `day` and rolls the totals up each span's outline path.
    ///
    /// Written once at the end of a scan, not incrementally: a half-filled
    /// report is a wrong report, and unlike rows — which stream so the view
    /// fills in — nobody is reading a total until it is a total.
    pub clock: Vec<lattice_mode::ClockSpan>,
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
/// MV.3 — the identity of a scan-driven view: which provider name, which
/// buffer, which minor to activate.
///
/// The agenda used to hold these as module constants, which is precisely what
/// made the view un-ownable by the plugin whose feature it is. They are a
/// parameter now, so the same machinery serves the agenda and any
/// plugin-declared `scan` view; only the names differ.
///
/// The machinery itself does NOT move to the guest and is not meant to: the
/// bounded walk, the batched reads, the read-and-parse-once handoff, the stable
/// sort and the group-run computation are measured host work that no plugin
/// should reimplement. What moves is who says what the view is called.
#[derive(Debug, Clone)]
pub struct ScanViewIdentity {
    /// The provider name, used in messages and in the headerline prefix.
    pub provider: String,
    /// The view's buffer name.
    pub buffer_name: String,
    /// A minor the owner wants on the view, beyond the host's own refresh mode.
    pub view_mode: Option<String>,
    /// What to say when no source produces rows for this view.
    ///
    /// A field rather than a generic sentence because the message is
    /// USER-VISIBLE behaviour, and MV.3 is a migration of ownership, not of
    /// behaviour. Parameterising the wording along with the prefix silently
    /// reworded the agenda's decline; `agenda_declines_when_no_plugin_provides_rows`
    /// caught it, which is exactly the job the "existing tests pass unedited"
    /// rule was given.
    pub no_rows_message: String,
}

impl ScanViewIdentity {
    /// The agenda's own identity — the constants this type replaced, kept in
    /// one place so `:agenda` and org's declaration cannot drift apart.
    pub fn agenda() -> Self {
        Self {
            provider: PROVIDER_NAME.to_string(),
            buffer_name: VIEW_NAME.to_string(),
            view_mode: None,
            no_rows_message: "no plugin provides agenda rows".to_string(),
        }
    }
}

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
    open_scan_view(activator, &ScanViewIdentity::agenda(), args)
}

/// OA.11a: split a trigger's arguments into the host's slot and the guest's.
///
/// The arguments are **positional**: index 0 is the root override, which the
/// host interprets because it does the walk, and everything after it is the
/// source's own vocabulary, which the host must not read. `Args::String` is
/// the one-argument spelling and still means a root, so every trigger that
/// predates this slice — `:org-agenda`, `:org-agenda ~/notes` — arrives here
/// unchanged.
///
/// Returns the root already trimmed; empty means "no override", which is how a
/// caller names scan args without naming a root.
fn split_view_args(args: &Args) -> (String, Vec<String>) {
    let as_str = |v: &lattice_grammar::args::ArgValue| match v {
        lattice_grammar::args::ArgValue::String(s) => s.clone(),
        // The boundary admits only strings here, so this is unreachable from a
        // plugin. A native caller passing something else gets the slot treated
        // as absent rather than stringified into a path nobody typed.
        _ => String::new(),
    };
    match args {
        Args::String(s) => (s.trim().to_string(), Vec::new()),
        Args::List(values) => {
            let mut it = values.iter().map(as_str);
            let root = it.next().unwrap_or_default().trim().to_string();
            (root, it.collect())
        }
        _ => (String::new(), Vec::new()),
    }
}

/// MV.3: the agenda's opener, with its identity as a parameter.
///
/// Byte-for-byte the previous `open_agenda` body except that the four names it
/// used to hard-code now come from `identity`. Kept that way on purpose: the
/// agenda is the first thing ever to run through the plugin-view seam (MV.2 was
/// dropped), so the migration must be a rename of who-decides, not a rewrite of
/// what-happens. Every existing agenda test passes unedited, which is the only
/// signal separating "ownership moved" from "behaviour moved".
pub fn open_scan_view(
    activator: &mut dyn ModeActivator,
    identity: &ScanViewIdentity,
    args: &Args,
) -> ProviderViewOutcome {
    let name = identity.provider.as_str();
    let services = activator.services();

    let Some(sources) = services.get::<ScannedExcerptSourceRegistryHandle>() else {
        return ProviderViewOutcome::Declined {
            message: format!(
                "{name}: no scanned-excerpt-source registry; the plugin host is not wired"
            ),
        };
    };
    let snapshot = sources.load();
    if snapshot.is_empty() {
        // Not an error — it is the honest state of an editor with no agenda
        // plugin installed. Opening an empty view and leaving the user to
        // guess why is the worse UX (the `Declined` contract).
        return ProviderViewOutcome::Declined {
            message: format!("{name}: {}", identity.no_rows_message),
        };
    }

    let Some(registry) = services.get::<CommandRegistryHandle>() else {
        return ProviderViewOutcome::Declined {
            message: format!("{name}: command registry unavailable; cannot open the view"),
        };
    };
    let lang_registry = services
        .get::<Arc<lattice_syntax::LangRegistry>>()
        .map(|h| (*h).clone());

    // Read before any `&mut` use of the activator: `snapshot` borrows the
    // registry's ArcSwap guard, and the activation calls below need the
    // activator mutably.
    let view_modes = snapshot.view_modes();

    let existing = existing_view(&services, &identity.buffer_name);

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
    let (root_arg, scan_args) = split_view_args(args);
    if !root_arg.is_empty() {
        // An explicit argument REPLACES the list rather than joining it:
        // `:org-agenda ~/notes` means "this, instead of what I configured",
        // which is what makes the argument an escape hatch and not a filter.
        options.roots = vec![PathBuf::from(shellexpand_tilde(&root_arg))];
    }
    if !scan_args.is_empty() {
        options.scan_args = scan_args;
    }

    let view = match existing {
        Some(view) => view,
        None => create_multibuffer_view(
            activator,
            HashMap::new(),
            Vec::new(),
            Some(identity.buffer_name.clone()),
            BufferFlags::default(),
            (*registry).clone(),
            lang_registry,
            // AF.1: the agenda groups by DATE and SECTION, and its rows
            // interleave across files on purpose (OM.A2) — so a file is the
            // one thing that is not a contiguous run here. Folding by source
            // file would make `home.org`'s fold swallow every `work.org` row
            // sitting between its earliest and latest entry.
            crate::FoldGrouping::HeaderRuns,
        ),
    };

    let Some(mb_registry) = services.get::<MultibufferRegistryHandle>() else {
        return ProviderViewOutcome::Declined {
            message: format!("{name}: multibuffer registry unavailable; cannot open the view"),
        };
    };
    let Some(handle) = mb_registry.handle(view) else {
        return ProviderViewOutcome::Declined {
            message: format!("{name}: the view failed to open"),
        };
    };

    if let Some(svc) = services.get::<AgendaServiceHandle>() {
        svc.set_state(
            view,
            AgendaState {
                options: options.clone(),
                // The scan that is about to run fills this; carrying the
                // PREVIOUS scan's spans forward would be worse than empty,
                // since a stale total reads exactly like a current one.
                clock: Vec::new(),
            },
        );
    } else {
        tracing::debug!("agenda: service not registered; the view's root will not be tracked");
    }

    // OA.0c: the view is NOT emptied here. The scan collects every file,
    // sorts once and writes its rows in a single terminal call, so clearing
    // up front left the view blank for the WHOLE scan — which is why a slow
    // scan read as "refresh is broken" rather than as "refresh is slow". A
    // refresh now shows the previous rows, marked in-progress, until the new
    // ones replace them atomically (`append_sorted`) or a genuinely empty
    // result clears them (`finish_empty`).
    //
    // A first open has nothing to keep, so this costs it nothing.
    handle.set_headerline(HeaderlineStatus::InProgress {
        label: "Building agenda".to_string(),
        count: Some(0),
        emphasis: None,
    });

    // The root this view scanned, so `:files` / `:search` from inside the
    // agenda answer for the project the rows came from rather than for the
    // process working directory. The view has no path of its own.
    activator.set_buffer_scope_dir(view, options.scope_dir());

    // OM.A3: the view's own minor. `gr` arrives through the implies cascade
    // (`refresh_action` returns `Some`), so this one call is the whole of the
    // wiring — the same shape `project_search` uses for `ProjectSearchMode`.
    activator.activate_minor_by_id(view, AgendaViewMode::mode_id());

    // MV.3: the view OWNER's minor, when it declared one. Distinct from the
    // per-source minors below: this one belongs to whoever owns the view, those
    // belong to whoever produced its rows. The agenda declares none — its
    // interactions come through its sources' `view-mode`, which is how
    // `org-agenda-mode` reaches it today and continues to.
    if let Some(mode) = identity.view_mode.as_deref() {
        activator.activate_minor_by_id(view, lattice_mode::ModeId::new(mode));
    }

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
        Some(Arc::clone(&services)),
    );

    ProviderViewOutcome::Opened {
        view,
        message: Some(format!("{name}: scanning…")),
    }
}

/// The already-open agenda view, if there is one.
fn existing_view(services: &ServiceRegistry, view_name: &str) -> Option<BufferId> {
    let buffers = services.get::<lattice_mode::BufferStoreHandle>()?;
    let id = buffers.find_by_name(view_name)?;
    let registry = services.get::<MultibufferRegistryHandle>()?;
    registry.handle(id).map(|_| id)
}

/// `~` expansion for a hand-typed root.
///
/// Delegates to [`lattice_core::home::expand_tilde`]. It used to resolve the
/// home directory as `std::env::var_os("HOME")`, which is POSIX-only — so on
/// Windows `~/notes` stayed verbatim, found nothing, and the agenda reported an
/// empty corpus while this option's own documentation promised "`~` is
/// expanded".
fn shellexpand_tilde(raw: &str) -> String {
    lattice_core::home::expand_tilde(raw)
}

// ─────────────────────────────────────────────────────────────────
// The scan
// ─────────────────────────────────────────────────────────────────

/// One file's contribution: its text (kept so the source `Document` is built
/// from the SAME bytes the guest saw) and the rows found in it.
struct FileRows {
    path: PathBuf,
    text: String,
    entries: Vec<ScannedExcerpt>,
}

/// A row after the cross-file sort, carrying the index of its file.
struct SortedRow {
    file: usize,
    entry: ScannedExcerpt,
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
#[allow(clippy::too_many_arguments)]
pub fn spawn_agenda_scan(
    view: BufferId,
    options: AgendaOptions,
    sources: Vec<Arc<dyn ScannedExcerptSource>>,
    mb_registry: MultibufferRegistryHandle,
    events: Option<Arc<EventBus>>,
    // OA.5: `services` because the scan carries per-row style spans back, and
    // publishing them needs two things the walk itself does not — the
    // synthetic-highlight sink and the theme that resolves a slot name. A
    // registry rather than two handles: the scan runs long after the opener
    // returned, and a third consumer would otherwise mean a third parameter.
    services: Option<Arc<lattice_mode::ServiceRegistry>>,
) {
    tokio::spawn(async move {
        // `begin` first, and a source that refuses is dropped from THIS scan
        // rather than failing it: its per-scan state is now unknown, so its
        // rows would be untrustworthy, but the other sources' are fine.
        let mut live: Vec<Arc<dyn ScannedExcerptSource>> = Vec::with_capacity(sources.len());
        for source in sources {
            match source.begin(&options.scan_args).await {
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

        // AF.2: resolve the roots, most specific first.
        //
        //   1. what the caller named (an explicit argument, or the open view's
        //      own stored root on `gr`);
        //   2. what the live sources ask for — their users' configuration;
        //   3. the project root, which is what an editor with no agenda
        //      configuration has always scanned.
        //
        // Asked here rather than at open because a source's answer comes from
        // the guest, and the opener runs on the dispatch path where a guest
        // call does not belong.
        let mut roots = options.roots.clone();
        if roots.is_empty() {
            for source in &live {
                match source.roots().await {
                    Ok(named) => roots.extend(
                        named
                            .iter()
                            .map(|r| PathBuf::from(shellexpand_tilde(r.trim())))
                            .filter(|p| !p.as_os_str().is_empty()),
                    ),
                    // A source that cannot say where to look must not be able
                    // to make the agenda scan nothing.
                    Err(e) => tracing::debug!(
                        source = source.source_id(),
                        error = %e,
                        "agenda: a source could not name its roots; ignoring it"
                    ),
                }
            }
            roots.sort();
            roots.dedup();
        }
        if roots.is_empty() {
            roots.push(project_root_from_cwd());
        }

        let max_files = options.max_files.unwrap_or(usize::MAX);
        let extensions: Vec<String> = live
            .iter()
            .flat_map(|s| s.extensions().iter().cloned())
            .collect();

        let walk_roots = roots.clone();
        let candidates = match tokio::task::spawn_blocking(move || {
            collect_candidates(&walk_roots, &extensions, max_files)
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
        // OA.14b: every clocked span the walk saw, across every file and every
        // source. Accumulated flat rather than per file because the report
        // groups by outline and day, not by which file a span came from — and
        // a headline's time is its own wherever it lives.
        let mut clock: Vec<lattice_mode::ClockSpan> = Vec::new();
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
                        Ok(result) => {
                            // A file it could read resets the counter: the
                            // budget is for a source that has STOPPED
                            // answering, not one with a few bad files in a
                            // large project.
                            health.insert(id, 0);
                            // OA.14b: clock spans are collected whether or not
                            // the file produced any ROWS. A file whose only
                            // headline is untagged and undated contributes no
                            // agenda row and can still hold a week of clocked
                            // time — dropping it with the rows is exactly the
                            // under-reporting this seam exists to avoid.
                            if !result.clock.is_empty() {
                                clock.extend(result.clock);
                            }
                            if !result.entries.is_empty() {
                                files.push(FileRows {
                                    path: path.clone(),
                                    text: text.clone(),
                                    entries: result.entries,
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
        // OA.14b: publish the clock spans the walk collected, at the END and in
        // one write. A report is a total; a half-filled one is simply wrong,
        // and unlike rows — which stream so the view fills in — nothing reads
        // this until the scan has finished.
        //
        // Published even when the scan produced no ROWS: a corpus can hold a
        // week of clocked time and not a single agenda row, and that is
        // precisely the case a report has to answer.
        if let Some(services) = services.as_deref()
            && let Some(svc) = services.get::<AgendaServiceHandle>()
            && let Some(state) = svc.state(view)
            && let Ok(mut state) = state.write()
        {
            state.clock = clock;
        }
        let sorted = sort_rows(&files);
        if sorted.is_empty() {
            finish_empty(&mb_registry, &events, view, &outcome);
        } else {
            append_sorted(
                &mb_registry,
                &events,
                view,
                &files,
                sorted,
                &outcome,
                services.as_deref(),
            );
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
/// AF.2: every candidate across `roots`, capped at `max_files` in TOTAL.
///
/// A root that is a FILE is taken as given without the extension test — naming
/// a file is the claim, and a user who writes `~/notes/birthdays.txt` in their
/// agenda files meant it. A root that is a directory is walked as before.
///
/// A root that does not exist is skipped at `info!`: user-actionable (it is
/// their configuration), and one bad entry must not fail the agenda, which is
/// the same rule the per-file reads already follow.
///
/// De-duplicated, because a configured file inside a configured directory is an
/// ordinary way to write "everything here, and that one too" and must not scan
/// twice.
fn collect_candidates(roots: &[PathBuf], extensions: &[String], max_files: usize) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for root in roots {
        if out.len() >= max_files {
            break;
        }
        let meta = match std::fs::metadata(root) {
            Ok(meta) => meta,
            Err(error) => {
                tracing::info!(
                    path = %root.display(),
                    %error,
                    "agenda: a configured path could not be read; skipping it"
                );
                continue;
            }
        };
        let found = if meta.is_file() {
            vec![root.clone()]
        } else {
            walk_candidates(root, extensions, max_files - out.len())
        };
        for path in found {
            let key = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if seen.insert(key) {
                out.push(path);
            }
            if out.len() >= max_files {
                break;
            }
        }
    }
    // No silent truncation: a short agenda that looks complete is worse than a
    // slow one, so the cap says what it dropped.
    if out.len() >= max_files {
        tracing::info!(max_files, "agenda: hit the file cap; the agenda is partial");
    }
    out
}

/// The files a configured directory names — **one level, not the subtree**.
///
/// OA.0d. This walked recursively, which no configuration asked for: emacs
/// expands a directory in `org-agenda-files` with `directory-files`, one
/// level, and a user who wants a subdirectory lists it. An org directory with
/// `roam/`, `journal/` or `archive/` under it was pulling all of them into
/// every scan — wrong rows, and a corpus far larger than the one configured.
///
/// No new option to express this. The roots list is already the mechanism:
/// naming a subdirectory opts it in, which is exactly how emacs users do it
/// and why emacs never needed a recursion flag either.
///
/// `max_depth(1)` rather than `read_dir` so `ignore`'s hidden-file and
/// `.gitignore` filtering still applies — `.git` and ignored files stay out,
/// which is the one thing the recursive walk was doing right.
fn walk_candidates(root: &Path, extensions: &[String], max_files: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in ignore::WalkBuilder::new(root).max_depth(Some(1)).build() {
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
#[allow(clippy::too_many_arguments)]
fn append_sorted(
    mb_registry: &MultibufferRegistryHandle,
    events: &Option<Arc<EventBus>>,
    view: BufferId,
    files: &[FileRows],
    rows: Vec<SortedRow>,
    outcome: &ScanOutcome,
    services: Option<&lattice_mode::ServiceRegistry>,
) {
    let Some(handle) = mb_registry.handle(view) else {
        return;
    };

    // One source document per file, however many rows it contributed —
    // otherwise a file with five agenda entries would be opened five times
    // and an edit through one row would not be visible through the others.
    let mut source_ids: HashMap<usize, BufferId> = HashMap::new();
    let mut sources: HashMap<BufferId, Arc<dyn Document>> = HashMap::new();
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
            let doc = Arc::new(doc_handle) as Arc<dyn Document>;
            // `add_source` is not redundant with the map below. It is what
            // derives the per-excerpt `SyntaxHandle` from the path, and
            // `replace_excerpts` only swaps the source map — so building the
            // map alone leaves every agenda row uncoloured. Caught by
            // `agenda_rows_carry_per_excerpt_syntax_handles`, which is the
            // test AH.1 left behind for exactly this.
            handle.add_source(id, Arc::clone(&doc));
            sources.insert(id, doc);
            id
        });
    }

    let excerpts = build_excerpts(&rows, &source_ids);
    let count = excerpts.len();
    publish_row_spans(services, view, &rows, &excerpts);
    // OA.0c: REPLACE, not append. This is the swap that makes keeping the old
    // rows safe — appending onto a view we deliberately did not clear would
    // show the previous scan's rows and this one's together. Replacing also
    // drops the previous scan's source documents, which an append would leak
    // into the view for as long as it stayed open.
    //
    // (`state.source_syntax` is NOT re-baselined by `replace_excerpts`, so a
    // dropped source's handle outlives it. Pre-existing — the old
    // clear-then-append path had the same hole — and untouched here.)
    handle.replace_excerpts(sources, excerpts);

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

/// OA.5: paint the rows with the colour the source asked for.
///
/// Without this an agenda row is coloured by the SOURCE FILE's tree-sitter
/// grammar — all the host has — so the view looks like org text that happens
/// to be out of order. Which word is a TODO keyword, a priority or a tag is
/// org semantics, not the org grammar's.
///
/// Two translations happen here, and both are the reason this is host-side.
/// A producer reports offsets into its row's OWN line, because it cannot know
/// where that row lands until every other file's rows have been interleaved
/// by the sort; the composed row index is only knowable after
/// `build_excerpts`. And a `slot` is a NAME, resolved through exactly the path
/// a `highlights.scm` capture takes — so a plugin's own registered element
/// (`org.todo.WAITING`) picks up the active colourscheme, and an unresolvable
/// name renders unstyled rather than failing the row.
///
/// Silent no-op when no source asked for colour, which is the ordinary case:
/// an empty publish would still cost a drain and a repaint.
fn publish_row_spans(
    services: Option<&lattice_mode::ServiceRegistry>,
    view: BufferId,
    rows: &[SortedRow],
    excerpts: &[Excerpt],
) {
    let Some(services) = services else {
        return;
    };
    if rows.iter().all(|r| r.entry.spans.is_empty()) {
        return;
    }
    let Some(sink) = services.get::<lattice_mode::PendingSyntheticHighlights>() else {
        tracing::debug!("agenda: no synthetic-highlight sink; rows keep grammar colour");
        return;
    };
    let theme = services.get::<lattice_theme::ThemeRegistryHandle>();
    let theme_ref: Option<&dyn lattice_theme::ThemeRegistry> = theme
        .as_deref()
        .map(|t| &**t as &dyn lattice_theme::ThemeRegistry);

    sink.store_and_wake(view, composed_row_spans(rows, excerpts, theme_ref));
}

/// The translation itself, pure so it can be tested without an Editor — the
/// sink's map is private and only the Editor drains it.
///
/// Two things to get right, and both fail INVISIBLY: the rows still render,
/// just wrongly coloured.
///
/// - A span's offsets stay relative to its own LINE and are not rebased.
/// - A row lands at its COMPOSED index, which is not its index among its own
///   file's rows — the sort interleaves files, so the two differ whenever
///   more than one file contributes.
fn composed_row_spans(
    rows: &[SortedRow],
    excerpts: &[Excerpt],
    theme: Option<&dyn lattice_theme::ThemeRegistry>,
) -> Vec<Vec<lattice_cells::StyledSpan>> {
    // `excerpt_start_rows` is the same helper the fold providers use, so a
    // row's spans and its fold agree on where that row is.
    let starts = crate::motions::excerpt_start_rows(excerpts);
    let total: usize = excerpts.iter().map(|e| e.line_count() as usize).sum();
    let mut out: Vec<Vec<lattice_cells::StyledSpan>> = vec![Vec::new(); total];
    for (row, &start) in rows.iter().zip(starts.iter()) {
        let Some(slot) = out.get_mut(start as usize) else {
            continue;
        };
        for span in &row.entry.spans {
            slot.push(lattice_cells::StyledSpan {
                start: span.start as usize,
                end: span.end as usize,
                style: lattice_syntax::style::name_to_style_with_theme(&span.slot, theme),
            });
        }
    }
    out
}

/// The terminal state for a scan that produced nothing.
///
/// It still publishes [`MultibufferExcerptsReady`]: an empty agenda has to
/// repaint too, or the view keeps showing "Building agenda…" forever.
///
/// OA.0c: and it must CLEAR. Since `open_scan_view` stopped emptying the view
/// up front, this is the only thing standing between "you have nothing
/// scheduled" and a refresh that silently keeps showing yesterday's rows —
/// the worse of the two failures, because stale rows look exactly like
/// correct ones.
fn finish_empty(
    mb_registry: &MultibufferRegistryHandle,
    events: &Option<Arc<EventBus>>,
    view: BufferId,
    outcome: &ScanOutcome,
) {
    let Some(handle) = mb_registry.handle(view) else {
        return;
    };
    handle.replace_excerpts(HashMap::new(), Vec::new());
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
/// host machinery: the second scanned-excerpt-source plugin — the markdown TODO
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

    /// OA.4b: this view folds by blocks, so `<Tab>` / `<S-Tab>` come from the
    /// shared `foldable-view-mode`. Nothing special to do on a block, so it
    /// names the generic body.
    fn fold_toggle_action(&self) -> Option<&'static str> {
        Some(lattice_mode::FOLD_TOGGLE_DEFAULT_ACTION)
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
                        state.read().ok().map(|s| match s.options.roots.first() {
                            // AF.2: only a root the USER named is replayed.
                            // Roots that came from a source are deliberately
                            // NOT — re-asking picks up a `:set` of the
                            // source's own option, so `gr` after editing
                            // your agenda files shows the new set. What the
                            // replay protects is the explicit argument, and
                            // that is the case that arrives as one root.
                            Some(root) if s.options.roots.len() == 1 => {
                                Args::String(root.display().to_string())
                            }
                            _ => Args::None,
                        })
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// A unique scratch directory. Timestamp alone collides under parallel
    /// `cargo test`, so a counter rides with it.
    fn scratch(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("lattice-agenda-walk-{tag}-{nanos}-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// OA.0d: a configured directory means the files IN it. Emacs expands a
    /// directory entry in `org-agenda-files` one level, and an org directory
    /// with `roam/` or `archive/` beneath it should not drag those in.
    #[test]
    fn a_configured_directory_does_not_pull_in_its_subtree() {
        let dir = scratch("subtree");
        std::fs::write(dir.join("a.org"), "* TODO a\n").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("b.org"), "* TODO b\n").unwrap();

        let exts = vec!["org".to_string()];
        let found = collect_candidates(std::slice::from_ref(&dir), &exts, usize::MAX);
        let names: Vec<String> = found
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert_eq!(names, vec!["a.org"], "found: {found:?}");

        // The escape hatch: naming the subdirectory opts it in, which is the
        // whole reason this needs no recursion flag.
        let both = collect_candidates(&[dir.clone(), dir.join("sub")], &exts, usize::MAX);
        assert_eq!(both.len(), 2, "found: {both:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The one thing the recursive walk did right, kept: `ignore`'s filtering
    /// still applies at the single level, so dotfiles stay out.
    #[test]
    fn the_single_level_walk_still_skips_hidden_files() {
        let dir = scratch("hidden");
        std::fs::write(dir.join("a.org"), "* TODO a\n").unwrap();
        std::fs::write(dir.join(".hidden.org"), "* TODO h\n").unwrap();

        let found =
            collect_candidates(std::slice::from_ref(&dir), &["org".to_string()], usize::MAX);
        let names: Vec<String> = found
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert_eq!(names, vec!["a.org"], "found: {found:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file named directly is still taken, whatever its depth — the roots
    /// list holds files as well as directories, and only the DIRECTORY
    /// expansion changed.
    #[test]
    fn a_file_named_directly_is_still_taken() {
        let dir = scratch("file");
        std::fs::create_dir_all(dir.join("deep")).unwrap();
        let deep = dir.join("deep").join("c.org");
        std::fs::write(&deep, "* TODO c\n").unwrap();

        let found = collect_candidates(
            std::slice::from_ref(&deep),
            &["org".to_string()],
            usize::MAX,
        );
        assert_eq!(found, vec![deep]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// OA.5: the two translations `composed_row_spans` owns, on the layout
    /// that makes them differ — two files interleaved by the sort, so a row's
    /// composed index is NOT its index among its own file's rows.
    #[test]
    fn row_spans_land_on_the_composed_row_with_line_relative_offsets() {
        let mut a = entry(7, "day-1", "Day 1", 1);
        a.spans = vec![lattice_mode::scanned_excerpt_source::RowSpan {
            start: 2,
            end: 6,
            slot: "keyword".to_string(),
        }];
        let mut b = entry(3, "day-2", "Day 2", 2);
        b.spans = vec![lattice_mode::scanned_excerpt_source::RowSpan {
            start: 0,
            end: 4,
            slot: "comment".to_string(),
        }];
        // Row `a` is line 7 of file 0; row `b` is line 3 of file 1. Sorted,
        // `a` is composed row 0 and `b` composed row 1 — neither matching the
        // source line either carries.
        let rows = vec![
            SortedRow { file: 0, entry: a },
            SortedRow { file: 1, entry: b },
        ];
        let src_a = BufferId::next();
        let src_b = BufferId::next();
        let excerpts = vec![Excerpt::new(src_a, 7, 7), Excerpt::new(src_b, 3, 3)];

        let out = composed_row_spans(&rows, &excerpts, None);

        assert_eq!(out.len(), 2, "one entry per composed row");
        assert_eq!(out[0].len(), 1);
        assert_eq!(
            (out[0][0].start, out[0][0].end),
            (2, 6),
            "offsets stay relative to the row's own line, not rebased"
        );
        assert_eq!(out[1].len(), 1);
        assert_eq!((out[1][0].start, out[1][0].end), (0, 4));
        assert_ne!(
            out[0][0].style, out[1][0].style,
            "each slot resolves to its own style"
        );
    }

    /// A multi-line row must not smear its spans over the rows below it, and
    /// the row after it must still land at the right composed index.
    #[test]
    fn a_multi_line_row_does_not_shift_the_row_after_it() {
        let mut a = entry(0, "g", "G", 1);
        a.spans = vec![lattice_mode::scanned_excerpt_source::RowSpan {
            start: 0,
            end: 3,
            slot: "keyword".to_string(),
        }];
        let mut b = entry(9, "g", "", 2);
        b.spans = vec![lattice_mode::scanned_excerpt_source::RowSpan {
            start: 1,
            end: 2,
            slot: "comment".to_string(),
        }];
        let rows = vec![
            SortedRow { file: 0, entry: a },
            SortedRow { file: 0, entry: b },
        ];
        let src = BufferId::next();
        // The first excerpt spans TWO lines, so the second row is composed
        // row 2 rather than row 1.
        let excerpts = vec![Excerpt::new(src, 0, 1), Excerpt::new(src, 9, 9)];

        let out = composed_row_spans(&rows, &excerpts, None);

        assert_eq!(out.len(), 3, "two lines plus one: {out:?}");
        assert_eq!(out[0].len(), 1, "the first row's span is on its head line");
        assert!(out[1].is_empty(), "its second line carries nothing");
        assert_eq!(out[2].len(), 1, "the next row is at composed row 2");
    }

    fn entry(line: u32, group: &str, label: &str, sort_key: i64) -> ScannedExcerpt {
        ScannedExcerpt {
            line,
            end_line: line,
            group: group.to_string(),
            label: label.to_string(),
            sort_key,
            spans: Vec::new(),
        }
    }

    fn file(path: &str, entries: Vec<ScannedExcerpt>) -> FileRows {
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
        /// OA.11a: the scan args `begin` was handed, so a test can assert the
        /// view's arguments reached the producer rather than only that the
        /// scan ran.
        begin_args: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl FakeSource {
        fn new(id: u64, exts: &[&str]) -> Self {
            Self {
                id,
                exts: exts.iter().map(|e| e.to_string()).collect(),
                begins: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                offered: Arc::new(std::sync::Mutex::new(Vec::new())),
                begin_args: Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }
    }

    impl lattice_mode::ScannedExcerptSource for FakeSource {
        fn source_id(&self) -> u64 {
            self.id
        }
        fn extensions(&self) -> &[String] {
            &self.exts
        }
        fn begin(&self, args: &[String]) -> lattice_mode::AgendaBeginFuture<'_> {
            self.begins
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Ok(mut a) = self.begin_args.lock() {
                *a = args.to_vec();
            }
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
                Ok(lattice_mode::ScanResult::rows(
                    text.lines()
                        .enumerate()
                        .filter_map(|(i, line)| {
                            let rest = line.strip_prefix("* TODO ")?;
                            let key: i64 = rest.trim().parse().ok()?;
                            Some(ScannedExcerpt {
                                line: i as u32,
                                end_line: i as u32,
                                group: format!("day-{key}"),
                                label: format!("Day {key}"),
                                sort_key: key,
                                spans: Vec::new(),
                            })
                        })
                        .collect(),
                ))
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

    /// OA.11a: a trigger's arguments split into the host's slot and the
    /// guest's, positionally.
    ///
    /// The cases that matter are the two that predate the slice — a bare open
    /// and a root — because they must be untouched, and the two that motivate
    /// it: a command key with no root, and a root and a command key together.
    #[test]
    fn view_args_split_into_a_root_and_the_guests_own() {
        use lattice_grammar::args::ArgValue;

        // Unchanged by this slice: every trigger that existed before it.
        assert_eq!(split_view_args(&Args::None), (String::new(), Vec::new()));
        assert_eq!(
            split_view_args(&Args::String("  ~/notes  ".to_string())),
            ("~/notes".to_string(), Vec::new()),
            "trimmed, and still a root"
        );

        // A command key with no root — the dispatcher's ordinary case. Position
        // 0 is empty rather than missing, which is what keeps the list
        // positional instead of making the first element ambiguous.
        assert_eq!(
            split_view_args(&Args::List(vec![
                ArgValue::String(String::new()),
                ArgValue::String("waiting".to_string()),
            ])),
            (String::new(), vec!["waiting".to_string()]),
            "no root override, one scan arg"
        );

        // Both at once: neither consumes the other. This is the case a
        // single-slot design cannot express at all.
        assert_eq!(
            split_view_args(&Args::List(vec![
                ArgValue::String("~/notes".to_string()),
                ArgValue::String("waiting".to_string()),
            ])),
            ("~/notes".to_string(), vec!["waiting".to_string()])
        );
    }

    /// OA.11a: scan args are sticky across a re-open, exactly as roots are.
    ///
    /// `gr` re-enters through the opener, so an agenda opened for one command
    /// must not quietly revert to the default one on refresh. Replaced only
    /// when a new open supplies its own — which is how the dispatcher moves
    /// you between commands.
    #[test]
    fn a_re_open_without_args_keeps_the_command_it_was_opened_for() {
        use lattice_grammar::args::ArgValue;

        // What the opener does to a stored set of options, in the same order.
        let apply = |options: &mut AgendaOptions, args: &Args| {
            let (root, scan_args) = split_view_args(args);
            if !root.is_empty() {
                options.roots = vec![PathBuf::from(shellexpand_tilde(&root))];
            }
            if !scan_args.is_empty() {
                options.scan_args = scan_args;
            }
        };

        let mut options = AgendaOptions::default();
        apply(
            &mut options,
            &Args::List(vec![
                ArgValue::String(String::new()),
                ArgValue::String("waiting".to_string()),
            ]),
        );
        assert_eq!(options.scan_args, vec!["waiting".to_string()]);

        // A refresh carries no arguments and must not lose the command.
        apply(&mut options, &Args::None);
        assert_eq!(
            options.scan_args,
            vec!["waiting".to_string()],
            "`gr` keeps the agenda you chose"
        );

        // Choosing another command replaces it.
        apply(
            &mut options,
            &Args::List(vec![
                ArgValue::String(String::new()),
                ArgValue::String("refile".to_string()),
            ]),
        );
        assert_eq!(options.scan_args, vec!["refile".to_string()]);
    }

    /// OA.14b: a file with NO agenda rows still contributes its clocked time.
    ///
    /// The property the whole seam exists for, and the one an implementation
    /// that hung clock data off rows cannot have: a corpus can hold a week of
    /// logged time and not a single agenda row. Asserted through
    /// `spawn_agenda_scan` so it covers the driver's own filter — an earlier
    /// draft collected clock only inside the `!entries.is_empty()` branch,
    /// which passes every row test and loses exactly this file.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_file_with_no_rows_still_reports_its_clocked_time() {
        let dir = tempdir();
        // No `* TODO` line anywhere: the fake source yields zero rows.
        write(&dir, "notes.org", "* Notes\nsome prose\n");

        let (registry, view) = view_handle();
        let mut registry_builder = lattice_mode::ServiceRegistry::new();
        let svc = InMemoryAgendaService::handle();
        registry_builder.register::<AgendaServiceHandle>(svc.clone());
        let services = Arc::new(registry_builder);
        svc.set_state(
            view,
            AgendaState {
                options: AgendaOptions::default(),
                clock: Vec::new(),
            },
        );

        spawn_agenda_scan(
            view,
            AgendaOptions {
                roots: vec![dir.clone()],
                max_files: None,
                ..Default::default()
            },
            vec![Arc::new(ClockOnlySource {
                exts: vec!["org".to_string()],
            })],
            registry.clone(),
            None,
            Some(services),
        );
        settle_agenda(&registry, view).await;

        let state = svc.state(view).expect("the view has state");
        let clock = state.read().expect("readable").clock.clone();
        assert_eq!(
            clock
                .iter()
                .map(|c| (c.outline.clone(), c.minutes))
                .collect::<Vec<_>>(),
            vec![(vec!["Notes".to_string()], 90)],
            "the clocked time survives a file that produced no rows"
        );
    }

    /// A source with time but no rows — the shape the test above needs and the
    /// one a row-attached design could not express at all.
    #[derive(Debug)]
    struct ClockOnlySource {
        exts: Vec<String>,
    }

    impl lattice_mode::ScannedExcerptSource for ClockOnlySource {
        fn source_id(&self) -> u64 {
            42
        }
        fn extensions(&self) -> &[String] {
            &self.exts
        }
        fn begin(&self, _args: &[String]) -> lattice_mode::AgendaBeginFuture<'_> {
            Box::pin(async { Ok(()) })
        }
        fn scan(&self, _p: PathBuf, _t: String) -> lattice_mode::AgendaFuture<'_> {
            Box::pin(async {
                Ok(lattice_mode::ScanResult {
                    entries: Vec::new(),
                    clock: vec![lattice_mode::ClockSpan {
                        line: 0,
                        outline: vec!["Notes".to_string()],
                        day: 20_000,
                        minutes: 90,
                    }],
                })
            })
        }
    }

    /// OA.11a: the view's scan args reach every source's `begin`.
    ///
    /// The walk is untouched by them — this asserts the same file set is
    /// offered either way — because that is the whole point of the two-slot
    /// split. `roots` is the host's parameter and drives the walk; `scan_args`
    /// are the guest's and drive nothing the host does. A single-slot design
    /// fails here by turning the command key into a root and offering no files
    /// at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_views_scan_args_reach_every_source() {
        let dir = tempdir();
        write(&dir, "a.org", "* TODO 1\n");

        let (registry, view) = view_handle();
        let source = Arc::new(FakeSource::new(1, &["org"]));
        let begin_args = source.begin_args.clone();
        let offered = source.offered.clone();

        spawn_agenda_scan(
            view,
            AgendaOptions {
                roots: vec![dir.clone()],
                max_files: None,
                scan_args: vec!["waiting".to_string()],
            },
            vec![source],
            registry.clone(),
            None,
            None,
        );
        settle_agenda(&registry, view).await;

        assert_eq!(
            *begin_args.lock().unwrap(),
            vec!["waiting".to_string()],
            "the args the view carries are handed to the producer verbatim"
        );
        assert_eq!(
            offered.lock().unwrap().len(),
            1,
            "and the walk is unchanged by them — scan args parameterise the \
             GUEST, not the file set"
        );
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
                roots: vec![dir.clone()],
                max_files: None,
                ..Default::default()
            },
            vec![source],
            registry.clone(),
            None,
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
                roots: vec![dir.clone()],
                max_files: None,
                ..Default::default()
            },
            vec![Arc::new(FakeSource::new(1, &["org"]))],
            registry.clone(),
            None,
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
                roots: vec![dir.clone()],
                max_files: None,
                ..Default::default()
            },
            vec![Arc::new(FakeSource::new(1, &["org"]))],
            registry.clone(),
            None,
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
                roots: vec![dir.clone()],
                max_files: None,
                ..Default::default()
            },
            vec![source],
            registry.clone(),
            None,
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

    impl lattice_mode::ScannedExcerptSource for DeadSource {
        fn source_id(&self) -> u64 {
            99
        }
        fn extensions(&self) -> &[String] {
            &self.exts
        }
        fn begin(&self, _args: &[String]) -> lattice_mode::AgendaBeginFuture<'_> {
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
                roots: vec![dir.clone()],
                max_files: None,
                ..Default::default()
            },
            // The healthy source still contributes: one bad producer must not
            // cost the user the other's rows.
            vec![Arc::new(FakeSource::new(1, &["org"])), dead],
            registry.clone(),
            None,
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
                roots: vec![dir.clone()],
                max_files: None,
                ..Default::default()
            },
            vec![Arc::new(FakeSource::new(1, &["org"]))],
            registry.clone(),
            None,
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
                    roots: vec![PathBuf::from("/p")],
                    max_files: Some(10),
                    ..Default::default()
                },
                clock: Vec::new(),
            },
        );
        let got = svc.state(view).unwrap();
        assert_eq!(got.read().unwrap().options.roots, vec![PathBuf::from("/p")]);
        svc.clear(view);
        assert!(svc.state(view).is_none());
    }
}

#[cfg(test)]
mod af2_roots {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    /// The sibling module's helper, restated rather than imported: it is
    /// `#[cfg(test)]` in a private module, and a counter is what keeps parallel
    /// `cargo test` runs from colliding ([[tempdir-helpers-need-a-counter]]).
    fn tempdir() -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("lattice-af2-{nanos}-{n}"));
        std::fs::create_dir_all(&dir).expect("tempdir");
        dir
    }

    fn touch(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, "* TODO x\n").unwrap();
        p
    }

    fn names(paths: &[PathBuf]) -> Vec<String> {
        let mut v: Vec<String> = paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    /// The two shapes one list has to carry: a directory that gets walked, and
    /// a file taken as given. Both are ordinary org usage — `org-agenda-files`
    /// holding `org-directory` plus a single `anniversaries.org` is the case
    /// this exists for.
    #[test]
    fn a_root_may_be_a_directory_or_a_file() {
        let base = tempdir();
        let notes = base.join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        touch(&notes, "a.org");
        touch(&notes, "b.org");
        let loose = touch(&base, "anniversaries.org");
        // A file the walk would NOT have offered: nothing claims `.txt`.
        // Naming it is the claim, which is why the extension test is skipped
        // for a root that is a file.
        let odd = base.join("birthdays.txt");
        std::fs::write(&odd, "* TODO y\n").unwrap();

        let exts = vec!["org".to_string()];
        let got = collect_candidates(&[notes.clone(), loose, odd], &exts, usize::MAX);
        assert_eq!(
            names(&got),
            vec!["a.org", "anniversaries.org", "b.org", "birthdays.txt"]
        );
    }

    /// A file inside a configured directory is an ordinary way to write
    /// "everything here, and that one too" — and must not scan twice, or its
    /// rows appear twice in the view.
    #[test]
    fn a_file_inside_a_configured_directory_is_not_scanned_twice() {
        let base = tempdir();
        let notes = base.join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        let a = touch(&notes, "a.org");

        let exts = vec!["org".to_string()];
        let got = collect_candidates(&[notes, a], &exts, usize::MAX);
        assert_eq!(names(&got), vec!["a.org"], "deduplicated across roots");
    }

    /// One bad entry in a config list is the same failure class as one bad
    /// file: skip it and scan the rest. Refusing the whole agenda because a
    /// path was renamed would be the worse answer by far.
    #[test]
    fn a_configured_path_that_is_gone_does_not_fail_the_scan() {
        let base = tempdir();
        let notes = base.join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        touch(&notes, "a.org");

        let exts = vec!["org".to_string()];
        let got = collect_candidates(&[base.join("does-not-exist"), notes], &exts, usize::MAX);
        assert_eq!(names(&got), vec!["a.org"]);
    }

    /// The cap bounds the UNION. A cap that reset per root would not be a
    /// bound, which is the whole reason it exists — the walk is unattended.
    #[test]
    fn the_file_cap_applies_across_roots_not_per_root() {
        let base = tempdir();
        let one = base.join("one");
        let two = base.join("two");
        std::fs::create_dir_all(&one).unwrap();
        std::fs::create_dir_all(&two).unwrap();
        touch(&one, "a.org");
        touch(&one, "b.org");
        touch(&two, "c.org");
        touch(&two, "d.org");

        let exts = vec!["org".to_string()];
        let got = collect_candidates(&[one, two], &exts, 3);
        assert_eq!(got.len(), 3, "three across both roots, not three from each");
    }

    /// With nothing configured the options are EMPTY, not the project root —
    /// the fallback moved into the scan, where every source has been asked.
    /// A default that still baked in the project root would mean a source's
    /// roots could never be reached.
    #[test]
    fn the_default_names_no_root_so_the_scan_can_ask() {
        assert!(
            AgendaOptions::default().roots.is_empty(),
            "the fallback belongs to the scan, which is the only place that \
             has heard from the sources"
        );
        // …and the view's scope still resolves to something usable.
        assert!(!AgendaOptions::default().scope_dir().as_os_str().is_empty());
    }
}
