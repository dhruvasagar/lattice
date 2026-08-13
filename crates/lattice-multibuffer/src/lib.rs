//! # `lattice-multibuffer`
//!
//! M.2.b.1 (2026-05-31): dedicated crate for every multibuffer
//! concern. Lives outside `lattice-runtime` so that:
//!
//! - The runtime crate stays focused on the actor + handle +
//!   Document-trait substrate; multibuffer is one specific kind
//!   of document built on top of that substrate, not part of it.
//! - Plugins (post-v1) can depend on `lattice-multibuffer`
//!   directly without pulling in the full actor machinery.
//! - The crate boundary makes the design self-documenting —
//!   every multibuffer concern is in one tree; nothing else
//!   knows multibuffer exists except `lattice-host`'s tiny
//!   boot-wiring registration.
//!
//! See `docs/dev/architecture/multibuffer-views.md` §3.6 for the
//! crate-layout decision.
//!
//! ## What this crate ships (M.2.b.1)
//!
//! * **Data model**: `Excerpt`, `ExcerptId`, `ExcerptHeader`,
//!   `ExcerptHeaderStyle`, `RowEntry`, `RowTranslation`.
//! * **Handle**: `MultibufferDocumentHandle` — read-only impl of
//!   `lattice_runtime::Document` composing N source handles into
//!   one view. M.3 lifts the read-only restriction.
//! * **Header provider**: `MultibufferExcerptHeaderProvider` (impl
//!   `lattice_cells::VirtualRowProvider`) emitting one virtual
//!   row per excerpt header.
//!
//! ## What lands later
//!
//! * **M.2.b.2** — `MultibufferMode` as the major mode for
//!   `BufferKind::Multibuffer`. Activation owns the header
//!   provider registration + per-buffer typed context Guard.
//! * **M.2.b.3** — `]e` / `[e` / `]E` / `[E` motions registered
//!   through the grammar; bound in `MultibufferMode` keymap.
//! * **M.3** — edit propagation (writes flow back to source
//!   handles via the row translation).
//! * **M.4** — live updates from sources (auto-recompose on
//!   `EventKind::DocumentChanged`; anchor sliding; source-close
//!   auto-remove).
//! * **M.5–M.8** — expand-context, provider trait + first
//!   consumer, fold providers.

// PV.1 (2026-08-12): crate-level events, moved out of the
// feature-gated `providers::search` so the off-keystroke wake exists in
// every build and every provider crate can publish it.
pub mod events;
pub mod install;
pub mod mode;
pub mod motions;
pub mod providers;
pub mod registry;
pub mod view;

pub use crate::events::MultibufferExcerptsReady;
pub use crate::install::install;
pub use crate::mode::{
    MultibufferMode, register_multibuffer_ex_commands, register_multibuffer_modes,
};
pub use crate::motions::{MultibufferMotionIds, register_multibuffer_motions};
pub use crate::registry::{
    InMemoryMultibufferRegistry, MultibufferRegistry, MultibufferRegistryHandle,
};
pub use crate::view::create_multibuffer_view;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use arc_swap::ArcSwap;
use lattice_cells::cell::Cell;
use lattice_cells::headerline::{Headerline, HeaderlineProvider, HeaderlineRow};
use lattice_cells::virtual_rows::{
    AnchorPosition, ProviderId, VirtualRow, VirtualRowKind, VirtualRowProvider,
};
use lattice_core::buffer::AppliedEdit;
use lattice_core::{Buffer, BufferId};
use lattice_grammar::{
    CancellationToken, CommandInvocation, CommandKind, CommandRegistryHandle, Effect,
    execute_with_env,
};
use lattice_protocol::edit::Edit;
use lattice_protocol::ids::DocumentId;
use lattice_protocol::position::Position;
use lattice_protocol::selection::SelectionSet;
use lattice_runtime::{
    Document, DocumentSnapshot, Pending, PublishedSnapshot, RuntimeError, SnapshotCache,
};
// K.4.7 (2026-06-07): per-excerpt syntax highlighting.
use lattice_syntax::{Lang, LangRegistry, Syntax, SyntaxHandle, SyntaxSnapshot};
// T.7 (2026-06-18): mode-owned theme elements for the excerpt header.
use lattice_theme::{
    Color, ColorRef, ElementId, ElementName, ElementOwner, StyleSpec, ThemeRegistryHandle,
};

// ─────────────────────────────────────────────────────────────────
// Excerpt + identity + header
// ─────────────────────────────────────────────────────────────────

/// Unique identity for an excerpt within a multibuffer. Stable
/// for the excerpt's lifetime; survives reorders / source-edit
/// rebuilds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExcerptId(pub u64);

impl ExcerptId {
    pub fn next() -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(1);
        Self(SEQ.fetch_add(1, Ordering::Relaxed))
    }
}

/// Header presentation for an excerpt — title + style + the
/// mode-owned semantic data the rich header renderer reads
/// (MH.A2, see multibuffer-views.md §3.8).
#[derive(Debug, Clone, Default)]
pub struct ExcerptHeader {
    /// Human-readable label. Conventionally
    /// `"<path> : <start_line+1>-<end_line+1>"` for a regular
    /// file excerpt (1-indexed for display). Empty string = no
    /// header rendered. Fallback basename when `path` is `None`.
    pub title: String,
    pub style: ExcerptHeaderStyle,
    /// MH.A2: the source file path. Drives the leading file-type
    /// icon and the basename-bright / dir-dim split in
    /// [`header_cells`]. `None` ⇒ fall back to `title`. Set by the
    /// producing mode (e.g. the search provider) at excerpt
    /// creation, NOT baked into cells here — the glyph + colours
    /// are resolved live in `collect()` so `ui.nerd_fonts` /
    /// `:colorscheme` toggles re-render correctly.
    pub path: Option<std::path::PathBuf>,
    /// MH.A2: hit count for the `· N matches` badge. Mode-consumed
    /// datum set at production (the search mode counts hits per
    /// source). `None` ⇒ no badge rendered.
    pub match_count: Option<u32>,
}

impl ExcerptHeader {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            style: ExcerptHeaderStyle::default(),
            path: None,
            match_count: None,
        }
    }
}

/// Style discriminator for excerpt headers. M.2 ships with a
/// single `Default` variant; future variants distinguish header
/// presentation (severity-prefixed for diagnostics provider,
/// hunk-decorated for project-diff provider).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExcerptHeaderStyle {
    #[default]
    Default,
}

/// One excerpt of a source document, identified by its source
/// `BufferId` and an inclusive line range
/// `[start_line, end_line]`.
///
/// M.1 keeps the range as integer line numbers; M.4 swaps to
/// `Anchor`-based positions that slide on source edits.
#[derive(Debug, Clone)]
pub struct Excerpt {
    pub id: ExcerptId,
    pub source: BufferId,
    pub start_line: u32,
    pub end_line: u32,
    pub header: ExcerptHeader,
}

impl Excerpt {
    pub fn new(source: BufferId, start_line: u32, end_line: u32) -> Self {
        Self {
            id: ExcerptId::next(),
            source,
            start_line,
            end_line,
            header: ExcerptHeader::default(),
        }
    }

    pub fn with_header(mut self, header: ExcerptHeader) -> Self {
        self.header = header;
        self
    }

    /// Number of source rows this excerpt covers. Always `>= 1`
    /// for a well-formed excerpt.
    pub fn line_count(&self) -> u32 {
        self.end_line.saturating_sub(self.start_line) + 1
    }
}

// ─────────────────────────────────────────────────────────────────
// Row translation
// ─────────────────────────────────────────────────────────────────

/// One row in the composed multibuffer view, mapped back to its
/// source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowEntry {
    Excerpt {
        excerpt_id: ExcerptId,
        source_row: u32,
    },
}

/// Composed-row → source-row mapping. One entry per composed
/// row, in display order. Rebuilt on every recompose.
#[derive(Debug, Clone, Default)]
pub struct RowTranslation {
    pub entries: Vec<RowEntry>,
}

impl RowTranslation {
    pub fn build(excerpts: &[Excerpt]) -> Self {
        let mut this = Self::default();
        this.append(excerpts);
        this
    }

    /// MH.B1 (2026-06-19): extend the translation in place with a
    /// batch of newly-appended excerpts. Because `build` is pure
    /// concatenation (one `RowEntry::Excerpt` per source row, per
    /// excerpt, in order — no cross-excerpt state), appending a
    /// batch's entries to an existing translation yields exactly
    /// the same `Vec<RowEntry>` as rebuilding from the full excerpt
    /// list. This is the row-translation half of the O(batch)
    /// incremental `append_excerpts`; the equivalence is pinned by
    /// `incremental_append_matches_full_build`.
    pub fn append(&mut self, excerpts: &[Excerpt]) {
        for excerpt in excerpts {
            for row in excerpt.start_line..=excerpt.end_line {
                self.entries.push(RowEntry::Excerpt {
                    excerpt_id: excerpt.id,
                    source_row: row,
                });
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────
// MultibufferDocumentHandle
// ─────────────────────────────────────────────────────────────────

struct MultibufferInner {
    id: DocumentId,
    buffer_id: BufferId,
    // M.2.b.2 (2026-06-01): sources + excerpts move behind a
    // Mutex so providers can stream updates asynchronously via
    // `append_excerpts` / `replace_excerpts` / `add_source` /
    // `remove_source`. Hot reads (`snapshot`, `row_translation`,
    // `excerpts`) go through the lock-free `PublishedSnapshot`
    // cell and the `ArcSwap<RowTranslation>` — the Mutex is
    // only acquired on mutation + on the recompose seam.
    state: std::sync::Mutex<MultibufferState>,
    /// M.11 (2026-06-02): the composed rope, owned LOCALLY.
    /// Edits apply here synchronously (byte-identical to
    /// `RopeDocumentHandle`'s apply_edit) before being forwarded
    /// to the relevant source actor async. The multibuffer is no
    /// longer a proxy — it IS a buffer.
    ///
    /// Wrapped in `Mutex` because `apply_edit` runs on the
    /// caller's thread (the host's block_on bridge) while the
    /// source forwarder task and excerpt-mutation paths
    /// (append_excerpts / replace_excerpts) read it from other
    /// contexts. Lock-free reads go through `snapshot_cell`.
    composed_doc: std::sync::Mutex<lattice_core::Document>,
    /// M.11 (2026-06-02): mpsc sender into the source-forwarder
    /// task. Each `apply_edit` queues a (composed_edit,
    /// row_translation_snapshot) pair; the forwarder task
    /// translates to source coords and ships to the source
    /// actor. Fire-and-forget — caller doesn't wait.
    source_forward_tx: tokio::sync::mpsc::UnboundedSender<SourceForwardMsg>,
    snapshot_cell: Arc<PublishedSnapshot>,
    row_translation: ArcSwap<RowTranslation>,
    // M.4 (2026-06-01): view-level headerline rendered above the
    // first excerpt. Async providers update this to surface
    // progress + completion status (see
    // `multibuffer-views.md` §3.7 "headerline status convention").
    // Lock-free read via `ArcSwap`; writes go through
    // `set_headerline` which also publishes
    // `MultibufferHeaderlineChanged`.
    headerline: ArcSwap<HeaderlineStatus>,
    // M.6.5 (2026-06-08): monotonic version for the view-status headerline.
    // Bumped in `set_headerline` so `MultibufferStatusProvider::version()`
    // advances and the cells worker rebuilds the sticky row.
    headerline_version: AtomicU64,
    // M.4 (2026-06-01): event-bus subscription bookkeeping for
    // the auto-recompose forwarder. `SubscriptionId`s registered
    // by `attach_event_subscriptions` are unsubscribed on Drop.
    subscriptions: std::sync::Mutex<SubscriptionBookkeeping>,
    // K.4.11 (2026-06-02): CommandRegistry the multibuffer runs
    // grammar against in `dispatch_with_cancel`. Passed at
    // construction so the multibuffer is a self-sufficient
    // Document — same shape `spawn_document(id, doc, registry)`
    // takes for regular Document handles. Replaces the
    // host-side kind-branch in `Editor::dispatch_blocking` (the
    // multibuffer's own `Document::dispatch_with_cancel` impl
    // now does the work uniformly).
    //
    // B3b: the `ArcSwap` handle (was `Arc<CommandRegistry>`) so a plugin
    // grammar contribution registered at runtime is live for this view's
    // next dispatch; `dispatch_with_cancel` `.load_full()`s an owned
    // snapshot for each keystroke.
    registry: CommandRegistryHandle,
    // K.4.7 (2026-06-07): language registry for per-source
    // SyntaxHandle creation. Set once after construction via
    // `set_lang_registry`; `None` until the host wires it.
    lang_registry: std::sync::OnceLock<Arc<LangRegistry>>,
    // K.4.7 (2026-06-08): monotonic generation counter. Incremented
    // every time `source_syntax` gains a new handle (`add_source` /
    // `set_lang_registry`). Folded into `MatrixVersion::syntax` in
    // `publish_render_state` so the cells worker invalidates its cache
    // when per-source handles are first populated. XOR of individual
    // handle text_versions is unreliable: N handles all at version=1
    // XOR to 0 for even N, colliding with the initial-zero and
    // producing a false cache hit that freezes highlighting.
    excerpt_syntax_gen: std::sync::atomic::AtomicU64,
    // K.4.7 (2026-06-08): monotonic publish counter. Stamped as the
    // `text_version` on every DocumentSnapshot emitted by
    // `append_excerpts` / `replace_excerpts`. Document::from_text()
    // always returns text_version=0, so without this counter the
    // MatrixVersion.text axis is permanently 0 and the cells worker
    // always returns CacheHit — the "empty until keypress" bug.
    publish_seq: std::sync::atomic::AtomicU64,
}

#[derive(Default)]
struct SubscriptionBookkeeping {
    /// Subscription ids returned by `EventBus::subscribe`; cleared
    /// on Inner Drop via `unsubscribe`.
    ids: Vec<lattice_runtime::SubscriptionId>,
    /// Cheap-clone Arc for the unsubscribe path. `None` until
    /// `attach_event_subscriptions` runs.
    bus: Option<Arc<lattice_runtime::EventBus>>,
}

impl Drop for MultibufferInner {
    fn drop(&mut self) {
        // Unsubscribe + drop the bus reference so the forwarder
        // task (which holds a Weak<MultibufferInner>) sees the
        // upgrade fail and exits cleanly.
        if let Ok(mut book) = self.subscriptions.lock()
            && let Some(bus) = book.bus.take()
        {
            for id in book.ids.drain(..) {
                let _ = bus.unsubscribe(id);
            }
        }
    }
}

/// M.11 (2026-06-02): one outbound edit waiting to be forwarded
/// to a source actor. Carries pre-resolved source coords so the
/// forwarder task doesn't need to re-walk the row translation
/// (which may have shifted by the time the task runs).
#[derive(Debug)]
enum SourceForwardMsg {
    /// Propagate a composed-coordinate edit to its source actor.
    Edit {
        source_handle: Arc<dyn Document>,
        source_edit: Edit,
    },
    /// Save-flush barrier (2026-06-10). Sent by
    /// [`MultibufferDocumentHandle::save`] through the SAME FIFO
    /// channel as edits, so by the time the forwarder pops it every
    /// prior edit has been applied to (and `await`ed on) its source
    /// actor. The forwarder then signals `done`, telling `save()` the
    /// sources are current — without this, `:w` would race the async
    /// forwarder and persist stale sources (drop the last keystrokes).
    Flush {
        done: tokio::sync::oneshot::Sender<()>,
    },
}

struct MultibufferState {
    sources: HashMap<BufferId, Arc<dyn Document>>,
    /// SS.2 (2026-08-11): the on-disk identity each source had when it
    /// entered the view.
    ///
    /// A multibuffer's sources are SNAPSHOTS — read once at
    /// view-creation and held for the view's lifetime — while
    /// `Document::save` writes every dirty one back. Without a baseline
    /// that silently overwrites whatever changed the file externally
    /// (a rebase, a formatter, another pane). SS.3 compares against
    /// this before writing.
    ///
    /// Pathless sources (synthetic documents) are absent and are never
    /// stale — there is no file to conflict with.
    /// See `docs/dev/architecture/multibuffer-stale-sources.md`.
    source_fingerprints: HashMap<BufferId, lattice_core::on_disk::OnDiskFingerprint>,
    excerpts: Vec<Excerpt>,
    /// K.4.7 (2026-06-07): per-source SyntaxHandle for excerpt
    /// highlighting. Populated by `add_source` when `lang_registry`
    /// is set. Sources with `Lang::Plain` are absent.
    source_syntax: HashMap<BufferId, Arc<SyntaxHandle>>,
    /// K.4.5 (2026-06-02): composed-coordinate selection set
    /// for the view. Multibuffers don't propagate selections to
    /// their source buffers (M.3 design — composed coordinates
    /// don't map cleanly back through edits / excerpts), but
    /// the view itself IS a buffer and carries its own
    /// selection state. Visual-mode highlight painting
    /// (`Editor::visual_selection_range` → renderer) reads
    /// these via `snapshot.selections`. Updated by
    /// `set_selections` (Document trait); rebuilt-but-preserved
    /// by every recompose path so excerpt mutations don't
    /// clobber the user's selection.
    selections: Arc<SelectionSet>,
}

// ─────────────────────────────────────────────────────────────────
// M.4 (2026-06-01): headerline status + typed events
// ─────────────────────────────────────────────────────────────────

/// View-level headerline status. Rendered above the first
/// excerpt (M.2.a `MultibufferExcerptHeaderProvider` extends to handle
/// the view header in a later renderer slice).
///
/// Async providers transition `Idle → InProgress → Complete` /
/// `Failed` as their scan progresses. See
/// `multibuffer-views.md` §3.7.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HeaderlineStatus {
    /// No status rendered. The view-header virtual row is empty.
    #[default]
    Idle,
    /// A scan / fetch / computation is running. `label` describes
    /// it; `count` is an optional running tally (hits found so
    /// far, files scanned, etc.). `emphasis` is an optional
    /// substring of `label` (e.g. the search query) that the
    /// renderer paints with the `multibuffer.status.query` accent
    /// role so it stands out; providers with nothing to emphasise
    /// pass `None`.
    InProgress {
        label: String,
        count: Option<usize>,
        emphasis: Option<String>,
    },
    /// The operation completed successfully. `summary` is the
    /// terminal label rendered to the user. `emphasis` is an
    /// optional substring of `summary` painted with the accent
    /// role (see [`HeaderlineStatus::InProgress`]).
    Complete {
        summary: String,
        emphasis: Option<String>,
    },
    /// The operation failed. `reason` is the terminal label
    /// rendered to the user.
    Failed { reason: String },
}

/// M.4 (2026-06-01): published whenever a view's headerline
/// status changes. Renderers + status-line consumers subscribe
/// via `EventBus::subscribe_typed::<MultibufferHeaderlineChanged>`.
#[derive(Debug, Clone)]
pub struct MultibufferHeaderlineChanged {
    pub view: BufferId,
    pub status: HeaderlineStatus,
}

lattice_protocol::register_event!(
    MultibufferHeaderlineChanged,
    "multibuffer.headerline-changed",
    "Multibuffer view's headerline status changed (Idle / InProgress / Complete / Failed).",
    "lattice-multibuffer",
);

/// M.4 (2026-06-01): published when one of a multibuffer's
/// source buffers closes. Providers subscribe to choose a
/// source-close policy: project-search drops the stale excerpts;
/// project-diff may keep them as historical reference.
/// Multibuffer itself prunes the source from its internal map.
#[derive(Debug, Clone)]
pub struct MultibufferSourceClosed {
    pub view: BufferId,
    pub source: BufferId,
}

lattice_protocol::register_event!(
    MultibufferSourceClosed,
    "multibuffer.source-closed",
    "One of a multibuffer view's source buffers closed; providers choose policy (drop excerpts, keep stale, etc.).",
    "lattice-multibuffer",
);

/// A multibuffer document handle. Composes N source
/// `Arc<dyn Document>`s into one read-only composed view; impls
/// [`Document`] so dispatch / motion / render code paths serve
/// it the same as a regular `RopeDocumentHandle`.
#[derive(Clone)]
pub struct MultibufferDocumentHandle {
    inner: Arc<MultibufferInner>,
}

/// SS.3: has `path`'s content changed since `baseline` was taken?
///
/// Cheap `(mtime, size)` pre-gate first; only a file that looks moved
/// is re-read and hashed. The content hash is authoritative, so a bare
/// `touch` — mtime bumped, bytes identical — is correctly NOT stale.
///
/// An unreadable file is treated as **not stale**: the subsequent save
/// will fail on its own and report a real I/O error, which is a better
/// message than "changed on disk" for a file that was deleted.
fn is_stale_on_disk(
    path: &std::path::Path,
    baseline: &lattice_core::on_disk::OnDiskFingerprint,
) -> bool {
    if baseline.stat_unchanged(path) {
        return false;
    }
    let Ok(disk_text) = std::fs::read_to_string(path) else {
        return false;
    };
    let current = lattice_core::on_disk::OnDiskFingerprint::from_path_and_text(path, &disk_text);
    !current.same_content(baseline)
}

/// SS.2: the on-disk baseline for one source, or `None` when it has no
/// path (a synthetic document — nothing on disk to conflict with).
///
/// Called at insertion, where the source's in-memory text IS what was
/// just read from disk, so the baseline is exact and costs one `stat`.
fn fingerprint_source(
    source: &Arc<dyn Document>,
) -> Option<lattice_core::on_disk::OnDiskFingerprint> {
    let path = source.path()?;
    let text = source.snapshot().buffer.as_string();
    Some(lattice_core::on_disk::OnDiskFingerprint::from_path_and_text(&path, &text))
}

impl MultibufferDocumentHandle {
    /// Construct a multibuffer composing `sources` + `excerpts`.
    ///
    /// M.2.b.2 (2026-06-01): empty `sources` + empty `excerpts`
    /// are valid — async providers (project-search, lsp-references,
    /// etc.) open an empty view immediately and stream content in
    /// via [`Self::append_excerpts`] / [`Self::add_source`] as
    /// their scan progresses. The previous `EmptyExcerpts` error
    /// was relaxed when the async-provider pattern landed; see
    /// `multibuffer-views.md` §3.7.
    ///
    /// Returns `UnknownSource` if any excerpt references a
    /// source BufferId not present in `sources`.
    pub fn new(
        sources: HashMap<BufferId, Arc<dyn Document>>,
        excerpts: Vec<Excerpt>,
        registry: CommandRegistryHandle,
    ) -> Result<Self, MultibufferError> {
        for ex in &excerpts {
            if !sources.contains_key(&ex.source) {
                return Err(MultibufferError::UnknownSource {
                    excerpt: ex.id,
                    source_buffer: ex.source,
                });
            }
        }

        let id = next_multibuffer_document_id();
        let buffer_id = BufferId::next();
        let row_translation = RowTranslation::build(&excerpts);
        // M.11 (2026-06-02): build the composed Document LOCALLY
        // from source content at construction time. From here on,
        // the multibuffer's composed_doc is authoritative — edits
        // land on it synchronously; sources are downstream
        // observers that get the same edit forwarded async.
        let composed_text = compose_text_from_sources(&sources, &excerpts);
        let composed_doc = lattice_core::Document::from_text(composed_text);
        let composed =
            snapshot_from_composed_doc(&composed_doc, id, Arc::new(SelectionSet::default()));
        let snapshot_cell = Arc::new(PublishedSnapshot::new(composed));

        // M.11 (2026-06-02): spawn the source-forwarder task on
        // the shared multi-thread runtime — the same runtime that
        // owns source actors (via `spawn_document` →
        // `shared_runtime().spawn(actor.run())`). This guarantees:
        // (1) the forwarder isn't tied to the caller's runtime
        // (which may be a current_thread editor actor about to
        // block in `block_on`); (2) cross-runtime mpsc + oneshot
        // semantics aren't needed (both forwarder + source actor
        // are on the same runtime); (3) the forwarder never gets
        // starved by the caller's runtime.
        let (source_forward_tx, mut source_forward_rx) =
            tokio::sync::mpsc::unbounded_channel::<SourceForwardMsg>();
        lattice_runtime::shared_runtime().spawn(async move {
            while let Some(msg) = source_forward_rx.recv().await {
                match msg {
                    // Discard the AppliedEdit — the multibuffer's
                    // local composed_doc is already authoritative.
                    // Best-effort propagation.
                    SourceForwardMsg::Edit {
                        source_handle,
                        source_edit,
                    } => {
                        let _ = source_handle.apply_edit(source_edit).await;
                    }
                    // FIFO: every prior Edit was applied + awaited
                    // above, so the sources are now current. Signal
                    // `save()` to proceed (best-effort — a dropped
                    // `done` just means save() falls through).
                    SourceForwardMsg::Flush { done } => {
                        let _ = done.send(());
                    }
                }
            }
        });

        Ok(Self {
            inner: Arc::new(MultibufferInner {
                id,
                buffer_id,
                state: std::sync::Mutex::new(MultibufferState {
                    source_fingerprints: sources
                        .iter()
                        .filter_map(|(id, src)| fingerprint_source(src).map(|fp| (*id, fp)))
                        .collect(),
                    sources,
                    excerpts,
                    source_syntax: HashMap::new(),
                    selections: Arc::new(SelectionSet::default()),
                }),
                composed_doc: std::sync::Mutex::new(composed_doc),
                source_forward_tx,
                snapshot_cell,
                row_translation: ArcSwap::from_pointee(row_translation),
                headerline: ArcSwap::from_pointee(HeaderlineStatus::Idle),
                headerline_version: AtomicU64::new(0),
                subscriptions: std::sync::Mutex::new(SubscriptionBookkeeping::default()),
                registry,
                lang_registry: std::sync::OnceLock::new(),
                excerpt_syntax_gen: std::sync::atomic::AtomicU64::new(0),
                publish_seq: std::sync::atomic::AtomicU64::new(0),
            }),
        })
    }

    /// Convenience constructor for the async-provider pattern:
    /// build an empty view with no sources and no excerpts. The
    /// provider streams content in via [`Self::append_excerpts`].
    /// Infallible.
    ///
    /// K.4.11 (2026-06-02): takes the same `CommandRegistryHandle`
    /// as the full [`Self::new`] constructor. The multibuffer is
    /// grammar-capable from creation — empty-view or not.
    pub fn empty(registry: CommandRegistryHandle) -> Self {
        Self::new(HashMap::new(), Vec::new(), registry)
            .expect("empty inputs are valid; UnknownSource impossible")
    }

    pub fn buffer_id(&self) -> BufferId {
        self.inner.buffer_id
    }

    /// M.2.b.2 (2026-06-01): the multibuffer's `DocumentId`, used
    /// by the cleanup subscriber to match an `Event::DocumentClosed`
    /// payload (which carries `DocumentId`, not `BufferId`) back
    /// to a registry entry keyed by `BufferId`.
    pub fn document_id(&self) -> DocumentId {
        self.inner.id
    }

    pub fn row_translation(&self) -> Arc<RowTranslation> {
        self.inner.row_translation.load_full()
    }

    /// Snapshot the current excerpt list. M.2.b.2 (2026-06-01):
    /// returns an owned `Vec` clone because excerpts now live
    /// behind a Mutex (async providers mutate); callers that
    /// need a borrow held across `await` points or across
    /// concurrent mutations get a deterministic copy instead.
    pub fn excerpts(&self) -> Vec<Excerpt> {
        self.lock_state().excerpts.clone()
    }

    /// Count of currently-registered excerpts. Cheap probe that
    /// avoids the `Vec` clone of [`Self::excerpts`].
    pub fn excerpt_count(&self) -> usize {
        self.lock_state().excerpts.len()
    }

    pub fn source_buffer_ids(&self) -> Vec<BufferId> {
        self.lock_state().sources.keys().copied().collect()
    }

    /// Generic multibuffer jump-to-source: resolve a source buffer
    /// id to its on-disk path by reading the source document's path
    /// directly. Unlike the per-provider `source_path` mapping
    /// (e.g. `ProjectSearchService::source_path`), this works for
    /// ANY multibuffer view without provider-specific state — every
    /// source document carries its path through the `Document` trait.
    /// Consumed by the generic `action:multibuffer-jump-to-source`
    /// handler registered by `MultibufferMode::on_activate`. Returns
    /// `None` when the source buffer id is unknown or has no path.
    pub fn source_path(&self, source_buffer_id: BufferId) -> Option<PathBuf> {
        self.lock_state().sources.get(&source_buffer_id)?.path()
    }

    /// M.10.2 (2026-06-03): translate a composed-coordinate
    /// cursor to its source-coordinate equivalent. Walks excerpts
    /// in display order to find the one containing
    /// `cursor.line`; returns `(source_buffer_id,
    /// source_position)` where `source_position.line` is the
    /// row in the originating source rope and
    /// `source_position.byte` is preserved verbatim (each
    /// composed row is a verbatim copy of its source line, so
    /// byte columns map 1:1).
    ///
    /// Returns `None` when the cursor is past the last
    /// excerpt's last composed row. Pure read; doesn't mutate
    /// any state.
    ///
    /// Consumed by mode handlers (search `<CR>`, project-diff
    /// `<CR>`, lsp-references `<CR>` once those land) — see
    /// `mode-architecture.md` §5.3.4 (substrate-vs-helper
    /// rule). Returning data, not behavior — the chord-binding +
    /// open-and-position logic lives in the mode's handler
    /// closure registered via the M.10.1.b
    /// `ActionHandlerRegistry`.
    pub fn translate_composed_to_source(&self, cursor: Position) -> Option<(BufferId, Position)> {
        let state = self.lock_state();
        let mut composed_cursor: u32 = 0;
        for excerpt in &state.excerpts {
            let next = composed_cursor.saturating_add(excerpt.line_count());
            if cursor.line < next {
                let offset = cursor.line - composed_cursor;
                let source_row = excerpt.start_line.saturating_add(offset);
                return Some((
                    excerpt.source,
                    Position {
                        line: source_row,
                        byte: cursor.byte,
                    },
                ));
            }
            composed_cursor = next;
        }
        None
    }

    /// M.5 (2026-06-01): grow / shrink the excerpt containing
    /// `cursor_row` by `delta_rows` total rows, split
    /// symmetrically above and below.
    ///
    /// Behaviour:
    /// - `delta_rows > 0` expands; `delta_rows < 0` contracts;
    ///   `delta_rows == 0` is a no-op.
    /// - Symmetric split: `delta_rows / 2` above, the remainder
    ///   below. With `delta_rows = 5`: 2 rows added above, 3 below.
    /// - Clip: `start_line` never goes below 0; `end_line` never
    ///   exceeds the source's last row (read from
    ///   `source.snapshot().buffer.line_count() - 1`).
    /// - Min size: if the contract would make `start > end`,
    ///   no-op (excerpt keeps its existing range).
    /// - No-op when the cursor sits outside every excerpt OR the
    ///   excerpt's source isn't in the source map (closed source).
    ///
    /// Recomposes + publishes after the mutation, matching
    /// `append_excerpts` / `replace_excerpts` shape.
    pub fn expand_excerpt_at(&self, cursor_row: u32, delta_rows: i32) {
        if delta_rows == 0 {
            return;
        }
        let mut state = self.lock_state();
        let Some(idx) = crate::motions::containing_excerpt_index(&state.excerpts, cursor_row)
        else {
            return;
        };
        // `containing_excerpt_index` returns the last excerpt for
        // rows past the view's end (motion-friendly). For
        // expand-context the cursor must actually sit within the
        // excerpt's composed range — verify by checking the
        // start-rows table.
        let starts = crate::motions::excerpt_start_rows(&state.excerpts);
        let excerpt_start_composed = starts[idx];
        let excerpt_end_composed = excerpt_start_composed
            .saturating_add(state.excerpts[idx].line_count())
            .saturating_sub(1);
        if cursor_row > excerpt_end_composed {
            return;
        }

        let source_id = state.excerpts[idx].source;
        let Some(source) = state.sources.get(&source_id) else {
            return;
        };
        // CV.3: content space — an excerpt may only expand onto lines
        // the source actually has, never the phantom one ropey reports
        // after a terminating newline.
        let source_line_count = source.snapshot().buffer.content_line_count() as i64;
        if source_line_count == 0 {
            return;
        }

        // Symmetric split: half above (integer divide rounds
        // toward zero, so positive delta puts the extra below;
        // negative delta puts the extra above).
        let above = (delta_rows / 2) as i64;
        let below = (delta_rows as i64) - above;

        let current_start = state.excerpts[idx].start_line as i64;
        let current_end = state.excerpts[idx].end_line as i64;

        let new_start = (current_start - above).clamp(0, source_line_count - 1);
        let new_end = (current_end + below).clamp(0, source_line_count - 1);

        if new_end < new_start {
            // Contract would invert: leave the excerpt as-is.
            return;
        }
        if new_start == current_start && new_end == current_end {
            // Hit both clips; no observable change.
            return;
        }

        state.excerpts[idx].start_line = new_start as u32;
        state.excerpts[idx].end_line = new_end as u32;

        let snapshot = compose_snapshot(
            self.inner.id,
            &state.sources,
            &state.excerpts,
            state.selections.clone(),
        );
        let translation = RowTranslation::build(&state.excerpts);
        drop(state);
        self.inner.snapshot_cell.store(snapshot);
        self.inner.row_translation.store(Arc::new(translation));
    }

    /// M.2.b.2 (2026-06-01): append excerpts to the end of the
    /// view. Used by async providers streaming batches of
    /// results (project-search, lsp-references, etc.). Any
    /// excerpts whose source isn't present are silently
    /// skipped (log + drop). Recomposes + publishes after the
    /// mutation.
    pub fn append_excerpts(&self, excerpts: Vec<Excerpt>) {
        if excerpts.is_empty() {
            return;
        }
        let mut state = self.lock_state();
        // MH.B1 (2026-06-19): collect the excerpts actually added
        // this call (mirroring the skip-if-missing-source filter)
        // so we can compose + translate ONLY the batch, not the
        // accumulated total.
        let mut added: Vec<Excerpt> = Vec::with_capacity(excerpts.len());
        for ex in excerpts {
            if !state.sources.contains_key(&ex.source) {
                // Silently drop — the provider is responsible
                // for adding the source first via `add_source`
                // if it's a new file. M.6 SearchProvider does
                // this in its scan task.
                continue;
            }
            state.excerpts.push(ex.clone());
            added.push(ex);
        }
        if added.is_empty() {
            // Every excerpt was dropped (unknown source). Nothing
            // to compose or publish — leave the view untouched.
            return;
        }
        // MH.B1 (2026-06-19): compose ONLY the batch we just added.
        // `compose_text_from_sources` carries no cross-excerpt
        // state — each line is pulled independently and given a
        // trailing `\n` — so composing the added sub-list yields
        // exactly the bytes that sub-list contributes to the full
        // composition. Appending those bytes to the END of the
        // existing composed_doc is therefore byte-identical to
        // `from_text(old_full_text + batch_text)`, at O(batch)
        // instead of O(total). Pinned by
        // `incremental_append_matches_full_build`.
        let batch_text = compose_text_from_sources(&state.sources, &added);
        // K.4.7: stamp with monotonic seq — `Document::apply_edit`
        // bumps text_version, but starting from a `from_text`-built
        // rope (which begins at text_version=0) means an empty view
        // streaming its first batch would land at text_version=1
        // regardless; we use the inner monotonic publish_seq so the
        // MatrixVersion advances on every publish.
        let seq = self.inner.publish_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let selections = state.selections.clone();
        // MH.B1: extend (not rebuild) the row translation by the
        // batch's entries. Pure concatenation == byte-identical to
        // `RowTranslation::build(&state.excerpts)`.
        let mut translation = (*self.inner.row_translation.load_full()).clone();
        translation.append(&added);
        drop(state);
        // MH.B1 (2026-06-02 superseded): append the batch text to
        // the END of composed_doc rather than rebuilding it from
        // all sources. This APPENDS — it preserves any local edits
        // already present in composed_doc (the previous full-rebuild
        // here CLOBBERED them). Insert-at-end keeps the local rope
        // authoritative + in sync with the growing excerpt set so
        // user edits land on a rope reflecting current content.
        let snapshot = {
            let mut doc = self
                .inner
                .composed_doc
                .lock()
                .expect("composed_doc mutex poisoned");
            if !batch_text.is_empty() {
                // Insert at the very end of the current rope. The
                // end Position is derived from the rope's byte
                // length so it is correct whether or not the rope
                // ends in a trailing newline.
                let end_byte = doc.buffer().byte_len() as usize;
                let at = doc
                    .buffer()
                    .byte_to_position(end_byte)
                    .expect("end-of-rope position is always in bounds");
                // Best-effort: a failed append leaves the rope as-is
                // (recoverable — we still publish the extended
                // translation + bumped seq). Never panic on this path.
                if let Err(err) = doc.apply_edit(Edit::insert(at, batch_text)) {
                    tracing::debug!(
                        ?err,
                        "append_excerpts: composed_doc append failed; rope unchanged"
                    );
                }
            }
            let mut snapshot = snapshot_from_composed_doc(&doc, self.inner.id, selections);
            snapshot.text_version = seq;
            snapshot
        };
        self.inner.snapshot_cell.store(snapshot);
        self.inner.row_translation.store(Arc::new(translation));
    }

    /// M.2.b.2 (2026-06-01): replace the entire excerpt list +
    /// source map atomically. Used by providers reacting to a
    /// query / filter change (e.g. user refines a search). The
    /// previous excerpts are dropped; the new set is composed
    /// + published in one mutation.
    pub fn replace_excerpts(
        &self,
        sources: HashMap<BufferId, Arc<dyn Document>>,
        excerpts: Vec<Excerpt>,
    ) {
        for ex in &excerpts {
            if !sources.contains_key(&ex.source) {
                // Same skip-and-continue behaviour as append.
                // Provider's responsibility to keep sources
                // map coherent with excerpts.
            }
        }
        let mut state = self.lock_state();
        // SS.2: re-baseline against the NEW source set. Carrying the old
        // map forward would leave fingerprints for sources that are gone
        // and none for the ones just added — a refresh must not inherit
        // a stale baseline.
        state.source_fingerprints = sources
            .iter()
            .filter_map(|(id, src)| fingerprint_source(src).map(|fp| (*id, fp)))
            .collect();
        state.sources = sources;
        state.excerpts = excerpts;
        // M.11 (2026-06-02): same rebuild as `append_excerpts` —
        // keep composed_doc in sync with the new excerpt set so
        // user edits land on a rope reflecting current content.
        let composed_text = compose_text_from_sources(&state.sources, &state.excerpts);
        let new_composed_doc = lattice_core::Document::from_text(composed_text);
        // K.4.7: same monotonic seq stamp as append_excerpts.
        let seq = self.inner.publish_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let mut snapshot =
            snapshot_from_composed_doc(&new_composed_doc, self.inner.id, state.selections.clone());
        snapshot.text_version = seq;
        let translation = RowTranslation::build(&state.excerpts);
        drop(state);
        {
            let mut doc = self
                .inner
                .composed_doc
                .lock()
                .expect("composed_doc mutex poisoned");
            *doc = new_composed_doc;
        }
        self.inner.snapshot_cell.store(snapshot);
        self.inner.row_translation.store(Arc::new(translation));
    }

    /// M.2.b.2 (2026-06-01): add a source buffer to the view's
    /// source map. Subsequent `append_excerpts` calls can
    /// reference it. Idempotent: re-adding an existing source
    /// updates the handle reference (which may have been
    /// replaced via slot-replacement upstream).
    ///
    /// K.4.7 (2026-06-07): if `set_lang_registry` has been called,
    /// detect the source's language from its path and create a
    /// long-lived `SyntaxHandle` for it. The handle worker runs on
    /// the tokio runtime of the caller (the scan task); subsequent
    /// reparsing is async and wait-free at read time.
    /// SS.2: the recorded on-disk baseline for `source`, if any.
    /// `None` for a pathless (synthetic) source, which is never stale.
    pub fn source_fingerprint(
        &self,
        source: BufferId,
    ) -> Option<lattice_core::on_disk::OnDiskFingerprint> {
        self.lock_state().source_fingerprints.get(&source).cloned()
    }

    pub fn add_source(&self, id: BufferId, source: Arc<dyn Document>) {
        let mut state = self.lock_state();
        if let Some(fp) = fingerprint_source(&source) {
            state.source_fingerprints.insert(id, fp);
        }
        state.sources.insert(id, source.clone());
        let path = source.path();
        tracing::debug!(
            buffer = ?id,
            path = ?path,
            has_lang_registry = self.inner.lang_registry.get().is_some(),
            "add_source: checking for syntax handle creation"
        );
        if let Some(lr) = self.inner.lang_registry.get() {
            let lang = Lang::detect_from_path(path.as_deref());
            tracing::debug!(buffer = ?id, ?lang, "add_source: detected language");
            if lang != Lang::Plain {
                match Syntax::for_language_with_registry(lang, lr.clone()) {
                    Ok(Some(mut syntax)) => {
                        let snap = source.snapshot();
                        let text = snap.buffer.as_string();
                        syntax.parse(&text);
                        let handle = SyntaxHandle::seeded(syntax);
                        state.source_syntax.insert(id, Arc::new(handle));
                        self.inner
                            .excerpt_syntax_gen
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        tracing::debug!(buffer = ?id, ?lang, "add_source: syntax handle created");
                    }
                    Ok(None) => {
                        tracing::debug!(buffer = ?id, ?lang, "add_source: no grammar registered");
                    }
                    Err(e) => {
                        tracing::debug!(buffer = ?id, ?lang, error = ?e, "add_source: grammar error");
                    }
                }
            }
        }
    }

    /// K.4.7 (2026-06-07): enable per-source syntax highlighting.
    /// Called by the host immediately after `create_multibuffer_view`.
    /// Subsequent `add_source` calls use the registry to detect the
    /// source language and create a `SyntaxHandle` per source.
    ///
    /// Also retroactively creates handles for sources that were already
    /// added before this call (the common case: `new(sources, ...)` is
    /// called first, then `set_lang_registry` wires highlighting).
    pub fn set_lang_registry(&self, lr: Arc<LangRegistry>) {
        if self.inner.lang_registry.set(lr.clone()).is_err() {
            return;
        }
        let mut state = self.lock_state();
        let ids: Vec<(BufferId, Arc<dyn Document>)> = state
            .sources
            .iter()
            .filter(|(id, _)| !state.source_syntax.contains_key(id))
            .map(|(id, src)| (*id, src.clone()))
            .collect();
        let mut added = 0u64;
        for (id, source) in ids {
            let lang = Lang::detect_from_path(source.path().as_deref());
            if lang == Lang::Plain {
                continue;
            }
            if let Ok(Some(mut syntax)) = Syntax::for_language_with_registry(lang, lr.clone()) {
                let snap = source.snapshot();
                let text = snap.buffer.as_string();
                syntax.parse(&text);
                let handle = SyntaxHandle::seeded(syntax);
                state.source_syntax.insert(id, Arc::new(handle));
                added += 1;
            }
        }
        if added > 0 {
            self.inner
                .excerpt_syntax_gen
                .fetch_add(added, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// K.4.7 (2026-06-08): monotonic version that increments whenever
    /// per-source `SyntaxHandle`s are created. Used by `publish_render_state`
    /// to invalidate the cells-worker cache when handles are first populated.
    pub fn excerpt_syntax_version(&self) -> u64 {
        self.inner
            .excerpt_syntax_gen
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// K.4.7 (2026-06-07): per-excerpt syntax entries for the cells
    /// worker. Each entry is `(composed_start, composed_end,
    /// source_start, handle)` where `composed_start`/`composed_end`
    /// are inclusive row bounds in the composed snapshot (0-indexed)
    /// and `source_start` is the first source row mapped to
    /// `composed_start`. Only excerpts with a `SyntaxHandle` are
    /// included.
    pub fn excerpt_syntax_entries(&self) -> Vec<(u32, u32, u32, Arc<SyntaxHandle>)> {
        let state = self.lock_state();
        let mut entries = Vec::new();
        let mut composed_row = 0u32;
        for ex in &state.excerpts {
            let line_count = ex.end_line.saturating_sub(ex.start_line) + 1;
            if let Some(handle) = state.source_syntax.get(&ex.source) {
                let composed_end = composed_row + line_count - 1;
                entries.push((composed_row, composed_end, ex.start_line, handle.clone()));
            }
            composed_row += line_count;
        }
        entries
    }

    /// Recompose the snapshot from current source state.
    /// Rebuilds the composed buffer + row translation, then
    /// publishes via `ArcSwap::store`.
    ///
    /// M.1 shipped this as a manual API; M.4 wires automatic
    /// invocation via source-edit event subscriptions. M.2.b.2
    /// kept the public surface stable but rerouted reads through
    /// the Mutex.
    pub fn recompose(&self) {
        let state = self.lock_state();
        let new_snapshot = compose_snapshot(
            self.inner.id,
            &state.sources,
            &state.excerpts,
            state.selections.clone(),
        );
        let new_translation = RowTranslation::build(&state.excerpts);
        drop(state);
        self.inner.snapshot_cell.store(new_snapshot);
        self.inner.row_translation.store(Arc::new(new_translation));
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, MultibufferState> {
        self.inner
            .state
            .lock()
            .expect("MultibufferInner state mutex poisoned")
    }

    /// M.4 (2026-06-01): the view's current headerline status.
    /// Lock-free read.
    pub fn headerline(&self) -> Arc<HeaderlineStatus> {
        self.inner.headerline.load_full()
    }

    /// M.4 (2026-06-01): set the view's headerline status.
    /// Publishes `MultibufferHeaderlineChanged` on the event bus
    /// the handle was attached to (no-op if
    /// [`Self::attach_event_subscriptions`] hasn't been called —
    /// the status still updates locally).
    pub fn set_headerline(&self, status: HeaderlineStatus) {
        let bus = self
            .inner
            .subscriptions
            .lock()
            .ok()
            .and_then(|book| book.bus.clone());
        self.inner.headerline.store(Arc::new(status.clone()));
        self.inner
            .headerline_version
            .fetch_add(1, Ordering::Release);
        if let Some(bus) = bus {
            bus.publish_typed(MultibufferHeaderlineChanged {
                view: self.inner.buffer_id,
                status,
            });
        }
    }

    /// M.4 (2026-06-01): subscribe the view to its sources'
    /// `DocumentChanged` / `DocumentClosed` events. On a source
    /// change, the view auto-recomposes; on a source close, the
    /// view publishes [`MultibufferSourceClosed`] and removes the
    /// source from its internal map.
    ///
    /// Subscriptions live until the handle drops — `MultibufferInner::drop`
    /// unsubscribes via the bookkeeping. The spawned forwarder
    /// task holds a `Weak<MultibufferInner>` so it exits cleanly
    /// once the handle is dropped.
    ///
    /// Idempotent: re-calling on an already-attached handle is a
    /// no-op. Requires a current tokio runtime context (the
    /// forwarder task is spawned via `tokio::spawn`).
    pub fn attach_event_subscriptions(&self, events: &Arc<lattice_runtime::EventBus>) {
        let mut book = self
            .inner
            .subscriptions
            .lock()
            .expect("subscriptions mutex poisoned");
        if book.bus.is_some() {
            // Already attached.
            return;
        }
        // Drop into the no-tokio-runtime case gracefully: the
        // event-bus subscribe still works, but the forwarder
        // task can't spawn. Match `register_multibuffer_modes`'s
        // shape.
        if tokio::runtime::Handle::try_current().is_err() {
            tracing::debug!(
                "MultibufferDocumentHandle::attach_event_subscriptions: no tokio runtime; \
                 skipping forwarder task wiring (expected in test paths)"
            );
            // Still stash the bus so set_headerline can publish.
            book.bus = Some(events.clone());
            return;
        }

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<lattice_protocol::Event>();
        let sub_id = events.subscribe(
            lattice_runtime::EventFilter::kinds(vec![
                lattice_protocol::EventKind::DocumentChanged,
                lattice_protocol::EventKind::DocumentClosed,
            ]),
            lattice_runtime::SubscriptionTarget::Channel(tx),
        );
        book.ids.push(sub_id);
        book.bus = Some(events.clone());
        drop(book);

        let weak_inner = Arc::downgrade(&self.inner);
        let events_for_task = events.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let Some(inner) = weak_inner.upgrade() else {
                    break;
                };
                match event {
                    lattice_protocol::Event::DocumentChanged { id, edits, .. } => {
                        if let Some(source_id) = source_buffer_for_document_id(&inner, id) {
                            // M.4.1: slide excerpts whose start
                            // row sits strictly below the edit's
                            // original end. Edits that overlap
                            // an excerpt's range, or sit below
                            // it, leave excerpts alone — the
                            // recompose picks up the new
                            // content for in-excerpt edits.
                            slide_anchors_for_source(&inner, source_id, &edits);
                            recompose_inner(&inner);
                        }
                    }
                    lattice_protocol::Event::DocumentClosed { id } => {
                        if let Some(source_id) = source_buffer_for_document_id(&inner, id) {
                            // Remove the source from our map.
                            if let Ok(mut state) = inner.state.lock() {
                                state.sources.remove(&source_id);
                            }
                            // Publish the typed event so providers
                            // pick up the close + choose policy.
                            events_for_task.publish_typed(MultibufferSourceClosed {
                                view: inner.buffer_id,
                                source: source_id,
                            });
                            // Recompose: removed source's
                            // excerpts will render empty rows
                            // (no entries in the source map).
                            recompose_inner(&inner);
                        }
                    }
                    _ => {}
                }
            }
        });
    }
}

/// Translate a [`DocumentId`] (carried by `Event::DocumentChanged`
/// / `Event::DocumentClosed`) to the `BufferId` key in our source
/// map, if the document is one of our sources.
fn source_buffer_for_document_id(
    inner: &Arc<MultibufferInner>,
    document_id: DocumentId,
) -> Option<BufferId> {
    let state = inner.state.lock().ok()?;
    state
        .sources
        .iter()
        .find(|(_, h)| h.id() == document_id)
        .map(|(id, _)| *id)
}

/// M.4.1 (2026-06-01): walk the `AppliedEdit`s from a source's
/// `DocumentChanged` event and slide excerpts of that source
/// whose `start_line` sits strictly below the edit's original
/// end row. Edits overlapping or below the excerpt leave it
/// alone — the recompose picks up new content for in-excerpt
/// edits; below-edits don't affect the excerpt's position.
///
/// Behaviourally equivalent to anchor tracking in the
/// linewise case (which is what excerpts care about — they're
/// line-bounded). A first-class `Anchor` primitive (line + col +
/// generation) can land later if column-precise tracking
/// proves load-bearing (none of the M.4.1 worked examples
/// need it).
///
/// Conservative bias: edits whose original_range end is AT or
/// ABOVE the excerpt's start_line don't slide. Erring against
/// false-positive slides keeps the user's mental model stable
/// when an edit straddles an excerpt boundary.
fn slide_anchors_for_source(
    inner: &Arc<MultibufferInner>,
    source: BufferId,
    edits: &[lattice_protocol::event::AppliedEdit],
) {
    if edits.is_empty() {
        return;
    }
    let Ok(mut state) = inner.state.lock() else {
        return;
    };
    for edit in edits {
        let old_end_row = edit.original_range.end.line;
        let new_end_row = edit.inserted_range.end.line;
        let row_delta = (new_end_row as i64) - (old_end_row as i64);
        if row_delta == 0 {
            continue;
        }
        for excerpt in state.excerpts.iter_mut() {
            if excerpt.source != source {
                continue;
            }
            if old_end_row < excerpt.start_line {
                let new_start = (excerpt.start_line as i64).saturating_add(row_delta).max(0);
                let new_end = (excerpt.end_line as i64).saturating_add(row_delta).max(0);
                excerpt.start_line = new_start as u32;
                excerpt.end_line = new_end as u32;
            }
        }
    }
}

/// Recompose an Inner — same shape as `MultibufferDocumentHandle::recompose`
/// but works against an `Arc<MultibufferInner>` so the forwarder
/// task can call it without holding a strong handle reference.
fn recompose_inner(inner: &Arc<MultibufferInner>) {
    let Ok(state) = inner.state.lock() else {
        return;
    };
    let new_snapshot = compose_snapshot(
        inner.id,
        &state.sources,
        &state.excerpts,
        state.selections.clone(),
    );
    let new_translation = RowTranslation::build(&state.excerpts);
    drop(state);
    inner.snapshot_cell.store(new_snapshot);
    inner.row_translation.store(Arc::new(new_translation));
}

impl Document for MultibufferDocumentHandle {
    fn snapshot(&self) -> Arc<DocumentSnapshot> {
        self.inner.snapshot_cell.load()
    }

    fn snapshot_cache(&self) -> SnapshotCache {
        SnapshotCache::new(self.inner.snapshot_cell.clone())
    }

    /// M.3 (2026-06-01): translate the composed-coordinate `edit`
    /// to its source-coordinate equivalent and forward to the
    /// source document's `apply_edit`.
    ///
    /// The returned `Pending<AppliedEdit>` carries the source's
    /// AppliedEdit — ranges + delta in source coordinates. Caller
    /// recompose()s (M.3) or auto-subscribes (M.4) to reflect.
    ///
    /// Boundary clipping (architecture §4): if `edit.range.end`
    /// extends past the start excerpt's last composed row, the
    /// end is clipped to the end-of-line of the excerpt's last
    /// source row. The edit's contribution to subsequent
    /// excerpts (and their sources) is dropped — matching Zed's
    /// "edits stay in the excerpt" rule. Out-of-range edits
    /// (cursor past view end, no excerpts) return
    /// `RuntimeError::ReadOnly`.
    fn apply_edit(&self, edit: Edit) -> Pending<AppliedEdit> {
        // M.11 (2026-06-02): byte-identical shape to
        // `RopeDocumentHandle::apply_edit` — mutate the LOCAL
        // composed rope, publish the new snapshot, return
        // synchronously. The multibuffer is now a true buffer at
        // the substrate level: insert-mode keystrokes, motions,
        // operators, undo/redo all flow through the same
        // `Document` code path as a regular `Document`. No
        // cross-actor round-trip, no Pending::spawn, no race
        // with M.4 forwarder. Source actors catch up async via
        // the source-forwarder task spawned at construction.
        //
        // Look up where the edit lands so the source forwarder
        // can translate to source coords. The lookup uses the
        // PRE-edit row translation (the only one that still
        // describes the source position the cursor is on); after
        // the edit the composed_doc's contents diverge from the
        // source until the forwarder catches up, but the row
        // translation continues to describe excerpt boundaries
        // in the composed view (one row per excerpt today).
        let state = self.lock_state();
        let source_forward = resolve_edit_target(&state, edit.range.start).map(|target| {
            let source_edit = build_source_edit(&target, &edit);
            SourceForwardMsg::Edit {
                source_handle: target.source_handle.clone(),
                source_edit,
            }
        });
        drop(state);

        // Mutate the local composed_doc synchronously.
        let applied = {
            let mut doc = self
                .inner
                .composed_doc
                .lock()
                .expect("composed_doc mutex poisoned");
            match doc.apply_edit(edit) {
                Ok(applied) => {
                    // Publish the post-edit snapshot before
                    // releasing the lock so consumers see a
                    // consistent (rope, version) pair.
                    let selections = self.lock_state().selections.clone();
                    let snap = snapshot_from_composed_doc(&doc, self.inner.id, selections);
                    self.inner.snapshot_cell.store(snap);
                    applied
                }
                Err(e) => return Pending::ready(Err(RuntimeError::Core(e))),
            }
        };

        // Fire-and-forget source forwarding. `try_send` so a full
        // unbounded mpsc (effectively impossible) doesn't block;
        // the forwarder task drains FIFO order so source actors
        // see edits in the same order the user typed them. If
        // construction had no tokio runtime (test paths), the rx
        // half was dropped and try_send returns Err — that's
        // expected, edits stay local-only.
        if let Some(msg) = source_forward {
            let _ = self.inner.source_forward_tx.send(msg);
        }

        Pending::ready(Ok(applied))
    }

    /// M.3 (2026-06-01): translate + forward each edit to its
    /// source. The batch is serialised through `apply_edit`
    /// per-edit and combined via `Pending::spawn` so the
    /// returned `Pending` resolves asynchronously without
    /// blocking the runtime. Multi-source batches dispatch
    /// each sub-edit sequentially; per-edit parallelism is a
    /// later refinement once a consumer needs it.
    fn apply_edit_batch(&self, edits: Vec<Edit>) -> Pending<Vec<AppliedEdit>> {
        // M.11 (2026-06-02): same shape as `apply_edit` — local
        // mutation per edit, sync source-forward enqueue, return
        // synchronously. No Pending::spawn, no cross-actor
        // round-trip.
        let mut applied_results = Vec::with_capacity(edits.len());
        let mut forwards = Vec::with_capacity(edits.len());

        for edit in edits {
            // Pre-resolve source forward target before mutating
            // (the row translation uses composed coords valid
            // before this edit lands).
            let state = self.lock_state();
            let source_forward = resolve_edit_target(&state, edit.range.start).map(|target| {
                let source_edit = build_source_edit(&target, &edit);
                SourceForwardMsg::Edit {
                    source_handle: target.source_handle.clone(),
                    source_edit,
                }
            });
            drop(state);

            let mut doc = self
                .inner
                .composed_doc
                .lock()
                .expect("composed_doc mutex poisoned");
            match doc.apply_edit(edit) {
                Ok(applied) => {
                    let selections = self.lock_state().selections.clone();
                    let snap = snapshot_from_composed_doc(&doc, self.inner.id, selections);
                    self.inner.snapshot_cell.store(snap);
                    applied_results.push(applied);
                    if let Some(msg) = source_forward {
                        forwards.push(msg);
                    }
                }
                Err(e) => return Pending::ready(Err(RuntimeError::Core(e))),
            }
        }

        for msg in forwards {
            let _ = self.inner.source_forward_tx.send(msg);
        }

        Pending::ready(Ok(applied_results))
    }

    /// M.3 (2026-06-01): fan undo out to every source the view
    /// references. Each source's undo independently rolls back
    /// its most recent action; the multibuffer's recompose
    /// (M.4 auto-driven, M.3 manual) reflects.
    ///
    /// v1 atomicity: each source's undo stack is independent.
    /// When the user typed in the multibuffer last, each
    /// affected source's most-recent entry IS that
    /// multibuffer-originated edit, so a fan-out undo rolls
    /// back the right thing. If a third pane edited a source
    /// in between, that source's most-recent is the third
    /// pane's edit — `u` from the multibuffer rolls THAT back.
    /// M.6+ slices can add transaction tracking if the
    /// independent-stack behaviour proves surprising.
    fn undo(&self) -> Pending<Vec<AppliedEdit>> {
        // M.11 (2026-06-02): undo operates on the LOCAL
        // composed_doc — synchronous, no cross-actor round-trip.
        // Pre-fix this used `Pending::spawn(async move { for
        // source in sources { source.undo().await } })` which
        // deadlocked the editor actor's current_thread runtime
        // when the host called via `block_on(self.document.undo())`
        // (same root cause as the apply_edit freeze before M.11).
        //
        // Source-side undo is NOT forwarded in v1 — the local
        // composed_doc retains its own undo stack and reversing
        // here gives the user visual undo of multibuffer edits.
        // Sources stay forward-edited (the search-and-replace
        // workflow's "I changed my mind on this hunk" semantics
        // need richer transaction tracking to coordinate
        // multi-source undo; queued for a future slice).
        let applied = {
            let mut doc = self
                .inner
                .composed_doc
                .lock()
                .expect("composed_doc mutex poisoned");
            match doc.undo() {
                Ok(applied) => {
                    let selections = self.lock_state().selections.clone();
                    let snap = snapshot_from_composed_doc(&doc, self.inner.id, selections);
                    self.inner.snapshot_cell.store(snap);
                    applied
                }
                Err(e) => return Pending::ready(Err(RuntimeError::Core(e))),
            }
        };
        Pending::ready(Ok(applied))
    }

    fn redo(&self) -> Pending<Vec<AppliedEdit>> {
        // M.11 (2026-06-02): redo operates on the LOCAL
        // composed_doc — symmetric with `undo` above.
        let applied = {
            let mut doc = self
                .inner
                .composed_doc
                .lock()
                .expect("composed_doc mutex poisoned");
            match doc.redo() {
                Ok(applied) => {
                    let selections = self.lock_state().selections.clone();
                    let snap = snapshot_from_composed_doc(&doc, self.inner.id, selections);
                    self.inner.snapshot_cell.store(snap);
                    applied
                }
                Err(e) => return Pending::ready(Err(RuntimeError::Core(e))),
            }
        };
        Pending::ready(Ok(applied))
    }

    fn save(&self) -> Pending<std::path::PathBuf> {
        // 2026-06-10: save every dirty source back to disk. Generic
        // for ALL multibuffer views (narrow, project-search, future
        // diff / references): the host's `save_blocking` calls this
        // `Document::save` uniformly, so `:w` persists the underlying
        // files with no kind-branch.
        //
        // Edits reach sources ASYNC via the source-forwarder, so we
        // FLUSH it first — a barrier sent through the same FIFO
        // channel guarantees every queued edit has been applied to
        // (and awaited on) its source actor before we read + save the
        // sources. Without the flush, `:w` races the forwarder and
        // could persist a source missing the user's last keystrokes.
        // SS.3: carry each source's baseline alongside it so the write
        // can be refused if the file moved underneath the view.
        let sources: Vec<(
            Arc<dyn Document>,
            Option<lattice_core::on_disk::OnDiskFingerprint>,
        )> = {
            let state = self.lock_state();
            state
                .sources
                .iter()
                .map(|(id, src)| (src.clone(), state.source_fingerprints.get(id).cloned()))
                .collect()
        };
        let forward_tx = self.inner.source_forward_tx.clone();
        Pending::spawn(async move {
            // Flush barrier. Best-effort: if the forwarder is gone the
            // send fails and we fall through (sources are as current
            // as they can be); if `done` is dropped the await errors
            // and we likewise proceed.
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            if forward_tx
                .send(SourceForwardMsg::Flush { done: done_tx })
                .is_ok()
            {
                let _ = done_rx.await;
            }

            // Save each dirty source; report the first saved path for
            // the `:w` echo. A view whose sources are all clean reports
            // the first source's path as a no-op success (matching
            // vim's `:w` on an unmodified buffer); an empty view (no
            // sources) is `ReadOnly`.
            let mut saved_path: Option<std::path::PathBuf> = None;
            let mut fallback_path: Option<std::path::PathBuf> = None;
            let mut last_err: Option<RuntimeError> = None;
            let mut skipped: Vec<std::path::PathBuf> = Vec::new();
            for (source, baseline) in &sources {
                if fallback_path.is_none() {
                    fallback_path = source.path();
                }
                if !source.dirty() {
                    continue;
                }
                // SS.3: refuse a source whose file changed on disk after
                // the view snapshotted it. Writing it would silently
                // discard that external change — the whole reason this
                // guard exists.
                //
                // Refuse the SOURCE, not the save: a 30-file view must
                // not fail wholesale because one file moved, and a save
                // that fails wholesale teaches a `:w!` habit that
                // discards exactly what is being protected.
                if let (Some(path), Some(baseline)) = (source.path(), baseline.as_ref())
                    && is_stale_on_disk(&path, baseline)
                {
                    tracing::warn!(
                        path = %path.display(),
                        "multibuffer save: source changed on disk since the view loaded it; \
                         refusing to overwrite"
                    );
                    skipped.push(path);
                    continue;
                }
                match source.save().await {
                    Ok(path) => {
                        saved_path.get_or_insert(path);
                    }
                    Err(e) => {
                        last_err = Some(e);
                    }
                }
            }
            if let Some(e) = last_err {
                return Err(e);
            }
            if !skipped.is_empty() {
                // Surface WHICH files, not a count: the user has to go
                // look at them, and the recovery (refresh the view —
                // `gr`, `:copen`, `:search`) re-reads from disk. The
                // other sources already persisted above; this is a
                // partial-success report.
                return Err(RuntimeError::SourcesChangedOnDisk { paths: skipped });
            }
            saved_path.or(fallback_path).ok_or(RuntimeError::ReadOnly)
        })
    }

    fn save_as(&self, _path: std::path::PathBuf) -> Pending<()> {
        Pending::ready(Err(RuntimeError::ReadOnly))
    }

    fn set_selections(&self, selections: SelectionSet) -> Pending<()> {
        // K.4.5 (2026-06-02): selections ARE view-owned in the
        // composed coordinate space (M.3 design — they don't
        // propagate to sources). Prior shape returned
        // `Err(ReadOnly)` which left the snapshot's selections
        // at `SelectionSet::default()`, breaking Visual-mode
        // highlight painting on multibuffer views
        // (`Editor::visual_selection_range` reads
        // `self.document.selections().primary()` uniformly
        // across BufferKinds — the right fix is for the
        // Document impl to honour the call, not for callers
        // to special-case multibuffer).
        //
        // Store the new selection set in `state.selections`
        // and rebuild the snapshot so the next snapshot read
        // sees the updated selections. Synchronous (Mutex-
        // routed write + ArcSwap publish) so
        // `set_selections_blocking` callers see the change
        // immediately. See [[feedback_buffers_no_special_case]].
        let selections = Arc::new(selections);
        let snapshot = {
            let mut state = self.lock_state();
            state.selections = Arc::clone(&selections);
            compose_snapshot(self.inner.id, &state.sources, &state.excerpts, selections)
        };
        self.inner.snapshot_cell.store(snapshot);
        Pending::ready(Ok(()))
    }

    /// K.4.6 follow-up (2026-06-02): publish the composed→source
    /// row map so the gutter can show original file line numbers
    /// (429, 430, 432, …) instead of composed-row indices
    /// (0, 1, 2, …). Walks the published `RowTranslation` once
    /// per call; cheap (typical N = hundreds-to-thousands; called
    /// once per render-state publish, NOT per keystroke).
    fn display_line_numbers(&self) -> Option<Arc<[u32]>> {
        let translation = self.inner.row_translation.load_full();
        let rows: Vec<u32> = translation
            .entries
            .iter()
            .map(|e| match e {
                RowEntry::Excerpt { source_row, .. } => *source_row,
            })
            .collect();
        Some(Arc::from(rows.into_boxed_slice()))
    }

    // K.4.7 (2026-06-08): mode owns its highlighting surface. The host's
    // `publish_render_state` calls these uniformly on every document; no
    // `BufferKind` branch needed there.
    fn excerpt_highlights(&self) -> Vec<lattice_cells::ExcerptHighlight> {
        let state = self.lock_state();
        let mut out = Vec::new();
        let mut composed_row = 0u32;
        for ex in &state.excerpts {
            let line_count = ex.end_line.saturating_sub(ex.start_line) + 1;
            if let Some(handle) = state.source_syntax.get(&ex.source) {
                out.push(lattice_cells::ExcerptHighlight {
                    composed_start: composed_row,
                    composed_end: composed_row + line_count - 1,
                    source_start: ex.start_line,
                    highlighter: Arc::clone(handle) as Arc<dyn lattice_cells::ExcerptHighlighter>,
                });
            }
            composed_row += line_count;
        }
        out
    }

    fn excerpt_syntax_version(&self) -> u64 {
        self.inner
            .excerpt_syntax_gen
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn dispatch_with_cancel(
        &self,
        invocation: CommandInvocation,
        cursor: Position,
        cancel: CancellationToken,
    ) -> Pending<Effect> {
        // K.4.11 (2026-06-02): the multibuffer now owns grammar
        // dispatch directly. Pre-K.4.11 this returned
        // Err(ReadOnly), and `Editor::dispatch_blocking`
        // carried a kind-branch that ran `lattice_grammar::execute`
        // against a scratch `lattice_core::Document` built from
        // the composed snapshot. That was a paramount-#3 violation
        // (kind-special-casing in the host); the registry now
        // lives on `MultibufferInner` (passed at construction
        // per spawn_document's shape) so the multibuffer can do
        // the same work itself and the host's kind-branch
        // disappears.
        //
        // Resulting Effect flows through the usual host pipeline:
        // motions return a cursor Effect; operators return
        // Effect::Edits in composed coordinates that the host's
        // apply_edit_blocking routes through this handle's
        // `apply_edit`, which translates to source coordinates +
        // forwards to the source document (M.3).
        // K.4.11.perf-fix (2026-06-02): Pre-fix this routed
        // `snapshot.buffer.as_string() → Document::from_text(&composed)`,
        // which allocated O(composed_size) bytes + rebuilt a fresh
        // Rope on EVERY keystroke on the App thread. For a search
        // multibuffer growing to 100s of KB during the scan, that
        // was tens of ms per `j`/`k` motion — the user-visible
        // "cursor moves after a lot of delay" + "lattice freezes
        // during scan" regressions. Architectural relocation per
        // [[feedback_no_ui_thread_work]]: reuse the snapshot's
        // existing Rope-backed Buffer directly. `Buffer::clone`
        // is `Rope::clone` which is Arc-backed + O(1); the new
        // path is one Arc bump per keystroke.
        let snapshot = self.snapshot();
        let mut scratch = lattice_core::Document::from_buffer(snapshot.buffer.clone());
        let buffer_id = self.inner.buffer_id;
        // B3b: owned snapshot for this dispatch (was `Arc::clone`); a
        // runtime plugin registration is live on the next keystroke.
        let registry = self.inner.registry.load_full();
        // N.1.5: tree-sitter text objects (`af` / `daf` / `znaf`) inside a
        // multibuffer view resolve against the SOURCE syntax, not the
        // composed text (which has no parse tree of its own). Build a
        // composed↔source `ScopeResolver` from the per-excerpt source
        // SyntaxSnapshots (K.4.7) and hand it to the dispatcher. Only
        // built for Operator / TextObject invocations -- motions
        // (j/k/w/...) never read it, so navigation in a big search view
        // stays O(1) per keystroke (paramount #1).
        let composed_resolver = if matches!(
            registry.lookup(invocation.command).map(|s| s.kind),
            Some(CommandKind::Operator | CommandKind::TextObject)
        ) {
            ComposedScopeResolver::build(&self.lock_state())
        } else {
            None
        };
        let result = execute_with_env(
            &registry,
            &mut scratch,
            buffer_id,
            cursor,
            invocation,
            &cancel,
            lattice_grammar::TextObjectEnv {
                scope_resolver: composed_resolver
                    .as_ref()
                    .map(|r| r as &dyn lattice_grammar::ScopeResolver),
                // Comment objects inside multibuffer views would resolve
                // the per-excerpt source's comment leader -- deferred (a
                // follow-up, like N.1.5's scope resolver was). v1: None.
                comment_syntax: None,
                // TS.1: multibuffer dispatch resolves motions/text-objects, not
                // grammar actions; no tree-snapshot handle needed.
                syntax: None,
            },
        )
        .map_err(RuntimeError::Grammar);
        Pending::ready(result)
    }
}

// ──────────────────────────────────────────────────────────────
// N.1.5 — composed↔source scope resolver
// ──────────────────────────────────────────────────────────────

/// N.1.5: a [`lattice_grammar::ScopeResolver`] that bridges composed
/// multibuffer coordinates to the per-excerpt SOURCE syntax. Tree-sitter
/// text objects dispatched against a multibuffer view (narrow / search /
/// project-diff) resolve their enclosing scope against the source file's
/// parse tree — the composed text has no tree of its own — and the
/// result is mapped back to composed coordinates so the operator applies
/// in the view. Built per Operator/TextObject dispatch from a snapshot
/// of the excerpt layout + source `SyntaxSnapshot`s (cheap Arc clones);
/// see [`MultibufferDocumentHandle::dispatch_with_cancel`].
struct ComposedScopeResolver {
    excerpts: Vec<ComposedExcerptSpan>,
    snapshots: HashMap<BufferId, Arc<SyntaxSnapshot>>,
}

/// One excerpt's placement: its source rows `[start_line, start_line +
/// line_count)` occupy composed rows `[composed_offset, composed_offset
/// + line_count)`.
struct ComposedExcerptSpan {
    source: BufferId,
    start_line: u32,
    line_count: u32,
    composed_offset: u32,
}

impl ComposedScopeResolver {
    /// Snapshot the excerpt layout + per-source `SyntaxSnapshot`s.
    /// Returns `None` when no excerpt has a source `SyntaxHandle` (a
    /// plain-text / no-language multibuffer) — the resolver would
    /// resolve nothing, so the caller passes `None` and text objects
    /// degrade to empty (graceful operator no-op).
    fn build(state: &MultibufferState) -> Option<Self> {
        let mut excerpts = Vec::with_capacity(state.excerpts.len());
        let mut snapshots: HashMap<BufferId, Arc<SyntaxSnapshot>> = HashMap::new();
        let mut composed_offset = 0u32;
        for ex in &state.excerpts {
            excerpts.push(ComposedExcerptSpan {
                source: ex.source,
                start_line: ex.start_line,
                line_count: ex.line_count(),
                composed_offset,
            });
            composed_offset = composed_offset.saturating_add(ex.line_count());
            if !snapshots.contains_key(&ex.source)
                && let Some(h) = state.source_syntax.get(&ex.source)
            {
                snapshots.insert(ex.source, h.snapshot());
            }
        }
        if snapshots.is_empty() {
            return None;
        }
        Some(Self {
            excerpts,
            snapshots,
        })
    }
}

impl lattice_grammar::ScopeResolver for ComposedScopeResolver {
    fn scope_at(
        &self,
        composed_line: u32,
        col_byte: u32,
        suffix: &str,
    ) -> Option<lattice_protocol::position::Range> {
        for ex in &self.excerpts {
            // First composed row PAST this excerpt (exclusive upper bound).
            let composed_end_exclusive = ex.composed_offset.saturating_add(ex.line_count);
            if composed_line < composed_end_exclusive {
                let source_line = ex.start_line + (composed_line - ex.composed_offset);
                let snap = self.snapshots.get(&ex.source)?;
                let src = snap.scope_at_cursor(source_line, col_byte, suffix)?;
                // Clamp the source range to THIS excerpt's source bounds,
                // then map back to composed rows. A scope extending past
                // the excerpt (a narrowed sub-region) is clipped to the
                // visible rows; a clamped edge loses its byte column
                // (falls to col 0) since it no longer marks a real token.
                // `saturating_sub` guards a (shouldn't-happen) zero-line
                // excerpt rather than underflowing to u32::MAX.
                let ex_last = ex
                    .start_line
                    .saturating_add(ex.line_count)
                    .saturating_sub(1);
                let cs = src.start.line.max(ex.start_line);
                let ce = src.end.line.min(ex_last);
                if cs > ce {
                    return None;
                }
                let start_byte = if src.start.line >= ex.start_line {
                    src.start.byte
                } else {
                    0
                };
                let end_byte = if src.end.line <= ex_last {
                    src.end.byte
                } else {
                    0
                };
                return Some(lattice_protocol::position::Range::new(
                    lattice_protocol::position::Position::new(
                        ex.composed_offset + (cs - ex.start_line),
                        start_byte,
                    ),
                    lattice_protocol::position::Position::new(
                        ex.composed_offset + (ce - ex.start_line),
                        end_byte,
                    ),
                ));
            }
        }
        None
    }

    // TSM.1: stub -- the real composed↔source tree walk for structural
    // motions (`]f`/`[c`/…) lands in a later slice, mirroring how
    // `scope_at` maps source scopes back to composed coordinates.
    // Graceful no-op until then (heuristic #5).
    fn scope_toward(
        &self,
        _composed_line: u32,
        _col_byte: u32,
        _suffix: &str,
        _dir: lattice_grammar::NavDir,
        _boundary: lattice_grammar::NavBoundary,
        _count: u32,
    ) -> Option<lattice_protocol::Position> {
        None
    }
}

// ──────────────────────────────────────────────────────────────
// M.3 translation helpers
// ──────────────────────────────────────────────────────────────

/// One excerpt + position pair resolved from a composed-coordinate
/// edit point. M.4 will likely read `source_id` for live-update
/// subscription bookkeeping; M.3 only needs the handle.
#[allow(dead_code)]
struct EditTarget {
    source_id: BufferId,
    source_handle: Arc<dyn Document>,
    /// The composed `Position` we translated from.
    composed_start: Position,
    /// Source-coordinate position equivalent to `composed_start`.
    source_start: Position,
    /// Last composed row of the containing excerpt (inclusive).
    excerpt_end_composed_row: u32,
    /// Last source row of the containing excerpt (inclusive).
    excerpt_end_source_row: u32,
}

/// Walk excerpts in display order to find the one that contains
/// `composed_pos`. Returns the source handle + the source
/// position equivalent. `None` when the position is past the
/// last excerpt or the source map doesn't have the excerpt's
/// source (an invariant violation, treated as out-of-range).
fn resolve_edit_target(state: &MultibufferState, composed_pos: Position) -> Option<EditTarget> {
    let mut composed_cursor: u32 = 0;
    for excerpt in &state.excerpts {
        let lines = excerpt.line_count();
        let next_cursor = composed_cursor.saturating_add(lines);
        if composed_pos.line < next_cursor {
            let offset_in_excerpt = composed_pos.line - composed_cursor;
            let source_row = excerpt.start_line.saturating_add(offset_in_excerpt);
            let source_handle = state.sources.get(&excerpt.source)?.clone();
            return Some(EditTarget {
                source_id: excerpt.source,
                source_handle,
                composed_start: composed_pos,
                source_start: Position {
                    line: source_row,
                    byte: composed_pos.byte,
                },
                excerpt_end_composed_row: next_cursor.saturating_sub(1),
                excerpt_end_source_row: excerpt.end_line,
            });
        }
        composed_cursor = next_cursor;
    }
    None
}

/// Build the source-coordinate `Edit` from a translation target +
/// the original composed-coordinate edit. Applies boundary
/// clipping: if `edit.range.end` extends past the start
/// excerpt's last row, the end is clipped to the end-of-line
/// of the excerpt's last source row.
fn build_source_edit(target: &EditTarget, edit: &Edit) -> Edit {
    let end_composed = edit.range.end;
    let source_end = if end_composed.line > target.excerpt_end_composed_row {
        // Boundary clip: pull `end` back to end-of-line of the
        // excerpt's last source row. Length comes from the
        // source's current snapshot — we already hold the
        // handle.
        let snap = target.source_handle.snapshot();
        let line_text = snap.buffer.line(target.excerpt_end_source_row);
        let line_byte_len = line_text
            .as_deref()
            .map(|s| s.trim_end_matches('\n').len() as u32)
            .unwrap_or(0);
        Position {
            line: target.excerpt_end_source_row,
            byte: line_byte_len,
        }
    } else {
        let row_offset = end_composed.line.saturating_sub(target.composed_start.line);
        Position {
            line: target.source_start.line.saturating_add(row_offset),
            byte: end_composed.byte,
        }
    };

    Edit {
        range: lattice_protocol::position::Range {
            start: target.source_start,
            end: source_end,
        },
        kind: edit.kind.clone(),
    }
}

// M.11 (2026-06-02): `translate_applied_to_composed` deleted —
// edits now apply to the local composed_doc, so AppliedEdit
// is already in composed coords. No translation needed.

impl std::fmt::Debug for MultibufferDocumentHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.lock_state();
        f.debug_struct("MultibufferDocumentHandle")
            .field("id", &self.inner.id)
            .field("buffer_id", &self.inner.buffer_id)
            .field("sources", &state.sources.len())
            .field("excerpts", &state.excerpts.len())
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MultibufferError {
    /// An excerpt referenced a `source` BufferId not present in
    /// the sources map. M.2.b.2 (2026-06-01) relaxed
    /// `EmptyExcerpts` (the async-provider pattern needs empty
    /// views).
    #[error("excerpt {excerpt:?} references unknown source buffer {source_buffer:?}")]
    UnknownSource {
        excerpt: ExcerptId,
        source_buffer: BufferId,
    },
}

// ─────────────────────────────────────────────────────────────────
// Header provider (VirtualRowProvider impl) — moved from
// `lattice-host::multibuffer` in M.2.b.1.
// ─────────────────────────────────────────────────────────────────

/// Namespace prefix for multibuffer header provider ids.
/// Distinct from the diff filler / overlay namespaces (`0xD1FF_*`).
const MULTIBUFFER_EXCERPT_HEADER_NAMESPACE: u64 = 0xBBBB_0001_0000_0000;

pub fn multibuffer_excerpt_header_provider_id(buffer_id: BufferId) -> ProviderId {
    MULTIBUFFER_EXCERPT_HEADER_NAMESPACE | u64::from(buffer_id.0)
}

// ─────────────────────────────────────────────────────────────────
// T.7 (2026-06-18): mode-owned theme elements for the excerpt header.
//
// The multibuffer MODE owns these elements + their defaults — they
// are NOT core builtins ([[feedback_mode_owns_its_surface]]). The
// mode registers them (idempotent by name) so the excerpt-header
// provider can resolve them into BAKED `u32` colors at row-build
// time (off the UI thread, the established cell/virtual-row pattern).
//
// This is the extensibility acid test: a provider crate adds ZERO
// host `Theme`/style fields and ZERO renderer match arms — it
// registers elements + references them, and the renderer paints
// `VirtualRow.bg` / `Cell.fg` generically.
// ─────────────────────────────────────────────────────────────────

/// Element name: the excerpt-header row's backdrop (a neutral
/// surface tint, distinct from the diff-deletion-block red the
/// renderer would otherwise fall a `bg: None` Generic row through to).
pub const ELEM_EXCERPT_HEADER: &str = "multibuffer.excerpt_header";
/// Element name: the excerpt-header file-path / title foreground.
pub const ELEM_EXCERPT_HEADER_PATH: &str = "multibuffer.excerpt_header.path";
/// Element name: the excerpt-header match-count foreground.
pub const ELEM_EXCERPT_HEADER_COUNT: &str = "multibuffer.excerpt_header.count";

// MH.A4 (2026-06-20): view-status headerline foreground elements.
// The status row (` ⟳ Searching … ` / ` ◆ N hits ` / ` ■ reason `)
// previously hardcoded its fg as ad-hoc hex (0x999999 / 0x44cc88 /
// 0xff4444); these map the three states onto palette role-keys so a
// `:colorscheme` swap recolors the status line.
/// Element name: the in-progress status row foreground (neutral grey).
pub const ELEM_STATUS_IN_PROGRESS: &str = "multibuffer.status.in_progress";
/// Element name: the completed status row foreground (green).
pub const ELEM_STATUS_COMPLETE: &str = "multibuffer.status.complete";
/// Element name: the failed status row foreground (red).
pub const ELEM_STATUS_FAILED: &str = "multibuffer.status.failed";
/// Element name: the emphasised-term foreground (accent) — e.g. the
/// project-search query woven into the status label.
pub const ELEM_STATUS_QUERY: &str = "multibuffer.status.query";

/// Register the multibuffer mode's theme elements against `reg`.
/// Idempotent by name (safe to call on every mode activation /
/// every view creation). Returns the interned [`ElementId`]s the
/// excerpt-header provider bakes from.
///
/// `owner` is the mode's id string ([`MultibufferMode::mode_id`] →
/// `as_str`), so `:describe-element` attributes these to the mode
/// rather than core. `lattice-theme` is a leaf crate that can't
/// depend on `lattice-mode`, hence the string owner.
pub fn register_multibuffer_theme_elements(
    reg: &dyn lattice_theme::ThemeRegistry,
    owner: ElementOwner,
) -> MultibufferHeaderElementIds {
    let backdrop = reg.register(
        ElementName::from(ELEM_EXCERPT_HEADER.to_string()),
        owner.clone(),
        // Neutral surface backdrop (Catppuccin "surface0"-ish), NOT
        // the diff-deletion red — that was the smell this slice fixes.
        StyleSpec::new().bg(Color::Rgb(0x31, 0x32, 0x44)),
        "Multibuffer excerpt header backdrop.",
    );
    let path = reg.register(
        ElementName::from(ELEM_EXCERPT_HEADER_PATH.to_string()),
        owner.clone(),
        StyleSpec::new().fg(ColorRef::Palette("blue".into())),
        "Excerpt header file path.",
    );
    let count = reg.register(
        ElementName::from(ELEM_EXCERPT_HEADER_COUNT.to_string()),
        owner.clone(),
        StyleSpec::new().fg(ColorRef::Palette("overlay2".into())),
        "Excerpt header match count.",
    );
    // MH.A4: status-row state colors. `in_progress` maps to the
    // muted `subtext` grey (the old 0x999999), `complete` to `green`
    // (old 0x44cc88), `failed` to `red` (old 0xff4444) — the nearest
    // semantic role-keys in the registered palette.
    let status_in_progress = reg.register(
        ElementName::from(ELEM_STATUS_IN_PROGRESS.to_string()),
        owner.clone(),
        StyleSpec::new().fg(ColorRef::Palette("subtext".into())),
        "Multibuffer in-progress status foreground.",
    );
    let status_complete = reg.register(
        ElementName::from(ELEM_STATUS_COMPLETE.to_string()),
        owner.clone(),
        StyleSpec::new().fg(ColorRef::Palette("green".into())),
        "Multibuffer completed status foreground.",
    );
    let status_failed = reg.register(
        ElementName::from(ELEM_STATUS_FAILED.to_string()),
        owner.clone(),
        StyleSpec::new().fg(ColorRef::Palette("red".into())),
        "Multibuffer failed status foreground.",
    );
    // The emphasised-term accent (e.g. the search query) maps to the
    // palette `yellow` — the conventional search-highlight hue,
    // distinct from the green/red/blue state colors above.
    let status_query = reg.register(
        ElementName::from(ELEM_STATUS_QUERY.to_string()),
        owner,
        StyleSpec::new().fg(ColorRef::Palette("yellow".into())),
        "Multibuffer emphasised-term (query) status foreground.",
    );
    MultibufferHeaderElementIds {
        backdrop,
        path,
        count,
        status_in_progress,
        status_complete,
        status_failed,
        status_query,
    }
}

/// The interned [`ElementId`]s for the excerpt-header elements,
/// captured once at view-creation and held by the provider so each
/// `collect()` is an array-index resolve (`resolved.get(id)`), never
/// a per-row name lookup. `Copy`; cheap to thread through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MultibufferHeaderElementIds {
    pub backdrop: ElementId,
    pub path: ElementId,
    pub count: ElementId,
    /// MH.A4: view-status row foregrounds, interned alongside the
    /// header elements so the status provider's `collect()` resolves
    /// by array index (never a per-row name lookup).
    pub status_in_progress: ElementId,
    pub status_complete: ElementId,
    pub status_failed: ElementId,
    /// The accent foreground for an emphasised term (e.g. the search
    /// query) woven into the status label.
    pub status_query: ElementId,
}

impl Default for MultibufferHeaderElementIds {
    /// All-INVALID placeholder for test paths that build the provider
    /// without a theme registry. Reads against an empty/default table
    /// return `Style::empty()` (no bg/fg baked) — never a panic.
    fn default() -> Self {
        Self {
            backdrop: ElementId::INVALID,
            path: ElementId::INVALID,
            count: ElementId::INVALID,
            status_in_progress: ElementId::INVALID,
            status_complete: ElementId::INVALID,
            status_failed: ElementId::INVALID,
            status_query: ElementId::INVALID,
        }
    }
}

/// Emits one virtual row per excerpt header, anchored above the
/// excerpt's first composed row. Cheap-clone reference to the
/// multibuffer handle; re-reads excerpts on each `collect()`.
///
/// T.7: when a [`ThemeRegistryHandle`] + the header element ids are
/// supplied, `collect()` resolves the backdrop + path elements and
/// bakes them into the row's `bg` / cells' `fg` — resolution happens
/// off-thread at row-build time, never on a paint path. Without a
/// handle (test paths) the rows fall back to `bg: None` (the renderer
/// then picks its kind default).
///
/// `Debug` is hand-rolled because `ThemeRegistryHandle` (a
/// `dyn ThemeRegistry` trait object) is not `Debug`; the
/// `VirtualRowProvider` trait requires `Debug`.
pub struct MultibufferExcerptHeaderProvider {
    multibuffer: MultibufferDocumentHandle,
    /// T.7: `None` for test paths that don't wire a theme registry.
    theme: Option<ThemeRegistryHandle>,
    elements: MultibufferHeaderElementIds,
    /// MH.A3: `ui.nerd_fonts`, captured at view construction so the
    /// leading file-type icon picks the nerd-font glyph vs the BMP
    /// fallback. Folded into `version()` so a global toggle re-runs
    /// `collect()`.
    ///
    /// MH.A3 follow-on: this is the GLOBAL default captured once at
    /// view creation — a live per-buffer `ui.nerd_fonts` toggle does
    /// not yet re-render the header. Reading per-buffer
    /// `ui.nerd_fonts` here needs the `FrameView::for_buffer` seam
    /// plumbed into the provider, deferred to avoid over-building.
    nerd_fonts: bool,
}

impl std::fmt::Debug for MultibufferExcerptHeaderProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MultibufferExcerptHeaderProvider")
            .field("buffer_id", &self.multibuffer.buffer_id())
            .field("has_theme", &self.theme.is_some())
            .field("elements", &self.elements)
            .field("nerd_fonts", &self.nerd_fonts)
            .finish()
    }
}

impl MultibufferExcerptHeaderProvider {
    /// Construct without theme wiring — headers render with no baked
    /// bg/fg (the renderer's kind default applies). Test convenience;
    /// production uses [`Self::with_theme`]. Defaults `nerd_fonts` to
    /// `false` (BMP-fallback palette).
    pub fn new(multibuffer: MultibufferDocumentHandle) -> Self {
        Self {
            multibuffer,
            theme: None,
            elements: MultibufferHeaderElementIds::default(),
            nerd_fonts: false,
        }
    }

    /// T.7: construct with the resolved theme handle + the header
    /// element ids so `collect()` bakes the registered backdrop / path
    /// colors into the header rows. MH.A3: `nerd_fonts` selects the
    /// icon palette (captured at view creation; see the struct field
    /// doc for the per-buffer-toggle follow-on note).
    pub fn with_theme(
        multibuffer: MultibufferDocumentHandle,
        theme: ThemeRegistryHandle,
        elements: MultibufferHeaderElementIds,
        nerd_fonts: bool,
    ) -> Self {
        Self {
            multibuffer,
            theme: Some(theme),
            elements,
            nerd_fonts,
        }
    }
}

impl VirtualRowProvider for MultibufferExcerptHeaderProvider {
    fn id(&self) -> ProviderId {
        multibuffer_excerpt_header_provider_id(self.multibuffer.buffer_id())
    }

    fn version(&self) -> u64 {
        // T.7: fold the resolved theme version into the provider
        // version so a `:colorscheme` / `:set ui.*` change re-runs
        // `collect()` and re-bakes the header colors. Mirrors how the
        // cell matrix invalidates via `MatrixVersion::theme =
        // resolved().version()` (dispatch.rs). The worker's
        // fingerprint is `hash[(id, version)]`, so a version bump on
        // any axis (excerpt content OR theme) triggers a rebuild.
        let content = self.multibuffer.snapshot().version;
        let theme = self
            .theme
            .as_ref()
            .map(|t| t.resolved().version())
            .unwrap_or(0);
        // MH.A3: fold the nerd_fonts term so a global toggle re-runs
        // `collect()` (the worker fingerprint is `hash[(id, version)]`).
        content
            .wrapping_add(theme)
            .wrapping_add(if self.nerd_fonts { 1 } else { 0 })
    }

    fn collect(&self) -> Vec<VirtualRow> {
        // Resolve the backdrop bg + all three segment fgs ONCE per
        // collect (off the UI thread), then bake into each header
        // row. `0` (the Cell "transparent / use default" sentinel) is
        // the fg fallback when an element is unresolved.
        //
        // header_fg = `multibuffer.excerpt_header` (the base/backdrop
        //   element)'s `.fg` — currently unset in the default
        //   registration (only `.bg`), so this resolves to 0 until a
        //   future slice adds a base fg; the renderer then uses its
        //   default fg. path_fg / count_fg come from the dedicated
        //   `.path` / `.count` elements.
        let resolved = self.theme.as_ref().map(|t| t.resolved());
        let header_bg: Option<u32> = resolved
            .as_ref()
            .and_then(|r| r.get(self.elements.backdrop).bg)
            .map(|c| c.to_rgb_u32(0));
        let header_fg: u32 = resolved
            .as_ref()
            .and_then(|r| r.get(self.elements.backdrop).fg)
            .map(|c| c.to_rgb_u32(0))
            .unwrap_or(0);
        let path_fg: u32 = resolved
            .as_ref()
            .and_then(|r| r.get(self.elements.path).fg)
            .map(|c| c.to_rgb_u32(0))
            .unwrap_or(0);
        let count_fg: u32 = resolved
            .as_ref()
            .and_then(|r| r.get(self.elements.count).fg)
            .map(|c| c.to_rgb_u32(0))
            .unwrap_or(0);
        let nerd_fonts = self.nerd_fonts;
        compose_header_rows(&self.multibuffer.excerpts(), |excerpt| {
            header_cells(&excerpt.header, nerd_fonts, header_fg, path_fg, count_fg)
        })
        .into_iter()
        .map(|mut row| {
            row.bg = header_bg;
            row
        })
        .collect()
    }
}

/// MH.A3: rich per-segment header builder. Replaces the old
/// single-fg `themed_header_cells`. Layout (per multibuffer-views.md
/// §3.8):
///
/// ```text
///   filename.rs  src/multibuffer/  ·  7 matches
/// ```
///
/// - leading file-type **icon** (resolved live from `header.path`'s
///   extension via [`lattice_core::ui::icons::glyph_for_entry`];
///   nerd glyph when `nerd_fonts`, BMP fallback otherwise — both the
///   same cell width) + a trailing space, fg `header_fg`.
/// - **basename** (`header.path.file_name()`, else `header.title`),
///   fg `header_fg`.
/// - a space + the **dir** path (dimmed) when `header.path` has a
///   parent, fg `path_fg`.
/// - ` · {n} matches` / ` · {n} match` when `header.match_count` is
///   `Some`, fg `count_fg`.
/// - empty title AND no path ⇒ `[untitled]` fallback in `header_fg`.
///
/// Colours are **resolved in `collect()` and passed in** — never
/// baked at excerpt-creation time — so a live `ui.nerd_fonts` /
/// `:colorscheme` toggle re-renders the whole surface. A fg of `0`
/// is the Cell "use renderer default" sentinel (test paths without a
/// theme).
pub(crate) fn header_cells(
    header: &ExcerptHeader,
    nerd_fonts: bool,
    header_fg: u32,
    path_fg: u32,
    count_fg: u32,
) -> Arc<[Cell]> {
    let mut cells: Vec<Cell> = Vec::new();

    let push = |cells: &mut Vec<Cell>, s: &str, fg: u32| {
        for ch in s.chars() {
            cells.push(Cell::new(ch as u32, fg, 0, 0));
        }
    };

    match &header.path {
        Some(path) => {
            // Leading file-type icon (already 2 cells wide: glyph +
            // trailing space in both palettes) — fg `header_fg`.
            let icon = lattice_core::ui::icons::glyph_for_entry(path, false, nerd_fonts);
            push(&mut cells, icon, header_fg);

            // Basename (bright). Fall back to the whole path string,
            // then to the title, if `file_name()` is unavailable.
            let basename: std::borrow::Cow<'_, str> = path
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_else(|| path.to_string_lossy());
            if basename.is_empty() {
                push(&mut cells, header.title.as_str(), header_fg);
            } else {
                push(&mut cells, basename.as_ref(), header_fg);
            }

            // Directory path (dimmed) when there's a parent.
            if let Some(parent) = path.parent() {
                let parent_str = parent.to_string_lossy();
                if !parent_str.is_empty() {
                    push(&mut cells, "  ", path_fg);
                    push(&mut cells, parent_str.as_ref(), path_fg);
                }
            }
        }
        None => {
            // No path → use the title; `[untitled]` when even that is
            // empty so the row never collapses to nothing.
            if header.title.is_empty() {
                push(&mut cells, "[untitled]", header_fg);
            } else {
                push(&mut cells, header.title.as_str(), header_fg);
            }
        }
    }

    // Match-count badge (` · N matches`), fg `count_fg`.
    if let Some(n) = header.match_count {
        let badge = if n == 1 {
            format!(" · {n} match")
        } else {
            format!(" · {n} matches")
        };
        push(&mut cells, badge.as_str(), count_fg);
    }

    Arc::from(cells)
}

/// Pure function from excerpt list → header virtual rows.
/// Emits ONE row per distinct consecutive source — i.e. when N
/// consecutive excerpts share `excerpt.source` (BufferId), only
/// the first contributes a header row, anchored `Above` its
/// first composed line. The rest advance the composed cursor
/// without emitting a header.
///
/// K.4.6 follow-up (2026-06-02): pre-fix this emitted one row
/// per excerpt unconditionally, which broke "1 header per file"
/// for providers like search that emit multiple excerpts per
/// file (one per hit cluster). The dedup happens here in
/// substrate, not in providers — every provider gets the
/// correct "1 header per source" behavior by default.
/// Providers that intentionally want one header per excerpt
/// can emit excerpts with distinct synthetic `source` BufferIds.
pub fn compose_header_rows(
    excerpts: &[Excerpt],
    mut render_cells: impl FnMut(&Excerpt) -> Arc<[Cell]>,
) -> Vec<VirtualRow> {
    let mut rows = Vec::with_capacity(excerpts.len());
    let mut composed_cursor: u32 = 0;
    let mut last_source: Option<BufferId> = None;
    for excerpt in excerpts {
        if last_source != Some(excerpt.source) {
            let cells = render_cells(excerpt);
            rows.push(VirtualRow {
                anchor_line: composed_cursor,
                position: AnchorPosition::Above,
                cells,
                height: 1,
                kind: VirtualRowKind::Generic,
                bg: None,
                scales: None,
            });
            last_source = Some(excerpt.source);
        }
        composed_cursor = composed_cursor.saturating_add(excerpt.line_count());
    }
    rows
}

// ─────────────────────────────────────────────────────────────────
// M.6.5 (2026-06-08): view-status sticky headerline
// ─────────────────────────────────────────────────────────────────

/// Namespace prefix for the view-status headerline provider.
/// Distinct from the excerpt-header namespace (`0xBBBB_0001_*`).
const MULTIBUFFER_STATUS_NAMESPACE: u64 = 0xBBBB_0002_0000_0000;

pub fn multibuffer_status_provider_id(buffer_id: BufferId) -> ProviderId {
    MULTIBUFFER_STATUS_NAMESPACE | u64::from(buffer_id.0)
}

/// MH.A4: fallback hex used when no theme is wired (test paths). These
/// are the pre-MH.A4 hardcoded colors; production resolves the
/// `multibuffer.status.*` theme elements instead.
const STATUS_IN_PROGRESS_FALLBACK_FG: u32 = 0x999999;
const STATUS_COMPLETE_FALLBACK_FG: u32 = 0x44cc88;
const STATUS_FAILED_FALLBACK_FG: u32 = 0xff4444;
/// Fallback accent for the emphasised term when the theme (or the
/// `multibuffer.status.query` element) is unresolved — a warm yellow,
/// the conventional search-highlight hue.
const STATUS_QUERY_FALLBACK_FG: u32 = 0xf9e2af;

/// Pure function: `HeaderlineStatus` → display row (or `None` when idle).
///
/// MH.A4: the per-state foregrounds are **resolved in `collect()` and
/// passed in** (from the `multibuffer.status.*` theme elements) — never
/// hardcoded here — so a `:colorscheme` swap recolors the status line.
/// Color legend:
///   InProgress — `in_progress_fg` (neutral grey), theme bg
///   Complete   — `complete_fg` (green), green `◆` prefix, theme bg
///   Failed     — `failed_fg` (red), red `■` prefix, theme bg
fn render_multibuffer_status(
    status: &HeaderlineStatus,
    in_progress_fg: u32,
    complete_fg: u32,
    failed_fg: u32,
    query_fg: u32,
) -> Option<HeaderlineRow> {
    let (text, emphasis): (String, Option<&str>) = match status {
        HeaderlineStatus::Idle => return None,
        HeaderlineStatus::InProgress {
            label,
            count: None,
            emphasis,
        } => (format!(" ⟳ {label} … "), emphasis.as_deref()),
        HeaderlineStatus::InProgress {
            label,
            count: Some(n),
            emphasis,
        } => (format!(" ⟳ {label} ({n}) … "), emphasis.as_deref()),
        HeaderlineStatus::Complete { summary, emphasis } => {
            (format!(" ◆ {summary} "), emphasis.as_deref())
        }
        HeaderlineStatus::Failed { reason } => (format!(" ■ {reason} "), None),
    };
    let fg: u32 = match status {
        HeaderlineStatus::InProgress { .. } => in_progress_fg,
        HeaderlineStatus::Complete { .. } => complete_fg,
        HeaderlineStatus::Failed { .. } => failed_fg,
        HeaderlineStatus::Idle => unreachable!(),
    };
    // Char-index range of the emphasised term within `text` (first
    // occurrence). Only the query cells get `query_fg`; everything else
    // keeps the state fg. Non-empty emphasis that isn't found → no accent.
    let emphasis_range: Option<(usize, usize)> = emphasis.filter(|e| !e.is_empty()).and_then(|e| {
        text.find(e).map(|byte_start| {
            let char_start = text[..byte_start].chars().count();
            (char_start, char_start + e.chars().count())
        })
    });
    let cells: Arc<[Cell]> = text
        .chars()
        .enumerate()
        .map(|(i, c)| {
            let cell_fg = match emphasis_range {
                Some((start, end)) if i >= start && i < end => query_fg,
                _ => fg,
            };
            Cell::new(c as u32, cell_fg, 0, 0)
        })
        .collect::<Vec<_>>()
        .into();
    Some(HeaderlineRow { cells, bg: None })
}

/// Sticky headerline provider that surfaces the view's
/// [`HeaderlineStatus`] as a pinned row above line 0.
///
/// Implements [`Headerline`] directly — the status lives in
/// `MultibufferInner` (behind an `ArcSwap`); no extra dedicated
/// state allocation is needed.
pub struct MultibufferStatusProvider {
    multibuffer: MultibufferDocumentHandle,
    /// MH.A4: `None` for test paths that don't wire a theme registry —
    /// `render()` then falls back to the pre-MH.A4 hardcoded hex.
    theme: Option<ThemeRegistryHandle>,
    /// MH.A4: the interned status element ids (mirrors the
    /// excerpt-header provider's `elements`), captured once at
    /// view-creation so `render()` resolves by array index.
    elements: MultibufferHeaderElementIds,
}

impl std::fmt::Debug for MultibufferStatusProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MultibufferStatusProvider")
            .field("buffer_id", &self.multibuffer.buffer_id())
            .field("has_theme", &self.theme.is_some())
            .field("elements", &self.elements)
            .finish()
    }
}

impl MultibufferStatusProvider {
    /// Construct without theme wiring — the status row renders with the
    /// pre-MH.A4 fallback hex. Test convenience; production uses
    /// [`Self::with_theme`].
    pub fn new(multibuffer: MultibufferDocumentHandle) -> Self {
        Self {
            multibuffer,
            theme: None,
            elements: MultibufferHeaderElementIds::default(),
        }
    }

    /// MH.A4: construct with the resolved theme handle + the interned
    /// status element ids so `render()` resolves the
    /// `multibuffer.status.*` foregrounds (mirrors the excerpt-header
    /// provider's `with_theme`).
    pub fn with_theme(
        multibuffer: MultibufferDocumentHandle,
        theme: ThemeRegistryHandle,
        elements: MultibufferHeaderElementIds,
    ) -> Self {
        Self {
            multibuffer,
            theme: Some(theme),
            elements,
        }
    }

    pub fn into_provider(self, buffer_id: BufferId) -> HeaderlineProvider {
        HeaderlineProvider::new(multibuffer_status_provider_id(buffer_id), Arc::new(self))
    }

    /// Resolve the three status foregrounds from the wired theme,
    /// falling back to the pre-MH.A4 hex per-state when the theme or an
    /// element is unresolved (test paths).
    fn status_fgs(&self) -> (u32, u32, u32, u32) {
        let resolved = self.theme.as_ref().map(|t| t.resolved());
        let resolve = |id: ElementId, fallback: u32| -> u32 {
            resolved
                .as_ref()
                .and_then(|r| r.get(id).fg)
                .map(|c| c.to_rgb_u32(fallback))
                .unwrap_or(fallback)
        };
        (
            resolve(
                self.elements.status_in_progress,
                STATUS_IN_PROGRESS_FALLBACK_FG,
            ),
            resolve(self.elements.status_complete, STATUS_COMPLETE_FALLBACK_FG),
            resolve(self.elements.status_failed, STATUS_FAILED_FALLBACK_FG),
            resolve(self.elements.status_query, STATUS_QUERY_FALLBACK_FG),
        )
    }
}

impl Headerline for MultibufferStatusProvider {
    fn version(&self) -> u64 {
        // MH.A4: fold the resolved theme version into the status
        // version so a `:colorscheme` / `:set ui.*` change re-runs
        // `render()` and re-resolves the status foregrounds. Mirrors
        // the excerpt-header provider's version composition.
        let content = self
            .multibuffer
            .inner
            .headerline_version
            .load(Ordering::Acquire);
        let theme = self
            .theme
            .as_ref()
            .map(|t| t.resolved().version())
            .unwrap_or(0);
        content.wrapping_add(theme)
    }

    fn render(&self) -> Option<HeaderlineRow> {
        let (in_progress_fg, complete_fg, failed_fg, query_fg) = self.status_fgs();
        render_multibuffer_status(
            &self.multibuffer.inner.headerline.load(),
            in_progress_fg,
            complete_fg,
            failed_fg,
            query_fg,
        )
    }
}

/// M.10.4 (2026-06-03): host glue for the
/// `:multibuffer-expand [n]` / `:multibuffer-contract [n]`
/// ex-commands. Looks up the active buffer's typed
/// `MultibufferDocumentHandle` via the
/// `MultibufferRegistryHandle` service and calls
/// `expand_excerpt_at(cursor_row, delta)`. No-op when the
/// active buffer isn't a multibuffer view, the service isn't
/// registered (test harness), or the cursor is out of range.
///
/// Replaces `Editor::do_multibuffer_expand` which used to live
/// in `lattice-host::dispatch`. Per
/// [[feedback_mode_owns_its_surface]] + `mode-architecture.md`
/// §5.3.4: this helper is the substrate-side counterpart to
/// the ex-command registration in `MultibufferMode`. Host's
/// `apply_effect` arm calls this directly — no longer
/// trampolines through `Action::MultibufferExpand` +
/// `Editor::do_multibuffer_expand`.
pub fn multibuffer_expand_excerpt_at(
    services: &lattice_mode::services::ServiceRegistry,
    buffer_id: BufferId,
    cursor_row: u32,
    delta: i32,
) {
    let Some(mb_registry) = services.get::<crate::registry::MultibufferRegistryHandle>() else {
        return;
    };
    let Some(view) = mb_registry.handle(buffer_id) else {
        return;
    };
    view.expand_excerpt_at(cursor_row, delta);
}

/// Default header-rendering: `── <title> ──` (box-drawing
/// rules). Empty title yields a row of box rules only.
pub fn default_header_cells(excerpt: &Excerpt) -> Arc<[Cell]> {
    let title = &excerpt.header.title;
    let mut cells = Vec::new();
    for _ in 0..2 {
        cells.push(Cell::with_codepoint('─' as u32));
    }
    if !title.is_empty() {
        cells.push(Cell::with_codepoint(' ' as u32));
        for ch in title.chars() {
            cells.push(Cell::with_codepoint(ch as u32));
        }
        cells.push(Cell::with_codepoint(' ' as u32));
    }
    for _ in 0..2 {
        cells.push(Cell::with_codepoint('─' as u32));
    }
    Arc::from(cells)
}

// ─────────────────────────────────────────────────────────────────
// Internals
// ─────────────────────────────────────────────────────────────────

fn next_multibuffer_document_id() -> DocumentId {
    static NEXT: AtomicU64 = AtomicU64::new(0x1000_0000_0000_0000);
    DocumentId::new(NEXT.fetch_add(1, Ordering::Relaxed))
}

/// M.11 (2026-06-02): build the composed text from sources at
/// initialization time. The result feeds `Document::from_text`
/// so the composed_doc starts in sync with the sources. After
/// this point, the composed_doc evolves through `apply_edit` —
/// sources catch up via the forwarder.
fn compose_text_from_sources(
    sources: &HashMap<BufferId, Arc<dyn Document>>,
    excerpts: &[Excerpt],
) -> String {
    let mut composed_text = String::new();
    for excerpt in excerpts {
        let Some(source) = sources.get(&excerpt.source) else {
            continue;
        };
        let snap = source.snapshot();
        for row in excerpt.start_line..=excerpt.end_line {
            if let Some(line) = snap.buffer.line(row) {
                composed_text.push_str(&line);
                if !composed_text.ends_with('\n') {
                    composed_text.push('\n');
                }
            }
        }
    }
    composed_text
}

/// M.11 (2026-06-02): build a `DocumentSnapshot` from the
/// composed_doc plus identity metadata. The composed_doc IS the
/// source of truth — this just wraps its current state in the
/// shape the renderer reads.
fn snapshot_from_composed_doc(
    doc: &lattice_core::Document,
    id: DocumentId,
    selections: Arc<SelectionSet>,
) -> DocumentSnapshot {
    DocumentSnapshot {
        id,
        version: doc.version(),
        text_version: doc.text_version(),
        buffer: doc.buffer().clone(),
        path: None,
        dirty: doc.dirty(),
        selections,
    }
}

fn compose_snapshot(
    id: DocumentId,
    sources: &HashMap<BufferId, Arc<dyn Document>>,
    excerpts: &[Excerpt],
    selections: Arc<SelectionSet>,
) -> DocumentSnapshot {
    let mut composed_text = String::new();
    let mut composed_version: u64 = 0;
    let mut composed_text_version: u64 = 0;

    for excerpt in excerpts {
        let Some(source) = sources.get(&excerpt.source) else {
            continue;
        };
        let snap = source.snapshot();
        composed_version = composed_version.saturating_add(snap.version);
        composed_text_version = composed_text_version.saturating_add(snap.text_version);
        for row in excerpt.start_line..=excerpt.end_line {
            if let Some(line) = snap.buffer.line(row) {
                composed_text.push_str(&line);
                if !composed_text.ends_with('\n') {
                    composed_text.push('\n');
                }
            }
        }
    }

    DocumentSnapshot {
        id,
        version: composed_version,
        text_version: composed_text_version,
        buffer: Buffer::from_text(&composed_text),
        path: None,
        dirty: false,
        // K.4.5 (2026-06-02): selections come from
        // `MultibufferState`, preserved across recomposes so
        // excerpt mutations (append / replace / clip) don't
        // clobber the user's Visual selection. Updated via
        // `set_selections` (Document trait).
        selections,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use lattice_core::Document as CoreDocument;
    use lattice_grammar::CommandRegistry;
    use lattice_runtime::spawn_document;

    fn empty_registry() -> lattice_grammar::CommandRegistryHandle {
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()))
    }

    fn make_sources(texts: &[&str]) -> (HashMap<BufferId, Arc<dyn Document>>, Vec<BufferId>) {
        let mut map: HashMap<BufferId, Arc<dyn Document>> = HashMap::new();
        let mut ids = Vec::new();
        for text in texts {
            let id = BufferId::next();
            let handle = spawn_document(id, CoreDocument::from_text(*text), empty_registry());
            map.insert(id, Arc::new(handle));
            ids.push(id);
        }
        (map, ids)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn single_source_single_excerpt_composes() {
        let (sources, ids) = make_sources(&["alpha\nbeta\ngamma\ndelta\nepsilon\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 1, 3)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        let snap = mb.snapshot();
        assert_eq!(snap.buffer.as_string(), "beta\ngamma\ndelta\n");
        assert!(!snap.dirty);
        assert!(snap.path.is_none());
        assert_eq!(snap.selections.all().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn multi_source_multi_excerpt_composes_in_order() {
        let (sources, ids) = make_sources(&["a1\na2\na3\n", "b1\nb2\nb3\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 0, 1), Excerpt::new(ids[1], 2, 2)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        let snap = mb.snapshot();
        assert_eq!(snap.buffer.as_string(), "a1\na2\nb3\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn save_is_readonly_for_a_pathless_view() {
        // A path-less in-memory source has nothing to persist on
        // disk, so `:w` on such a view is `ReadOnly`. Real
        // file-backed sources DO save now — see
        // `save_persists_view_edits_to_the_source_file`.
        let (sources, ids) = make_sources(&["x"]);
        let excerpts = vec![Excerpt::new(ids[0], 0, 0)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        assert!(matches!(mb.save().await, Err(RuntimeError::ReadOnly)));
    }

    /// 2026-06-10: editing a multibuffer view and calling `save()`
    /// flushes the source-forwarder and persists the edit to the
    /// source FILE on disk — the generic `:w`-saves-sources path that
    /// narrow + project-search both rely on. The flush is load-
    /// bearing: without it `save()` would race the async forwarder
    /// and write the pre-edit source.
    #[tokio::test(flavor = "multi_thread")]
    async fn save_persists_view_edits_to_the_source_file() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("lattice-mb-save-{unique}.txt"));
        std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();

        let id = BufferId::next();
        let doc = lattice_core::DocumentBuilder::default()
            .with_text("alpha\nbeta\ngamma\n")
            .with_path(path.clone())
            .build();
        let handle = spawn_document(id, doc, empty_registry());
        let source: Arc<dyn Document> = Arc::new(handle);
        let mut sources: HashMap<BufferId, Arc<dyn Document>> = HashMap::new();
        sources.insert(id, source);
        let excerpts = vec![Excerpt::new(id, 0, 2)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        // Insert "X" at the start of row 1 in the composed view.
        mb.apply_edit(Edit::insert(Position::new(1, 0), "X"))
            .await
            .expect("edit applies");

        // save() flushes the forwarder (so the source has the edit)
        // then writes the source file back to disk.
        let saved = mb.save().await.expect("save ok");
        assert_eq!(saved, path);

        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, "alpha\nXbeta\ngamma\n");
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_selections_stores_composed_selections_post_k_4_5() {
        // K.4.5 (2026-06-02): selections are view-owned in
        // composed coordinate space. set_selections now
        // stores the SelectionSet on `MultibufferState` and
        // republishes the snapshot, so
        // `Editor::visual_selection_range` reading
        // `self.document.selections().primary()` sees the
        // updated anchor / head — Visual-mode highlights
        // paint uniformly across BufferKinds.
        use lattice_protocol::position::Position;
        use lattice_protocol::selection::{Selection, VisualMode};

        let (sources, ids) = make_sources(&["alpha\nbeta\ngamma\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 0, 2)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        // Initial snapshot: default empty selection set.
        let initial = mb.snapshot();
        assert_eq!(initial.selections.all().len(), 1);
        assert_eq!(initial.selections.primary().anchor, Position::new(0, 0));
        assert_eq!(initial.selections.primary().head, Position::new(0, 0));

        // Set a Visual-mode selection spanning the composed view.
        let sel = Selection {
            anchor: Position::new(0, 0),
            head: Position::new(1, 3),
            visual: Some(VisualMode::Charwise),
        };
        let set = SelectionSet::single(sel);
        mb.set_selections(set.clone()).await.expect("ok");

        // Snapshot now reflects the new selection.
        let after = mb.snapshot();
        assert_eq!(after.selections.primary().anchor, Position::new(0, 0));
        assert_eq!(after.selections.primary().head, Position::new(1, 3));
        assert_eq!(
            after.selections.primary().visual,
            Some(VisualMode::Charwise)
        );

        // Recompose preserves the selection (excerpt-mutation
        // paths read state.selections through compose_snapshot).
        mb.recompose();
        let recomposed = mb.snapshot();
        assert_eq!(
            recomposed.selections.primary().head,
            Position::new(1, 3),
            "recompose must preserve composed-coordinate selections"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m3_insert_translates_and_forwards_to_source() {
        // M.11 (2026-06-02): under the local-rope architecture
        // the composed snapshot reflects the edit IMMEDIATELY
        // (synchronous local mutation). The source rope catches
        // up async via the source-forwarder task.
        let (sources, ids) = make_sources(&["alpha\nbeta\ngamma\n"]);
        let source_handle = sources.get(&ids[0]).expect("source present").clone();
        let excerpts = vec![Excerpt::new(ids[0], 0, 2)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        let applied = mb
            .apply_edit(Edit::insert(Position::new(1, 0), "X-"))
            .await
            .expect("insert should land locally");
        assert_eq!(applied.inserted_text, "X-");
        // Composed snapshot reflects the edit synchronously.
        assert_eq!(mb.snapshot().buffer.as_string(), "alpha\nX-beta\ngamma\n");

        // Source catches up async via the forwarder task running
        // on shared_runtime (cross-runtime from the test's
        // multi_thread runtime). Poll with sleep.
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            if source_handle.text() == "alpha\nX-beta\ngamma\n" {
                return;
            }
        }
        panic!(
            "source did not converge to multibuffer edit; got: {:?}",
            source_handle.text()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m3_insert_translates_when_excerpt_starts_off_zero() {
        // M.11: composed snapshot reflects edit synchronously.
        let (sources, ids) = make_sources(&["zero\none\ntwo\nthree\nfour\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 2, 4)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        assert_eq!(mb.snapshot().buffer.as_string(), "two\nthree\nfour\n");

        mb.apply_edit(Edit::insert(Position::new(0, 0), "Z "))
            .await
            .expect("insert should land locally");
        assert_eq!(mb.snapshot().buffer.as_string(), "Z two\nthree\nfour\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m3_delete_within_excerpt_translates() {
        // M.11: composed snapshot reflects edit synchronously.
        let (sources, ids) = make_sources(&["alpha\nbeta\ngamma\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 0, 2)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        use lattice_protocol::position::Range;
        let _ = mb
            .apply_edit(Edit::delete(Range::new(
                Position::new(1, 0),
                Position::new(2, 0),
            )))
            .await
            .expect("delete should land locally");
        assert_eq!(mb.snapshot().buffer.as_string(), "alpha\ngamma\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m3_out_of_range_edit_returns_read_only() {
        // M.11: the local composed_doc is a regular `Document`,
        // so its `apply_edit` rejects out-of-range edits via
        // `CoreError`. Any Err variant is acceptable — the
        // contract is "out-of-range fails, doesn't panic."
        let (sources, ids) = make_sources(&["a\nb\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 0, 1)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        assert!(
            mb.apply_edit(Edit::insert(Position::new(50, 0), "x"))
                .await
                .is_err(),
            "out-of-range edit must fail (any Err variant)"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m3_boundary_clip_drops_cross_excerpt_tail() {
        // Two excerpts from two different sources.
        let (mut sources, ids) = make_sources(&["AA\nBB\nCC\n", "11\n22\n33\n"]);
        // sources contains both; ids[0] = A-source, ids[1] = B-source.
        let excerpts = vec![
            // composed rows 0..=2 — A
            Excerpt::new(ids[0], 0, 2),
            // composed rows 3..=5 — B
            Excerpt::new(ids[1], 0, 2),
        ];
        // Snapshot the original B-source text for the post-edit assertion.
        let b_handle = sources.remove(&ids[1]).expect("B source present");
        sources.insert(ids[1], b_handle.clone());
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        let original_b_text = b_handle.text();

        // Cross-excerpt delete: range (0,0)..(5,0) — spans into B.
        use lattice_protocol::position::Range;
        let _ = mb
            .apply_edit(Edit::delete(Range::new(
                Position::new(0, 0),
                Position::new(5, 0),
            )))
            .await;

        // A was edited (boundary-clipped to A's last row).
        // B was NOT edited (boundary clip dropped the tail).
        assert_eq!(
            b_handle.text(),
            original_b_text,
            "B source must be untouched"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m3_apply_edit_batch_serialises_inserts() {
        let (sources, ids) = make_sources(&["alpha\nbeta\ngamma\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 0, 2)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        // Two inserts in row order. Batch dispatches them
        // sequentially; second insert sees the buffer state
        // after the first.
        let edits = vec![
            Edit::insert(Position::new(0, 0), "<"),
            Edit::insert(Position::new(2, 5), ">"),
        ];
        let results = mb.apply_edit_batch(edits).await.expect("batch ok");
        assert_eq!(results.len(), 2);
        // M.11: composed snapshot reflects edits synchronously.
        // After "<" at (0,0): "<alpha\nbeta\ngamma\n"
        // After ">" at composed (2,5) = source (2,5): "<alpha\nbeta\ngamma>\n"
        assert_eq!(mb.snapshot().buffer.as_string(), "<alpha\nbeta\ngamma>\n");
    }

    /// 2026-06-02 cursor-jump regression: an excerpt that
    /// covers SOURCE rows 5..=7 maps to COMPOSED rows 0..=2.
    /// An insert at composed (0, 5) hits source (5, 5). The
    /// host's insert-mode path
    /// (`lattice-host::dispatch::do_insert_str_blocking`)
    /// reads `applied.inserted_range.end.line` and sets the
    /// cursor — if `apply_edit` returned the source's
    /// inserted_range.end (line 5) instead of the composed
    /// equivalent (line 0), the cursor would jump to line 5
    /// of the composed view, which renders the wrong text and
    /// breaks every subsequent insert. Verify the translation
    /// happens.
    #[tokio::test(flavor = "multi_thread")]
    async fn m3_apply_edit_returns_composed_coords() {
        let (sources, ids) = make_sources(&["a\nb\nc\nd\ne\nf\ng\nh\n"]);
        // Excerpt covers source rows 5..=7 → composed rows
        // 0..=2.
        let excerpts = vec![Excerpt::new(ids[0], 5, 7)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        // Insert "X" at composed (0, 0). In source coords
        // that's (5, 0). The host's cursor advance reads
        // `applied.inserted_range.end`; pre-fix that returned
        // `Position { line: 5, byte: 1 }` (source coords),
        // jumping the cursor to composed row 5 — past the
        // multibuffer's three composed rows.
        let applied = mb
            .apply_edit(Edit::insert(Position::new(0, 0), "X"))
            .await
            .expect("edit ok");

        assert_eq!(
            applied.inserted_range.start,
            Position::new(0, 0),
            "start must be composed (0,0), not source (5,0)"
        );
        assert_eq!(
            applied.inserted_range.end,
            Position::new(0, 1),
            "end must be composed (0,1), not source (5,1) — \
             this is the cursor-jump bug"
        );
        assert_eq!(applied.original_range.start, Position::new(0, 0));
        assert_eq!(applied.original_range.end, Position::new(0, 0));
        // EditDelta positions also translated.
        assert_eq!(applied.delta.start_position, Position::new(0, 0));
        assert_eq!(applied.delta.new_end_position, Position::new(0, 1));
    }

    /// Same property for `apply_edit_batch` — each result in
    /// the batch must carry composed coords for that edit's
    /// excerpt.
    #[tokio::test(flavor = "multi_thread")]
    async fn m3_apply_edit_batch_returns_composed_coords() {
        // Two excerpts: source rows 5..=5 (composed 0..=0) and
        // source rows 10..=10 (composed 1..=1).
        let (sources, ids) = make_sources(&["0\n1\n2\n3\n4\n5\n6\n7\n8\n9\nA\nB\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 5, 5), Excerpt::new(ids[0], 10, 10)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        let results = mb
            .apply_edit_batch(vec![
                Edit::insert(Position::new(0, 0), "X"),
                Edit::insert(Position::new(1, 0), "Y"),
            ])
            .await
            .expect("batch ok");

        assert_eq!(results.len(), 2);
        // First result: composed row 0 (was source row 5).
        assert_eq!(
            results[0].inserted_range.end,
            Position::new(0, 1),
            "first batch result must be composed (0,1)"
        );
        // Second result: composed row 1 (was source row 10).
        // Note: the second edit's actual source row after the
        // first edit lands is 10 (the first insert was at
        // source col 0 of row 5, only widening that row's
        // bytes — row indices unchanged). Composed row 1.
        assert_eq!(
            results[1].inserted_range.end,
            Position::new(1, 1),
            "second batch result must be composed (1,1)"
        );
    }

    /// 2026-06-02 stale-snapshot regression: typing a character
    /// in a multibuffer must update the composed snapshot the
    /// renderer reads on the next frame. Pre-fix the host's
    /// `publish_document_changed` fired under the multibuffer's
    /// id (not the source's), so the M.4 forwarder ignored it
    /// and the composed snapshot stayed pre-edit. Cursor would
    /// advance correctly (translate_applied_to_composed) but the
    /// rendered text never changed. Verify the snapshot updates
    /// synchronously after apply_edit returns.
    #[tokio::test(flavor = "multi_thread")]
    async fn m3_apply_edit_updates_composed_snapshot_without_forwarder() {
        let (sources, ids) = make_sources(&["hello\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 0, 0)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        // No attach_event_subscriptions call — production path
        // does it in create_multibuffer_view, but the forwarder
        // wouldn't fire here anyway because no event bus is
        // wired. Verify apply_edit's own recompose lands.
        assert_eq!(mb.snapshot().buffer.as_string(), "hello\n");

        let _ = mb
            .apply_edit(Edit::insert(Position::new(0, 5), "!"))
            .await
            .expect("edit ok");

        assert_eq!(
            mb.snapshot().buffer.as_string(),
            "hello!\n",
            "composed snapshot must reflect the new content; \
             the renderer reads this on the next frame"
        );
    }

    /// M.10.2 (2026-06-03): cursor at composed (0, 5) on an
    /// excerpt covering source rows 5..=7 maps to source
    /// (5, 5). Byte column preserved (each composed row is a
    /// verbatim copy of its source line).
    #[tokio::test(flavor = "multi_thread")]
    async fn m10_2_translate_composed_to_source_single_excerpt() {
        let (sources, ids) = make_sources(&["0\n1\n2\n3\n4\n5\n6\n7\n8\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 5, 7)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        let target = mb
            .translate_composed_to_source(Position::new(0, 5))
            .expect("composed (0,5) must translate");
        assert_eq!(target.0, ids[0], "source buffer id");
        assert_eq!(target.1, Position::new(5, 5), "source position");

        // Composed (2, 0) → source (5 + 2, 0) = (7, 0).
        let target = mb
            .translate_composed_to_source(Position::new(2, 0))
            .expect("composed (2,0) must translate");
        assert_eq!(target.1, Position::new(7, 0));
    }

    // ---- N.1.5: ComposedScopeResolver (composed↔source text objects) ----

    use lattice_grammar::ScopeResolver as _;

    fn rust_snapshot(src: &str) -> Arc<SyntaxSnapshot> {
        let lr = LangRegistry::standard().unwrap();
        let mut syntax = Syntax::for_language_with_registry(Lang::Rust, lr)
            .unwrap()
            .unwrap();
        syntax.parse(src);
        SyntaxHandle::seeded(syntax).snapshot()
    }

    #[test]
    fn composed_resolver_maps_function_outer_to_source() {
        // Source: a `target` fn on rows 1..=3, narrowed into a one-excerpt
        // view (composed rows 0..=2 == source rows 1..=3).
        let src = "fn keep_a() {}\nfn target() {\n    let x = 1;\n}\nfn keep_b() {}\n";
        let snap = rust_snapshot(src);
        let resolver = ComposedScopeResolver {
            excerpts: vec![ComposedExcerptSpan {
                source: BufferId(1),
                start_line: 1,
                line_count: 3,
                composed_offset: 0,
            }],
            snapshots: HashMap::from([(BufferId(1), snap)]),
        };
        // Cursor at composed (1,4) == source (2,4), inside `target`'s body.
        // `af` resolves the whole function (source rows 1..=3) and maps
        // back to composed rows 0..=2.
        let r = resolver.scope_at(1, 4, "function.outer");
        assert_eq!(
            r,
            Some(lattice_protocol::position::Range::new(
                lattice_protocol::position::Position::new(0, 0),
                lattice_protocol::position::Position::new(2, 1),
            )),
            "af inside a narrow view resolves the source function, mapped to composed rows"
        );
    }

    #[test]
    fn composed_resolver_clamps_scope_to_excerpt() {
        // Excerpt covers only source row 2 (the body line). `af` resolves
        // the function (source 1..=3) but the result is CLAMPED to the
        // single visible row — never out-of-bounds composed rows.
        let src = "fn keep_a() {}\nfn target() {\n    let x = 1;\n}\nfn keep_b() {}\n";
        let snap = rust_snapshot(src);
        let resolver = ComposedScopeResolver {
            excerpts: vec![ComposedExcerptSpan {
                source: BufferId(1),
                start_line: 2,
                line_count: 1,
                composed_offset: 0,
            }],
            snapshots: HashMap::from([(BufferId(1), snap)]),
        };
        let r = resolver.scope_at(0, 4, "function.outer");
        assert_eq!(
            r,
            Some(lattice_protocol::position::Range::new(
                lattice_protocol::position::Position::new(0, 0),
                lattice_protocol::position::Position::new(0, 0),
            )),
            "a scope extending past the excerpt is clipped to the visible rows"
        );
    }

    #[test]
    fn composed_resolver_applies_composed_offset_for_second_excerpt() {
        // Two excerpts from the same source: [0,0] then [1,3]. The second
        // sits at composed_offset 1, so resolving inside it must add the
        // offset back when mapping the source range to composed rows.
        let src = "fn keep_a() {}\nfn target() {\n    let x = 1;\n}\nfn keep_b() {}\n";
        let snap = rust_snapshot(src);
        let resolver = ComposedScopeResolver {
            excerpts: vec![
                ComposedExcerptSpan {
                    source: BufferId(1),
                    start_line: 0,
                    line_count: 1,
                    composed_offset: 0,
                },
                ComposedExcerptSpan {
                    source: BufferId(1),
                    start_line: 1,
                    line_count: 3,
                    composed_offset: 1,
                },
            ],
            snapshots: HashMap::from([(BufferId(1), snap)]),
        };
        // Composed row 2 lives in the second excerpt → source row 2.
        let r = resolver.scope_at(2, 4, "function.outer");
        assert_eq!(
            r,
            Some(lattice_protocol::position::Range::new(
                lattice_protocol::position::Position::new(1, 0),
                lattice_protocol::position::Position::new(3, 1),
            )),
            "the second excerpt's composed_offset (1) is added to the mapped rows"
        );
    }

    /// M.10.2 (2026-06-03): out-of-range composed cursor
    /// returns None (no excerpt covers that row).
    #[tokio::test(flavor = "multi_thread")]
    async fn m10_2_translate_composed_to_source_out_of_range() {
        let (sources, ids) = make_sources(&["a\nb\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 0, 1)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        // Excerpt covers composed rows 0..=1; row 5 is past.
        assert!(
            mb.translate_composed_to_source(Position::new(5, 0))
                .is_none()
        );
    }

    /// M.10.2 (2026-06-03): multi-excerpt walk — cursor on the
    /// second excerpt maps to its source.
    #[tokio::test(flavor = "multi_thread")]
    async fn m10_2_translate_composed_to_source_multi_excerpt() {
        let (sources, ids) = make_sources(&["AA\nBB\n", "11\n22\n"]);
        let excerpts = vec![
            Excerpt::new(ids[0], 0, 1), // composed 0..=1
            Excerpt::new(ids[1], 0, 1), // composed 2..=3
        ];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        // Composed (0, 1) → source A (0, 1).
        let t = mb
            .translate_composed_to_source(Position::new(0, 1))
            .unwrap();
        assert_eq!(t.0, ids[0]);
        assert_eq!(t.1, Position::new(0, 1));

        // Composed (3, 1) → source B (1, 1).
        let t = mb
            .translate_composed_to_source(Position::new(3, 1))
            .unwrap();
        assert_eq!(t.0, ids[1]);
        assert_eq!(t.1, Position::new(1, 1));
    }

    /// M.11 (2026-06-02): the search provider's exact flow —
    /// construct an empty multibuffer, then stream excerpts via
    /// `append_excerpts`, then type. Pre-fix the composed_doc
    /// stayed empty (only state.excerpts + the published
    /// snapshot reflected the stream), so user inserts hit an
    /// empty rope and silently no-op'd through
    /// do_insert_text's `let Ok(applied) = … else { return };`.
    /// This was the root cause of "typing does nothing" the
    /// user reported after M.11 first landed.
    #[tokio::test(flavor = "multi_thread")]
    async fn m11_streamed_excerpts_keep_composed_doc_in_sync_for_insert() {
        // Empty construction — exactly what
        // `project_search → create_multibuffer_view` does for
        // an in-progress scan.
        let (sources, ids) = make_sources(&["zero\none\ntwo\nthree\nfour\n"]);
        let mb = MultibufferDocumentHandle::new(sources, vec![], empty_registry()).unwrap();
        assert_eq!(mb.snapshot().buffer.as_string(), "");

        // Stream an excerpt (the search hit).
        mb.append_excerpts(vec![Excerpt::new(ids[0], 2, 2)]);
        assert_eq!(
            mb.snapshot().buffer.as_string(),
            "two\n",
            "snapshot must reflect the streamed excerpt"
        );

        // Now type — insert at the end of the line.
        let applied = mb
            .apply_edit(Edit::insert(Position::new(0, 3), "X"))
            .await
            .expect("post-stream insert must succeed (pre-fix this Err'd silently)");
        assert_eq!(applied.inserted_range.end, Position::new(0, 4));
        assert_eq!(
            mb.snapshot().buffer.as_string(),
            "twoX\n",
            "composed snapshot must reflect the insert; \
             pre-fix the snapshot stayed at \"two\\n\" because \
             composed_doc was empty and apply_edit no-op'd"
        );
    }

    /// 2026-06-02: simulate the user's insert-mode flow — two
    /// consecutive single-char apply_edit calls. After each, the
    /// composed snapshot must reflect the cumulative text. The
    /// user reported the first char landed visibly but the second
    /// didn't ("switched to how it was before, every character in
    /// insert mode just moves the cursor along").
    #[tokio::test(flavor = "multi_thread")]
    async fn m3_consecutive_inserts_accumulate_in_composed_snapshot() {
        // Source with 8 lines; excerpt covers source row 5 only.
        let (sources, ids) = make_sources(&["0\n1\n2\n3\n4\nfive\n6\n7\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 5, 5)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        assert_eq!(mb.snapshot().buffer.as_string(), "five\n");

        // First insert at end-of-line (composed (0, 4)) — vim
        // `a` at end-of-line lands here. Append 'x'.
        let r1 = mb
            .apply_edit(Edit::insert(Position::new(0, 4), "x"))
            .await
            .expect("first edit ok");
        assert_eq!(
            r1.inserted_range.end,
            Position::new(0, 5),
            "first cursor must be composed (0,5)"
        );
        assert_eq!(
            mb.snapshot().buffer.as_string(),
            "fivex\n",
            "first char must land in composed snapshot"
        );

        // Second insert at composed (0, 5) — append 'y'.
        let r2 = mb
            .apply_edit(Edit::insert(Position::new(0, 5), "y"))
            .await
            .expect("second edit ok");
        assert_eq!(
            r2.inserted_range.end,
            Position::new(0, 6),
            "second cursor must be composed (0,6)"
        );
        assert_eq!(
            mb.snapshot().buffer.as_string(),
            "fivexy\n",
            "second char must accumulate; not revert to original"
        );

        // Third insert at composed (0, 6) — append 'z'.
        let r3 = mb
            .apply_edit(Edit::insert(Position::new(0, 6), "z"))
            .await
            .expect("third edit ok");
        assert_eq!(r3.inserted_range.end, Position::new(0, 7));
        assert_eq!(mb.snapshot().buffer.as_string(), "fivexyz\n");
    }

    // ─────────────────────────────────────────────────────────────
    // M.4 tests
    // ─────────────────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn m4_headerline_starts_idle_and_can_be_set() {
        let (sources, ids) = make_sources(&["x\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 0, 0)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        assert!(matches!(*mb.headerline(), HeaderlineStatus::Idle));

        mb.set_headerline(HeaderlineStatus::InProgress {
            label: "Searching".into(),
            count: Some(42),
            emphasis: None,
        });
        match &*mb.headerline() {
            HeaderlineStatus::InProgress { label, count, .. } => {
                assert_eq!(label, "Searching");
                assert_eq!(*count, Some(42));
            }
            other => panic!("expected InProgress, got {other:?}"),
        }

        mb.set_headerline(HeaderlineStatus::Complete {
            summary: "87 hits".into(),
            emphasis: None,
        });
        match &*mb.headerline() {
            HeaderlineStatus::Complete { summary, .. } => assert_eq!(summary, "87 hits"),
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m4_set_headerline_publishes_changed_event_when_attached() {
        let bus = Arc::new(lattice_runtime::EventBus::new());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<MultibufferHeaderlineChanged>();
        bus.subscribe_typed::<MultibufferHeaderlineChanged>(tx);

        let (sources, ids) = make_sources(&["y\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 0, 0)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        let view_id = mb.buffer_id();
        mb.attach_event_subscriptions(&bus);

        mb.set_headerline(HeaderlineStatus::Complete {
            summary: "done".into(),
            emphasis: None,
        });

        let evt = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
            .await
            .expect("event should arrive")
            .expect("channel open");
        assert_eq!(evt.view, view_id);
        assert!(matches!(evt.status, HeaderlineStatus::Complete { .. }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m4_source_change_auto_recomposes_view() {
        let bus = Arc::new(lattice_runtime::EventBus::new());
        let (sources, ids) = make_sources(&["alpha\nbeta\n"]);
        let source_handle = sources.get(&ids[0]).unwrap().clone();
        let excerpts = vec![Excerpt::new(ids[0], 0, 1)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        mb.attach_event_subscriptions(&bus);
        assert_eq!(mb.snapshot().buffer.as_string(), "alpha\nbeta\n");

        // Source edit publishes DocumentChanged on the bus the
        // multibuffer subscribed to. After a brief yield, the
        // forwarder task should have recomposed.
        // The mock setup above doesn't wire the source handle to
        // PUBLISH on the bus — `spawn_document` publishes events
        // only when given a bus. So this test verifies the
        // SUBSCRIBE path: directly publish a DocumentChanged
        // event with the source's DocumentId and confirm the
        // multibuffer recomposes.
        source_handle
            .apply_edit(Edit::insert(Position::new(0, 0), "<"))
            .await
            .unwrap();
        // Simulate the source's DocumentChanged publish.
        bus.publish(lattice_protocol::Event::DocumentChanged {
            id: source_handle.id(),
            path: None,
            version: source_handle.version(),
            edits: Vec::new(),
        });

        // Wait for the spawned forwarder to process. Longer
        // budget than yield_now because tokio's multi-thread
        // runtime may park the task briefly.
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            if mb.snapshot().buffer.as_string() != "alpha\nbeta\n" {
                break;
            }
        }
        assert_eq!(mb.snapshot().buffer.as_string(), "<alpha\nbeta\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m4_source_close_publishes_typed_event_and_prunes() {
        let bus = Arc::new(lattice_runtime::EventBus::new());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<MultibufferSourceClosed>();
        bus.subscribe_typed::<MultibufferSourceClosed>(tx);

        let (sources, ids) = make_sources(&["a\n", "b\n"]);
        let source_a_handle = sources.get(&ids[0]).unwrap().clone();
        let excerpts = vec![Excerpt::new(ids[0], 0, 0), Excerpt::new(ids[1], 0, 0)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        let view_id = mb.buffer_id();
        mb.attach_event_subscriptions(&bus);
        assert_eq!(mb.source_buffer_ids().len(), 2);

        // Publish DocumentClosed for source A.
        bus.publish(lattice_protocol::Event::DocumentClosed {
            id: source_a_handle.id(),
        });

        let evt = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
            .await
            .expect("event should arrive")
            .expect("channel open");
        assert_eq!(evt.view, view_id);
        assert_eq!(evt.source, ids[0]);

        // Source A pruned from the map.
        for _ in 0..10 {
            tokio::task::yield_now().await;
            if mb.source_buffer_ids().len() == 1 {
                break;
            }
        }
        assert_eq!(mb.source_buffer_ids(), vec![ids[1]]);
    }

    // ─────────────────────────────────────────────────────────────
    // M.4.1 tests — anchor sliding
    // ─────────────────────────────────────────────────────────────

    fn applied_edit(
        old_start: (u32, u32),
        old_end: (u32, u32),
        new_end: (u32, u32),
        replaced: &str,
        inserted: &str,
    ) -> lattice_protocol::event::AppliedEdit {
        use lattice_protocol::position::Range;
        lattice_protocol::event::AppliedEdit {
            original_range: Range::new(
                Position::new(old_start.0, old_start.1),
                Position::new(old_end.0, old_end.1),
            ),
            inserted_range: Range::new(
                Position::new(old_start.0, old_start.1),
                Position::new(new_end.0, new_end.1),
            ),
            replaced_text: replaced.into(),
            inserted_text: inserted.into(),
        }
    }

    async fn pump_forwarder() {
        for _ in 0..30 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m41_insert_above_excerpt_slides_down() {
        let bus = Arc::new(lattice_runtime::EventBus::new());
        let (sources, ids) = make_sources(&["aa\nbb\ncc\ndd\nee\n"]);
        let src = sources.get(&ids[0]).unwrap().clone();
        // Excerpt covers source rows 2-3 (cc, dd).
        let excerpts = vec![Excerpt::new(ids[0], 2, 3)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        mb.attach_event_subscriptions(&bus);

        // Synthesise: edit at line 0 byte 0 → line 0 byte 0
        // inserts 2 lines of content (row_delta = +2).
        // original_range end = (0, 0); inserted_range end = (2, 0).
        let edit = applied_edit((0, 0), (0, 0), (2, 0), "", "X\nY\n");
        bus.publish(lattice_protocol::Event::DocumentChanged {
            id: src.id(),
            path: None,
            version: 1,
            edits: vec![edit],
        });

        pump_forwarder().await;
        let excerpts_after = mb.excerpts();
        assert_eq!(
            excerpts_after[0].start_line, 4,
            "excerpt should slide to row 4"
        );
        assert_eq!(excerpts_after[0].end_line, 5);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m41_delete_above_excerpt_slides_up() {
        let bus = Arc::new(lattice_runtime::EventBus::new());
        let (sources, ids) = make_sources(&["aa\nbb\ncc\ndd\nee\nff\n"]);
        let src = sources.get(&ids[0]).unwrap().clone();
        // Excerpt covers rows 4-5 (ee, ff).
        let excerpts = vec![Excerpt::new(ids[0], 4, 5)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        mb.attach_event_subscriptions(&bus);

        // Delete rows 0-1 (aa\nbb\n). original_range end = (2, 0);
        // inserted_range end = (0, 0). row_delta = -2.
        let edit = applied_edit((0, 0), (2, 0), (0, 0), "aa\nbb\n", "");
        bus.publish(lattice_protocol::Event::DocumentChanged {
            id: src.id(),
            path: None,
            version: 1,
            edits: vec![edit],
        });

        pump_forwarder().await;
        let excerpts_after = mb.excerpts();
        assert_eq!(
            excerpts_after[0].start_line, 2,
            "excerpt should slide up to row 2"
        );
        assert_eq!(excerpts_after[0].end_line, 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m41_edit_below_excerpt_does_not_slide() {
        let bus = Arc::new(lattice_runtime::EventBus::new());
        let (sources, ids) = make_sources(&["aa\nbb\ncc\ndd\nee\n"]);
        let src = sources.get(&ids[0]).unwrap().clone();
        let excerpts = vec![Excerpt::new(ids[0], 0, 1)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        mb.attach_event_subscriptions(&bus);

        // Edit at row 3: original_range end = (3, 0).
        // excerpt.start_line = 0; condition is `old_end < start_line`
        // → `3 < 0` false → no slide.
        let edit = applied_edit((3, 0), (3, 0), (4, 0), "", "X\n");
        bus.publish(lattice_protocol::Event::DocumentChanged {
            id: src.id(),
            path: None,
            version: 1,
            edits: vec![edit],
        });

        pump_forwarder().await;
        let excerpts_after = mb.excerpts();
        assert_eq!(excerpts_after[0].start_line, 0);
        assert_eq!(excerpts_after[0].end_line, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m41_overlapping_edit_does_not_slide_excerpt() {
        let bus = Arc::new(lattice_runtime::EventBus::new());
        let (sources, ids) = make_sources(&["aa\nbb\ncc\ndd\n"]);
        let src = sources.get(&ids[0]).unwrap().clone();
        // Excerpt covers rows 1-2.
        let excerpts = vec![Excerpt::new(ids[0], 1, 2)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        mb.attach_event_subscriptions(&bus);

        // Edit that ends inside the excerpt (rows 0..=1):
        // original_range end = (2, 0). `2 < 1` false → no slide.
        let edit = applied_edit((0, 0), (2, 0), (1, 0), "aa\nbb\n", "X\n");
        bus.publish(lattice_protocol::Event::DocumentChanged {
            id: src.id(),
            path: None,
            version: 1,
            edits: vec![edit],
        });

        pump_forwarder().await;
        // Conservative slide: excerpt stays put. Recompose
        // picks up new content for the now-overlapped rows.
        let excerpts_after = mb.excerpts();
        assert_eq!(excerpts_after[0].start_line, 1);
        assert_eq!(excerpts_after[0].end_line, 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m41_other_source_edits_dont_slide_this_source() {
        let bus = Arc::new(lattice_runtime::EventBus::new());
        let (sources, ids) = make_sources(&["aa\nbb\n", "11\n22\n"]);
        let src_b = sources.get(&ids[1]).unwrap().clone();
        // Excerpt of source A at rows 0-1; source B has its own.
        let excerpts = vec![Excerpt::new(ids[0], 0, 1)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        mb.attach_event_subscriptions(&bus);

        // Insert 5 rows in source B above row 0. Should NOT
        // slide source A's excerpt.
        let edit = applied_edit((0, 0), (0, 0), (5, 0), "", "x\nx\nx\nx\nx\n");
        bus.publish(lattice_protocol::Event::DocumentChanged {
            id: src_b.id(),
            path: None,
            version: 1,
            edits: vec![edit],
        });

        pump_forwarder().await;
        let excerpts_after = mb.excerpts();
        assert_eq!(excerpts_after[0].start_line, 0);
        assert_eq!(excerpts_after[0].end_line, 1);
    }

    // ─────────────────────────────────────────────────────────────
    // M.5 tests — expand-context
    // ─────────────────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn m5_expand_grows_symmetrically() {
        // Source has 10 rows (line 0..9); excerpt covers rows 4-5.
        let mut text = String::new();
        for i in 0..10 {
            text.push_str(&format!("L{i}\n"));
        }
        let (sources, ids) = make_sources(&[&text]);
        let excerpts = vec![Excerpt::new(ids[0], 4, 5)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        // Cursor on composed row 0 (= source row 4). Expand by 4
        // rows: 2 above + 2 below → new range 2..7.
        mb.expand_excerpt_at(0, 4);
        let excerpts = mb.excerpts();
        assert_eq!(excerpts[0].start_line, 2);
        assert_eq!(excerpts[0].end_line, 7);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m5_expand_clips_to_source_start() {
        // Excerpt at rows 1-2; expand by 6 should clip top to 0.
        let mut text = String::new();
        for i in 0..10 {
            text.push_str(&format!("L{i}\n"));
        }
        let (sources, ids) = make_sources(&[&text]);
        let excerpts = vec![Excerpt::new(ids[0], 1, 2)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        // delta=6 → above=3, below=3. start = 1-3 = -2 → clipped to 0.
        // end = 2+3 = 5.
        mb.expand_excerpt_at(0, 6);
        let excerpts = mb.excerpts();
        assert_eq!(excerpts[0].start_line, 0);
        assert_eq!(excerpts[0].end_line, 5);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m5_expand_clips_to_source_end() {
        // Source text "L0\n...L9\n" has 10 content lines AND a
        // trailing empty line after the final `\n` — `Buffer::line_count`
        // returns 11. Clip target is the last row index = 10.
        let mut text = String::new();
        for i in 0..10 {
            text.push_str(&format!("L{i}\n"));
        }
        let (sources, ids) = make_sources(&[&text]);
        let excerpts = vec![Excerpt::new(ids[0], 7, 8)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        // delta=6 → above=3, below=3. start = 7-3 = 4.
        // end = 8+3 = 11 → clipped to source_line_count - 1 = 10.
        mb.expand_excerpt_at(0, 6);
        let excerpts = mb.excerpts();
        assert_eq!(excerpts[0].start_line, 4);
        assert_eq!(excerpts[0].end_line, 10);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m5_contract_shrinks_symmetrically() {
        let mut text = String::new();
        for i in 0..20 {
            text.push_str(&format!("L{i}\n"));
        }
        let (sources, ids) = make_sources(&[&text]);
        // Excerpt at rows 5-15 (11 rows).
        let excerpts = vec![Excerpt::new(ids[0], 5, 15)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        // delta=-4 → above=-2, below=-2. start = 5+2 = 7. end = 15-2 = 13.
        mb.expand_excerpt_at(0, -4);
        let excerpts = mb.excerpts();
        assert_eq!(excerpts[0].start_line, 7);
        assert_eq!(excerpts[0].end_line, 13);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m5_contract_below_one_row_is_noop() {
        let (sources, ids) = make_sources(&["a\nb\n"]);
        // Excerpt at rows 0-0 (single row).
        let excerpts = vec![Excerpt::new(ids[0], 0, 0)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        // Contract by 4 → new start = 2, new end = -2 (clipped to 0).
        // 0 > 2 inverted → no-op.
        mb.expand_excerpt_at(0, -4);
        let excerpts = mb.excerpts();
        assert_eq!(excerpts[0].start_line, 0);
        assert_eq!(excerpts[0].end_line, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m5_zero_delta_is_noop() {
        let (sources, ids) = make_sources(&["a\nb\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 0, 1)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        mb.expand_excerpt_at(0, 0);
        let excerpts = mb.excerpts();
        assert_eq!(excerpts[0].start_line, 0);
        assert_eq!(excerpts[0].end_line, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m5_no_excerpt_at_cursor_is_noop() {
        let (sources, ids) = make_sources(&["a\nb\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 0, 0)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        // Cursor at composed row 50 — well past the single excerpt.
        mb.expand_excerpt_at(50, 4);
        let excerpts = mb.excerpts();
        assert_eq!(excerpts[0].start_line, 0);
        assert_eq!(excerpts[0].end_line, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m5_expand_then_recompose_reflects_new_content() {
        let mut text = String::new();
        for i in 0..10 {
            text.push_str(&format!("L{i}\n"));
        }
        let (sources, ids) = make_sources(&[&text]);
        let excerpts = vec![Excerpt::new(ids[0], 4, 5)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        assert_eq!(mb.snapshot().buffer.as_string(), "L4\nL5\n");

        mb.expand_excerpt_at(0, 4);
        // After expand_excerpt_at recomposes, the snapshot
        // should already reflect the new rows.
        assert_eq!(mb.snapshot().buffer.as_string(), "L2\nL3\nL4\nL5\nL6\nL7\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m4_attach_is_idempotent() {
        let bus = Arc::new(lattice_runtime::EventBus::new());
        let (sources, ids) = make_sources(&["x\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 0, 0)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        mb.attach_event_subscriptions(&bus);
        // Second call returns immediately; no second subscription
        // ID is recorded (verifiable via the unique-ID set count
        // staying at 1, but our internal bookkeeping isn't
        // public — instead we verify no panic + behaviour stays
        // correct).
        mb.attach_event_subscriptions(&bus);
        mb.set_headerline(HeaderlineStatus::Complete {
            summary: "x".into(),
            emphasis: None,
        });
    }

    /// M.11 (2026-06-02): undo operates on the LOCAL composed_doc,
    /// not via fan-out to source actors. The composed_doc has its
    /// own undo stack populated by `apply_edit`; `undo()` pops
    /// the most recent entry and applies its inverse. Sources
    /// are NOT reverted (they retain their forwarded edits) —
    /// richer multi-source transaction tracking is a future
    /// slice. Replaces the pre-M.11 `m3_undo_fans_out_to_each_source`
    /// test whose semantics no longer apply.
    #[tokio::test(flavor = "multi_thread")]
    async fn m11_undo_reverses_local_composed_doc_edit() {
        let (sources, ids) = make_sources(&["aaa\nbbb\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 0, 1)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        let pre_text = mb.snapshot().buffer.as_string();
        assert_eq!(pre_text, "aaa\nbbb\n");

        // Apply an edit via the multibuffer (lands in composed_doc).
        mb.apply_edit(Edit::insert(Position::new(0, 3), "X"))
            .await
            .expect("insert ok");
        assert_eq!(mb.snapshot().buffer.as_string(), "aaaX\nbbb\n");

        // Undo reverses on composed_doc — synchronous, no
        // Pending::spawn deadlock (the user-reported freeze on
        // `u` after an insert was the pre-M.11 fan-out path).
        let applied = mb.undo().await.expect("undo ok");
        assert!(
            !applied.is_empty(),
            "undo should return the inverse edits applied to composed_doc"
        );
        assert_eq!(
            mb.snapshot().buffer.as_string(),
            "aaa\nbbb\n",
            "composed_doc must roll back to pre-edit state"
        );

        // Redo replays the insert.
        let _ = mb.redo().await.expect("redo ok");
        assert_eq!(mb.snapshot().buffer.as_string(), "aaaX\nbbb\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_excerpts_is_valid_for_async_providers() {
        // M.2.b.2 (2026-06-01): empty inputs are valid. Async
        // providers open an empty view and stream excerpts in
        // as their scan progresses.
        let mb = MultibufferDocumentHandle::empty(empty_registry());
        assert_eq!(mb.excerpt_count(), 0);
        assert_eq!(mb.snapshot().buffer.as_string(), "");
        let (sources, _ids) = make_sources(&["x"]);
        let mb = MultibufferDocumentHandle::new(sources, Vec::new(), empty_registry()).unwrap();
        assert_eq!(mb.excerpt_count(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn append_excerpts_extends_the_view() {
        let (sources, ids) = make_sources(&["alpha\nbeta\ngamma\n"]);
        let mb =
            MultibufferDocumentHandle::new(sources.clone(), Vec::new(), empty_registry()).unwrap();
        assert_eq!(mb.excerpt_count(), 0);

        mb.append_excerpts(vec![Excerpt::new(ids[0], 0, 0)]);
        assert_eq!(mb.excerpt_count(), 1);
        assert_eq!(mb.snapshot().buffer.as_string(), "alpha\n");

        mb.append_excerpts(vec![Excerpt::new(ids[0], 2, 2)]);
        assert_eq!(mb.excerpt_count(), 2);
        assert_eq!(mb.snapshot().buffer.as_string(), "alpha\ngamma\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn append_excerpts_drops_unknown_source_silently() {
        let (sources, ids) = make_sources(&["alpha\nbeta\n"]);
        let mb = MultibufferDocumentHandle::new(sources, Vec::new(), empty_registry()).unwrap();
        let bogus = BufferId(0xDEAD_BEEF);
        mb.append_excerpts(vec![Excerpt::new(ids[0], 0, 0), Excerpt::new(bogus, 0, 0)]);
        assert_eq!(
            mb.excerpt_count(),
            1,
            "unknown-source excerpt should be silently dropped",
        );
    }

    // MH.B2 (2026-06-19): the correctness gate for the incremental
    // `append_excerpts`. Build one view by streaming the excerpt
    // list in N batches; build a second view with the identical
    // full excerpt list in ONE shot. The composed rope TEXT and
    // the row-translation entries MUST be identical between the
    // two — this proves incremental append == full build, both for
    // the rope (insert-at-end == from_text(old + batch)) and for
    // the translation (concatenation == build-from-all).
    #[tokio::test(flavor = "multi_thread")]
    async fn incremental_append_matches_full_build() {
        // Six sources, each multi-line, so each batch contributes
        // several composed rows.
        let texts = [
            "a1\na2\na3\n",
            "b1\nb2\nb3\n",
            "c1\nc2\nc3\n",
            "d1\nd2\nd3\n",
            "e1\ne2\ne3\n",
            "f1\nf2\nf3\n",
        ];
        let (sources, ids) = make_sources(&texts);

        // The full excerpt list, varied start/end so spans differ.
        let all_excerpts = vec![
            Excerpt::new(ids[0], 0, 1),
            Excerpt::new(ids[1], 1, 2),
            Excerpt::new(ids[2], 0, 2),
            Excerpt::new(ids[3], 2, 2),
            Excerpt::new(ids[4], 0, 0),
            Excerpt::new(ids[5], 1, 2),
        ];

        // INCREMENTAL: empty view, stream in 3 batches of 2.
        let incremental =
            MultibufferDocumentHandle::new(sources.clone(), Vec::new(), empty_registry()).unwrap();
        for batch in all_excerpts.chunks(2) {
            incremental.append_excerpts(batch.to_vec());
        }

        // FULL: same source map + the entire excerpt list at once,
        // via the constructor's full build.
        let full = MultibufferDocumentHandle::new(sources, all_excerpts.clone(), empty_registry())
            .unwrap();

        // (a) composed rope text byte-identical.
        assert_eq!(
            incremental.snapshot().buffer.as_string(),
            full.snapshot().buffer.as_string(),
            "incremental append must produce a byte-identical composed rope",
        );

        // (b) row_translation entries identical (same Vec<RowEntry>),
        // modulo the ExcerptId values — the two views allocate
        // distinct ExcerptIds, so compare structurally on
        // (source_row) ordering within the same shape. We assert
        // the entry COUNT and source_row sequence match; the
        // excerpt_id mapping is per-view by construction.
        let inc_entries = incremental.row_translation().entries.clone();
        let full_entries = full.row_translation().entries.clone();
        assert_eq!(
            inc_entries.len(),
            full_entries.len(),
            "row translation length must match the full build",
        );
        let inc_rows: Vec<u32> = inc_entries
            .iter()
            .map(|RowEntry::Excerpt { source_row, .. }| *source_row)
            .collect();
        let full_rows: Vec<u32> = full_entries
            .iter()
            .map(|RowEntry::Excerpt { source_row, .. }| *source_row)
            .collect();
        assert_eq!(
            inc_rows, full_rows,
            "row translation source_row sequence must match the full build",
        );
    }

    // MH.B2 (2026-06-19): edge case — a batch containing an excerpt
    // whose source was never added is dropped identically by both
    // the incremental and full paths, leaving the composed text +
    // translation in lock-step.
    #[tokio::test(flavor = "multi_thread")]
    async fn incremental_append_skips_unknown_source_like_full_build() {
        let (sources, ids) = make_sources(&["a1\na2\n", "b1\nb2\n"]);
        let bogus = BufferId(0x0BAD_C0DE);

        let valid = vec![Excerpt::new(ids[0], 0, 0), Excerpt::new(ids[1], 1, 1)];

        // INCREMENTAL: stream one valid excerpt, then a batch that
        // mixes a valid + a bogus-source excerpt (bogus dropped).
        let incremental =
            MultibufferDocumentHandle::new(sources.clone(), Vec::new(), empty_registry()).unwrap();
        incremental.append_excerpts(vec![valid[0].clone()]);
        incremental.append_excerpts(vec![Excerpt::new(bogus, 0, 0), valid[1].clone()]);

        // FULL: only the two valid excerpts (the constructor would
        // reject an unknown source, so the full build's coherent
        // input is the valid set — the same set the incremental
        // path retains after dropping the bogus excerpt).
        let full =
            MultibufferDocumentHandle::new(sources, valid.clone(), empty_registry()).unwrap();

        assert_eq!(incremental.excerpt_count(), 2);
        assert_eq!(
            incremental.snapshot().buffer.as_string(),
            full.snapshot().buffer.as_string(),
            "dropping an unknown-source excerpt must leave the rope identical to the full build",
        );
        assert_eq!(
            incremental.row_translation().entries.len(),
            full.row_translation().entries.len(),
        );
    }

    // MH.B2 (2026-06-19): structural pin on the O(batch) guarantee.
    // Appending a 1-excerpt batch (covering R source rows) to an
    // N-excerpt view must grow the row translation by EXACTLY R
    // entries and grow the composed rope by EXACTLY the batch
    // text's byte length — i.e. no full recompute. Together with
    // `incremental_append_matches_full_build` this pins both the
    // correctness AND the incremental nature of `append_excerpts`.
    #[tokio::test(flavor = "multi_thread")]
    async fn append_excerpts_grows_by_exactly_the_batch() {
        let (sources, ids) = make_sources(&["l0\nl1\nl2\nl3\nl4\n"]);
        let mb = MultibufferDocumentHandle::new(sources, Vec::new(), empty_registry()).unwrap();

        // Seed with a 2-row excerpt.
        mb.append_excerpts(vec![Excerpt::new(ids[0], 0, 1)]);
        let rows_before = mb.row_translation().entries.len();
        let bytes_before = mb.snapshot().buffer.byte_len();
        assert_eq!(rows_before, 2);

        // Append a 3-row excerpt (rows 2..=4 → "l2\nl3\nl4\n" = 9 bytes).
        mb.append_excerpts(vec![Excerpt::new(ids[0], 2, 4)]);
        let rows_after = mb.row_translation().entries.len();
        let bytes_after = mb.snapshot().buffer.byte_len();

        assert_eq!(
            rows_after - rows_before,
            3,
            "translation must grow by exactly the batch's source-row count (O(batch))",
        );
        assert_eq!(
            bytes_after - bytes_before,
            "l2\nl3\nl4\n".len() as u64,
            "composed rope must grow by exactly the batch text length (insert-at-end, no recompute)",
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn replace_excerpts_swaps_atomically() {
        let (sources_a, ids_a) = make_sources(&["a-1\na-2\n"]);
        let excerpts_a = vec![Excerpt::new(ids_a[0], 0, 0)];
        let mb = MultibufferDocumentHandle::new(sources_a, excerpts_a, empty_registry()).unwrap();
        assert_eq!(mb.snapshot().buffer.as_string(), "a-1\n");

        let (sources_b, ids_b) = make_sources(&["b-1\nb-2\n"]);
        let excerpts_b = vec![Excerpt::new(ids_b[0], 1, 1)];
        mb.replace_excerpts(sources_b, excerpts_b);
        assert_eq!(mb.snapshot().buffer.as_string(), "b-2\n");
        assert_eq!(mb.excerpt_count(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_source_returns_error() {
        let (sources, _ids) = make_sources(&["x"]);
        let bogus = BufferId(99_999);
        let excerpts = vec![Excerpt::new(bogus, 0, 0)];
        let err = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap_err();
        assert!(matches!(
            err,
            MultibufferError::UnknownSource { source_buffer, .. } if source_buffer == bogus
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dispatches_via_dyn_document() {
        let (sources, ids) = make_sources(&["foo\nbar\nbaz\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 0, 1)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        let dyn_doc: Arc<dyn Document> = Arc::new(mb);
        assert_eq!(dyn_doc.text(), "foo\nbar\n");
        assert!(!dyn_doc.dirty());
        // M.3 (2026-06-01): apply_edit now translates and
        // forwards rather than returning ReadOnly.
        let applied = dyn_doc
            .apply_edit(Edit::insert(Position::ZERO, "x"))
            .await
            .expect("apply_edit should propagate");
        assert_eq!(applied.inserted_text, "x");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn source_edit_propagates_after_recompose() {
        let (sources, ids) = make_sources(&["alpha\nbeta\ngamma\n"]);
        let source_handle = sources.get(&ids[0]).expect("source present").clone();
        let excerpts = vec![Excerpt::new(ids[0], 0, 2)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        assert_eq!(mb.snapshot().text(), "alpha\nbeta\ngamma\n");

        source_handle
            .apply_edit(Edit::insert(Position::new(1, 0), "BB-"))
            .await
            .unwrap();
        assert_eq!(mb.snapshot().text(), "alpha\nbeta\ngamma\n");
        mb.recompose();
        assert_eq!(mb.snapshot().text(), "alpha\nBB-beta\ngamma\n");
    }

    // ─────────────────────────────────────────────────────────────
    // Header provider tests (moved from `lattice-host::multibuffer`)
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn header_rows_dedupe_consecutive_same_source() {
        // K.4.6 follow-up (2026-06-02): three excerpts sharing
        // the same source BufferId emit ONE header row (anchored
        // at the first excerpt's composed start). The remaining
        // two excerpts advance the composed cursor without
        // emitting headers. Closes the "1 header per file" UX
        // for grep-style search results.
        let mb_source = BufferId::next();
        let excerpts = vec![
            Excerpt::new(mb_source, 0, 2).with_header(ExcerptHeader::new("a")),
            Excerpt::new(mb_source, 0, 1).with_header(ExcerptHeader::new("b")),
            Excerpt::new(mb_source, 0, 0).with_header(ExcerptHeader::new("c")),
        ];
        let rows = compose_header_rows(&excerpts, |_| Arc::from(Vec::<Cell>::new()));

        assert_eq!(
            rows.len(),
            1,
            "consecutive same-source excerpts dedup to one header"
        );
        assert_eq!(rows[0].anchor_line, 0);
        assert_eq!(rows[0].position, AnchorPosition::Above);
        assert_eq!(rows[0].height, 1);
        assert_eq!(rows[0].kind, VirtualRowKind::Generic);
    }

    #[test]
    fn header_rows_distinct_sources_each_emit_header() {
        // K.4.6 follow-up (2026-06-02): excerpts with distinct
        // source BufferIds each emit their own header at the
        // correct composed offset. Three sources → three headers
        // at 0, 3, 5. Mirrors the production scenario where
        // search-provider clusters from three different files
        // appear in the composed view.
        let src_a = BufferId::next();
        let src_b = BufferId::next();
        let src_c = BufferId::next();
        let excerpts = vec![
            Excerpt::new(src_a, 0, 2).with_header(ExcerptHeader::new("a")),
            Excerpt::new(src_b, 0, 1).with_header(ExcerptHeader::new("b")),
            Excerpt::new(src_c, 0, 0).with_header(ExcerptHeader::new("c")),
        ];
        let rows = compose_header_rows(&excerpts, |_| Arc::from(Vec::<Cell>::new()));

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].anchor_line, 0);
        assert_eq!(rows[1].anchor_line, 3);
        assert_eq!(rows[2].anchor_line, 5);
        for row in &rows {
            assert_eq!(row.position, AnchorPosition::Above);
            assert_eq!(row.height, 1);
            assert_eq!(row.kind, VirtualRowKind::Generic);
        }
    }

    #[test]
    fn header_rows_interleaved_sources_each_get_header() {
        // K.4.6 follow-up (2026-06-02): when the same source
        // re-appears after a different source, it gets its own
        // header (the dedup is on *consecutive* same-source, not
        // on "has this source ever been seen"). Models a
        // pathological search ordering where hits from file A
        // and file B are interleaved.
        let src_a = BufferId::next();
        let src_b = BufferId::next();
        let excerpts = vec![
            Excerpt::new(src_a, 0, 0).with_header(ExcerptHeader::new("a")),
            Excerpt::new(src_b, 0, 0).with_header(ExcerptHeader::new("b")),
            Excerpt::new(src_a, 1, 1).with_header(ExcerptHeader::new("a-again")),
        ];
        let rows = compose_header_rows(&excerpts, |_| Arc::from(Vec::<Cell>::new()));

        assert_eq!(
            rows.len(),
            3,
            "non-consecutive same source still emits its own header"
        );
    }

    #[test]
    fn default_header_paints_box_rules_around_title() {
        let mb_source = BufferId::next();
        let with_title = Excerpt::new(mb_source, 0, 0).with_header(ExcerptHeader::new("hi"));
        let cells = default_header_cells(&with_title);
        assert_eq!(cells.len(), 8);
        assert_eq!(cells[0].codepoint, '─' as u32);
        assert_eq!(cells[3].codepoint, 'h' as u32);
        assert_eq!(cells[4].codepoint, 'i' as u32);

        let without_title = Excerpt::new(mb_source, 0, 0);
        let cells = default_header_cells(&without_title);
        assert_eq!(cells.len(), 4);
        for cell in cells.iter() {
            assert_eq!(cell.codepoint, '─' as u32);
        }
    }

    // ── MH.A3 / MH.A5: rich per-segment header_cells ────────────

    /// Collect the codepoints of every cell carrying `fg` into a String.
    fn cells_with_fg(cells: &[Cell], fg: u32) -> String {
        cells
            .iter()
            .filter(|c| c.fg == fg)
            .filter_map(|c| char::from_u32(c.codepoint))
            .collect()
    }

    fn cells_to_string(cells: &[Cell]) -> String {
        cells
            .iter()
            .filter_map(|c| char::from_u32(c.codepoint))
            .collect()
    }

    #[test]
    fn header_cells_with_path_splits_basename_and_dir() {
        // header_fg=0xAA, path_fg=0xBB, count_fg=0xCC (distinct).
        let header = ExcerptHeader {
            path: Some(std::path::PathBuf::from("src/multibuffer/view.rs")),
            ..ExcerptHeader::new("fallback-title")
        };
        let cells = header_cells(&header, false, 0xAA, 0xBB, 0xCC);

        // First cell is the (BMP-fallback) file-type icon in header_fg.
        let expected_icon = lattice_core::ui::icons::glyph_for_entry(
            std::path::Path::new("src/multibuffer/view.rs"),
            false,
            false,
        );
        let first_icon_ch = expected_icon.chars().next().unwrap();
        assert_eq!(cells[0].codepoint, first_icon_ch as u32);
        assert_eq!(cells[0].fg, 0xAA, "icon carries header_fg");

        // Basename present in header_fg.
        let header_seg = cells_with_fg(&cells, 0xAA);
        assert!(
            header_seg.contains("view.rs"),
            "basename rendered in header_fg; got {header_seg:?}"
        );
        assert!(
            !header_seg.contains("src/multibuffer"),
            "dir must NOT be in header_fg; got {header_seg:?}"
        );

        // Directory path present in path_fg (dim).
        let path_seg = cells_with_fg(&cells, 0xBB);
        assert!(
            path_seg.contains("src/multibuffer"),
            "dir rendered in path_fg; got {path_seg:?}"
        );
    }

    #[test]
    fn header_cells_match_count_badge_in_count_fg() {
        let header = ExcerptHeader {
            path: Some(std::path::PathBuf::from("a/b.rs")),
            match_count: Some(3),
            ..ExcerptHeader::default()
        };
        let cells = header_cells(&header, false, 0xAA, 0xBB, 0xCC);
        let count_seg = cells_with_fg(&cells, 0xCC);
        assert!(
            count_seg.contains("3 matches"),
            "plural badge in count_fg; got {count_seg:?}"
        );

        // Singular form for n == 1.
        let header1 = ExcerptHeader {
            match_count: Some(1),
            ..header.clone()
        };
        let cells1 = header_cells(&header1, false, 0xAA, 0xBB, 0xCC);
        let count_seg1 = cells_with_fg(&cells1, 0xCC);
        assert!(
            count_seg1.contains("1 match") && !count_seg1.contains("matches"),
            "singular badge for n==1; got {count_seg1:?}"
        );
    }

    #[test]
    fn header_cells_nerd_vs_bmp_same_width_different_icon() {
        let header = ExcerptHeader {
            path: Some(std::path::PathBuf::from("src/lib.rs")),
            ..ExcerptHeader::default()
        };
        let bmp = header_cells(&header, false, 0xAA, 0xBB, 0xCC);
        let nerd = header_cells(&header, true, 0xAA, 0xBB, 0xCC);

        // Width parity: the icon helper emits a 2-cell glyph in both
        // palettes, so total cell count is identical.
        assert_eq!(
            bmp.len(),
            nerd.len(),
            "nerd vs BMP must occupy the same cell count (column geometry stable)"
        );

        // Different leading icon codepoint between palettes.
        let bmp_icon = lattice_core::ui::icons::glyph_for_entry(
            std::path::Path::new("src/lib.rs"),
            false,
            false,
        );
        let nerd_icon = lattice_core::ui::icons::glyph_for_entry(
            std::path::Path::new("src/lib.rs"),
            false,
            true,
        );
        // (Guard: only assert "different" if the helper actually
        // returns distinct glyphs for this extension — it does for
        // `.rs`, but keep the test honest about its premise.)
        assert_ne!(
            bmp_icon.chars().next(),
            nerd_icon.chars().next(),
            "nerd and BMP icon glyphs differ for .rs"
        );
        assert_eq!(bmp[0].codepoint, bmp_icon.chars().next().unwrap() as u32);
        assert_eq!(nerd[0].codepoint, nerd_icon.chars().next().unwrap() as u32);
    }

    #[test]
    fn header_cells_empty_title_no_path_falls_back() {
        let header = ExcerptHeader::default(); // empty title, no path
        let cells = header_cells(&header, false, 0xAA, 0xBB, 0xCC);
        let s = cells_to_string(&cells);
        assert_eq!(s, "[untitled]", "empty title + no path renders fallback");
        // Fallback rendered in header_fg.
        assert!(cells.iter().all(|c| c.fg == 0xAA));
    }

    #[test]
    fn header_cells_no_path_uses_title() {
        let header = ExcerptHeader::new("my synthetic title");
        let cells = header_cells(&header, false, 0xAA, 0xBB, 0xCC);
        let s = cells_to_string(&cells);
        assert_eq!(s, "my synthetic title");
        assert!(cells.iter().all(|c| c.fg == 0xAA));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn provider_collects_one_row_per_distinct_source() {
        // K.4.6 follow-up (2026-06-02): two excerpts from the
        // same source dedup to ONE header — the search-provider
        // "1 header per file, N excerpts per file (one per
        // cluster)" UX.
        let (sources, ids) = make_sources(&["alpha\nbeta\ngamma\n"]);
        let excerpts = vec![
            Excerpt::new(ids[0], 0, 1).with_header(ExcerptHeader::new("first")),
            Excerpt::new(ids[0], 2, 2).with_header(ExcerptHeader::new("second")),
        ];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        let provider = MultibufferExcerptHeaderProvider::new(mb);
        let rows = provider.collect();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].anchor_line, 0);
    }

    #[test]
    fn provider_id_namespace_is_stable() {
        let buffer_id = BufferId(42);
        let id = multibuffer_excerpt_header_provider_id(buffer_id);
        assert_eq!(id & 0xFFFF_FFFF, 42);
        assert!(!(0xD1FF_0000_0000_0000..0xD200_0000_0000_0000).contains(&id));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn provider_version_bumps_with_recompose() {
        let (sources, ids) = make_sources(&["alpha\nbeta\n"]);
        let source = sources.get(&ids[0]).unwrap().clone();
        let excerpts = vec![Excerpt::new(ids[0], 0, 0)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        let provider = MultibufferExcerptHeaderProvider::new(mb.clone());

        let v_before = provider.version();
        source
            .apply_edit(Edit::insert(Position::ZERO, "X"))
            .await
            .unwrap();
        mb.recompose();
        let v_after = provider.version();
        assert!(
            v_after > v_before,
            "version must bump after recompose; before={v_before} after={v_after}"
        );
    }

    // ── T.7: mode-owned theme-element acid test ─────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn excerpt_header_elements_register_and_bake_into_virtual_row() {
        // T.7 acid test: the multibuffer mode registers its OWN theme
        // elements (`multibuffer.excerpt_header[.path|.count]`) and the
        // header provider resolves+bakes them into the header
        // VirtualRow's `bg`. No host `Theme` field, no renderer match
        // arm — the renderer paints `VirtualRow.bg` generically.
        use lattice_theme::{
            ElementName, ElementOwner, InMemoryThemeRegistry, ThemeRegistry, ThemeRegistryHandle,
            default_palette,
        };

        // A registry with the default palette (so `blue` / `overlay2`
        // palette keys resolve) but NO core builtins — proving the mode
        // is the sole registrant of these elements.
        let registry = Arc::new(InMemoryThemeRegistry::new(default_palette()));

        // The mode registers its elements (idempotent by name).
        let owner = ElementOwner::Mode(
            crate::MultibufferMode::mode_id()
                .as_str()
                .to_string()
                .into(),
        );
        let ids = crate::register_multibuffer_theme_elements(registry.as_ref(), owner);

        // 1) The element is registered + resolves to the neutral backdrop.
        let backdrop_name = ElementName::from(ELEM_EXCERPT_HEADER.to_string());
        let id = registry
            .id(&backdrop_name)
            .expect("multibuffer.excerpt_header registered after activation");
        assert_eq!(id, ids.backdrop);
        let resolved = registry.resolved();
        assert_eq!(
            resolved.get(id).bg,
            Some(Color::Rgb(0x31, 0x32, 0x44)),
            "excerpt_header backdrop resolves to the neutral surface tint, \
             NOT the diff-deletion red"
        );

        // 2) The built header VirtualRow carries the BAKED bg —
        //    `Some(0x313244)`, not `None` (which would fall through to
        //    the renderer's diff-deletion-block tint).
        let theme: ThemeRegistryHandle = registry.clone();
        let (sources, src_ids) = make_sources(&["alpha\nbeta\ngamma\n"]);
        // MH.A3: supply a `path` so the rich header renders the dir
        // segment in the resolved `.path` (blue) fg — the basename is
        // in `header_fg` (the backdrop element's `.fg`, unset here).
        let header = ExcerptHeader {
            path: Some(std::path::PathBuf::from("src/file.rs")),
            ..ExcerptHeader::new("file.rs")
        };
        let excerpts = vec![Excerpt::new(src_ids[0], 0, 1).with_header(header)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        let provider = MultibufferExcerptHeaderProvider::with_theme(mb, theme, ids, false);
        let rows = provider.collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].bg,
            Some(0x0031_3244),
            "header VirtualRow.bg is the baked backdrop, not None"
        );
        assert_eq!(rows[0].kind, VirtualRowKind::Generic);
        // The dir-path cells carry the baked `blue` fg (0x89b4fa).
        assert!(
            rows[0].cells.iter().any(|c| c.fg == 0x0089_b4fa),
            "dir-path cells carry the resolved path fg (blue)"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn excerpt_header_provider_version_folds_theme_version() {
        // T.7 invalidation: a theme change bumps the provider version
        // (the worker's fingerprint axis), so `collect()` re-runs and
        // re-bakes — mirrors `MatrixVersion::theme = resolved().version()`.
        use lattice_theme::{
            ElementName, ElementOwner, InMemoryThemeRegistry, StyleSpec, ThemeRegistry,
            ThemeRegistryHandle, default_palette,
        };
        let registry = Arc::new(InMemoryThemeRegistry::new(default_palette()));
        let owner = ElementOwner::Mode(
            crate::MultibufferMode::mode_id()
                .as_str()
                .to_string()
                .into(),
        );
        let ids = crate::register_multibuffer_theme_elements(registry.as_ref(), owner);
        let theme: ThemeRegistryHandle = registry.clone();
        let (sources, src_ids) = make_sources(&["a\nb\n"]);
        let excerpts = vec![Excerpt::new(src_ids[0], 0, 0)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        let provider = MultibufferExcerptHeaderProvider::with_theme(mb, theme, ids, false);

        // Force the table to resolve so the version is established.
        let _ = registry.resolved();
        let v_before = provider.version();
        // A `:set ui.*`-style override dirties the table → next
        // `resolved()` bumps the version.
        registry.set_override(
            ElementName::from(ELEM_EXCERPT_HEADER.to_string()),
            StyleSpec::new().bg(Color::Rgb(1, 2, 3)),
        );
        let v_after = provider.version();
        assert!(
            v_after > v_before,
            "theme change must bump provider version; before={v_before} after={v_after}"
        );
    }

    // ── M.6.5: MultibufferStatusProvider ────────────────────────

    #[test]
    fn status_provider_hidden_when_idle() {
        let mb = MultibufferDocumentHandle::empty(empty_registry());
        let provider = MultibufferStatusProvider::new(mb.clone()).into_provider(mb.buffer_id());
        // Idle → no sticky row
        assert!(provider.collect().is_empty());
    }

    #[test]
    fn status_provider_emits_sticky_row_when_in_progress() {
        let mb = MultibufferDocumentHandle::empty(empty_registry());
        mb.set_headerline(HeaderlineStatus::InProgress {
            label: "searching".into(),
            count: Some(3),
            emphasis: None,
        });
        let provider = MultibufferStatusProvider::new(mb.clone()).into_provider(mb.buffer_id());
        let rows = provider.collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, VirtualRowKind::Sticky);
        assert_eq!(rows[0].anchor_line, 0);
        assert_eq!(rows[0].bg, None); // theme bg
    }

    #[test]
    fn status_provider_version_bumps_on_set_headerline() {
        let mb = MultibufferDocumentHandle::empty(empty_registry());
        let buffer_id = mb.buffer_id();
        let provider = MultibufferStatusProvider::new(mb.clone()).into_provider(buffer_id);
        let v0 = provider.version();
        mb.set_headerline(HeaderlineStatus::Complete {
            summary: "done".into(),
            emphasis: None,
        });
        assert!(
            provider.version() > v0,
            "version must advance after set_headerline"
        );
    }

    #[test]
    fn status_provider_namespace_does_not_collide_with_excerpt_header() {
        let buf = BufferId(7);
        let excerpt_id = multibuffer_excerpt_header_provider_id(buf);
        let status_id = multibuffer_status_provider_id(buf);
        assert_ne!(excerpt_id, status_id);
        assert_eq!(status_id & 0xFFFF_FFFF, 7);
    }

    // ── MH.A4: status colors resolve from theme elements ────────

    #[test]
    fn status_provider_no_theme_uses_fallback_hex() {
        // Test path (no theme service): the three states render with
        // the pre-MH.A4 fallback hex so nothing regresses where the
        // theme isn't wired.
        let cases = [
            (
                HeaderlineStatus::InProgress {
                    label: "x".into(),
                    count: None,
                    emphasis: None,
                },
                STATUS_IN_PROGRESS_FALLBACK_FG,
            ),
            (
                HeaderlineStatus::Complete {
                    summary: "done".into(),
                    emphasis: None,
                },
                STATUS_COMPLETE_FALLBACK_FG,
            ),
            (
                HeaderlineStatus::Failed {
                    reason: "boom".into(),
                },
                STATUS_FAILED_FALLBACK_FG,
            ),
        ];
        for (status, expected_fg) in cases {
            let mb = MultibufferDocumentHandle::empty(empty_registry());
            mb.set_headerline(status);
            let provider = MultibufferStatusProvider::new(mb);
            let row = provider.render().expect("non-idle status renders a row");
            assert!(
                row.cells.iter().all(|c| c.fg == expected_fg),
                "no-theme status uses fallback fg {expected_fg:#08x}"
            );
        }
    }

    #[test]
    fn status_elements_register_and_bake_resolved_fg() {
        // MH.A4 acid test: the mode registers `multibuffer.status.*`
        // elements; the status provider resolves them and bakes the
        // per-state fg into the rendered row. Assert against the
        // RESOLVED palette role-key (green / red / subtext), NOT the old
        // ad-hoc hex.
        use lattice_theme::{
            ElementName, ElementOwner, InMemoryThemeRegistry, ThemeRegistry, ThemeRegistryHandle,
            default_palette,
        };

        let registry = Arc::new(InMemoryThemeRegistry::new(default_palette()));
        let owner = ElementOwner::Mode(
            crate::MultibufferMode::mode_id()
                .as_str()
                .to_string()
                .into(),
        );
        let ids = crate::register_multibuffer_theme_elements(registry.as_ref(), owner);

        // 1) The three elements are registered + resolve to the mapped
        //    palette role-keys.
        let resolved = registry.resolved();
        let palette = default_palette();
        let role = |key: &str| palette.get(&key.to_string().into()).unwrap();
        assert_eq!(
            resolved
                .get(
                    registry
                        .id(&ElementName::from(ELEM_STATUS_IN_PROGRESS.to_string()))
                        .unwrap()
                )
                .fg,
            Some(role("subtext")),
            "in_progress maps to the grey `subtext` role-key"
        );
        assert_eq!(
            resolved
                .get(
                    registry
                        .id(&ElementName::from(ELEM_STATUS_COMPLETE.to_string()))
                        .unwrap()
                )
                .fg,
            Some(role("green")),
            "complete maps to the `green` role-key"
        );
        assert_eq!(
            resolved
                .get(
                    registry
                        .id(&ElementName::from(ELEM_STATUS_FAILED.to_string()))
                        .unwrap()
                )
                .fg,
            Some(role("red")),
            "failed maps to the `red` role-key"
        );

        // 2) The rendered status row bakes the resolved fg per state.
        let theme: ThemeRegistryHandle = registry.clone();
        let expect_fg = |status: HeaderlineStatus, key: &str| {
            let mb = MultibufferDocumentHandle::empty(empty_registry());
            mb.set_headerline(status);
            let provider = MultibufferStatusProvider::with_theme(mb, theme.clone(), ids);
            let row = provider.render().expect("non-idle status renders a row");
            let want = role(key).to_rgb_u32(0);
            assert!(
                row.cells.iter().all(|c| c.fg == want),
                "status row baked fg = resolved {key} ({want:#08x})"
            );
        };
        expect_fg(
            HeaderlineStatus::InProgress {
                label: "s".into(),
                count: Some(2),
                emphasis: None,
            },
            "subtext",
        );
        expect_fg(
            HeaderlineStatus::Complete {
                summary: "done".into(),
                emphasis: None,
            },
            "green",
        );
        expect_fg(
            HeaderlineStatus::Failed {
                reason: "err".into(),
            },
            "red",
        );
    }

    #[test]
    fn status_emphasis_term_gets_query_accent_fg() {
        // The emphasised substring (e.g. the search query woven into the
        // status label) is painted with the resolved
        // `multibuffer.status.query` accent (yellow); the rest of the row
        // keeps the state fg. Only the term cells get the accent.
        use lattice_theme::{
            ElementName, ElementOwner, InMemoryThemeRegistry, ThemeRegistry, ThemeRegistryHandle,
            default_palette,
        };
        let registry = Arc::new(InMemoryThemeRegistry::new(default_palette()));
        let owner = ElementOwner::Mode(
            crate::MultibufferMode::mode_id()
                .as_str()
                .to_string()
                .into(),
        );
        let ids = crate::register_multibuffer_theme_elements(registry.as_ref(), owner);

        // The query element resolves to the `yellow` role-key.
        let resolved = registry.resolved();
        let palette = default_palette();
        let role = |key: &str| palette.get(&key.to_string().into()).unwrap();
        assert_eq!(
            resolved
                .get(
                    registry
                        .id(&ElementName::from(ELEM_STATUS_QUERY.to_string()))
                        .unwrap()
                )
                .fg,
            Some(role("yellow")),
            "query accent maps to the `yellow` role-key"
        );

        // Render a Complete status carrying an emphasised term.
        let theme: ThemeRegistryHandle = registry.clone();
        let mb = MultibufferDocumentHandle::empty(empty_registry());
        mb.set_headerline(HeaderlineStatus::Complete {
            summary: "\"needle\" — 3 hits in 2 files".into(),
            emphasis: Some("needle".into()),
        });
        let provider = MultibufferStatusProvider::with_theme(mb, theme, ids);
        let row = provider.render().expect("non-idle status renders a row");

        let query_fg = role("yellow").to_rgb_u32(0);
        let complete_fg = role("green").to_rgb_u32(0);
        let text: String = row
            .cells
            .iter()
            .map(|c| char::from_u32(c.codepoint).unwrap_or(' '))
            .collect();
        let byte_start = text.find("needle").expect("term present in row");
        let char_start = text[..byte_start].chars().count();
        let char_end = char_start + "needle".chars().count();
        for (i, cell) in row.cells.iter().enumerate() {
            if i >= char_start && i < char_end {
                assert_eq!(cell.fg, query_fg, "emphasis cell {i} uses the query accent");
            } else {
                assert_eq!(
                    cell.fg, complete_fg,
                    "non-emphasis cell {i} keeps the state (complete) fg"
                );
            }
        }
    }

    #[test]
    fn status_provider_version_folds_theme_version() {
        // MH.A4: a colorscheme swap (theme override) bumps the status
        // provider version so the headerline re-renders the recolored
        // status row.
        use lattice_theme::{
            ElementName, ElementOwner, InMemoryThemeRegistry, StyleSpec, ThemeRegistry,
            ThemeRegistryHandle, default_palette,
        };
        let registry = Arc::new(InMemoryThemeRegistry::new(default_palette()));
        let owner = ElementOwner::Mode(
            crate::MultibufferMode::mode_id()
                .as_str()
                .to_string()
                .into(),
        );
        let ids = crate::register_multibuffer_theme_elements(registry.as_ref(), owner);
        let theme: ThemeRegistryHandle = registry.clone();
        let mb = MultibufferDocumentHandle::empty(empty_registry());
        mb.set_headerline(HeaderlineStatus::Complete {
            summary: "done".into(),
            emphasis: None,
        });
        let provider = MultibufferStatusProvider::with_theme(mb, theme, ids);

        let _ = registry.resolved();
        let v_before = provider.version();
        registry.set_override(
            ElementName::from(ELEM_STATUS_COMPLETE.to_string()),
            StyleSpec::new().fg(Color::Rgb(1, 2, 3)),
        );
        let v_after = provider.version();
        assert!(
            v_after > v_before,
            "theme change must bump status provider version; before={v_before} after={v_after}"
        );
    }
}

// ── M.7: ExcerptFoldProvider ────────────────────────────────────────────────

/// Namespace for excerpt fold provider IDs: `0xBBBB_0003_0000_0000 | buffer_id`.
/// Distinct from the header provider (`0xBBBB_0001_*`) and status
/// provider (`0xBBBB_0002_*`) namespaces.
const EXCERPT_FOLD_NAMESPACE: u64 = 0xBBBB_0003_0000_0000;
const FILE_BOUNDARY_FOLD_NAMESPACE: u64 = 0xBBBB_0004_0000_0000;

/// M.7: computes one open [`lattice_core::Fold`] per excerpt in the
/// composed multibuffer. Registered as a
/// [`lattice_core::FoldSource`] by `MultibufferMode::on_activate`
/// via `FoldOverlayService`; the `FoldSourceAdapter` in `lattice-host`
/// gates `compute_folds` to calls where `FoldContext::buffer_id`
/// matches this multibuffer's buffer ID.
pub struct ExcerptFoldProvider {
    id: lattice_core::ProviderId,
    handle: MultibufferDocumentHandle,
}

impl ExcerptFoldProvider {
    pub fn new(handle: MultibufferDocumentHandle, buffer_id: BufferId) -> Self {
        let id = lattice_core::ProviderId(EXCERPT_FOLD_NAMESPACE | buffer_id.0 as u64);
        Self { id, handle }
    }
}

impl lattice_core::FoldSource for ExcerptFoldProvider {
    fn id(&self) -> lattice_core::ProviderId {
        self.id
    }

    fn compute_folds(&self) -> Vec<lattice_core::Fold> {
        let excerpts = self.handle.excerpts();
        let starts = crate::motions::excerpt_start_rows(&excerpts);
        excerpts
            .iter()
            .zip(starts.iter())
            .map(|(excerpt, &start)| {
                let line_count = excerpt.line_count();
                let end = start.saturating_add(line_count.saturating_sub(1));
                lattice_core::Fold {
                    start_line: start,
                    end_line: end,
                    closed: false,
                    identity: Some(excerpt.id.0),
                }
            })
            .collect()
    }
}

/// M.8: computes one open [`lattice_core::Fold`] per distinct source
/// [`BufferId`] (file), spanning the composed-row range from the first
/// to the last excerpt belonging to that file. Registered alongside
/// `ExcerptFoldProvider` by `MultibufferMode::on_activate`. Enables
/// collapsing all excerpts from a file to its header row with `za`.
pub struct FileBoundaryFoldProvider {
    id: lattice_core::ProviderId,
    handle: MultibufferDocumentHandle,
}

impl FileBoundaryFoldProvider {
    pub fn new(handle: MultibufferDocumentHandle, buffer_id: BufferId) -> Self {
        let id = lattice_core::ProviderId(FILE_BOUNDARY_FOLD_NAMESPACE | buffer_id.0 as u64);
        Self { id, handle }
    }
}

impl lattice_core::FoldSource for FileBoundaryFoldProvider {
    fn id(&self) -> lattice_core::ProviderId {
        self.id
    }

    fn compute_folds(&self) -> Vec<lattice_core::Fold> {
        let excerpts = self.handle.excerpts();
        if excerpts.is_empty() {
            return Vec::new();
        }
        let starts = crate::motions::excerpt_start_rows(&excerpts);
        // Group by source BufferId: track (start_row, end_row) per file.
        // First appearance sets start_row; later excerpts from the same
        // file extend end_row (handles non-contiguous same-file excerpts).
        let mut by_source: std::collections::HashMap<BufferId, (u32, u32)> =
            std::collections::HashMap::new();
        for (excerpt, &start) in excerpts.iter().zip(starts.iter()) {
            let end = start.saturating_add(excerpt.line_count().saturating_sub(1));
            by_source
                .entry(excerpt.source)
                .and_modify(|(_, e)| *e = end)
                .or_insert((start, end));
        }
        // Sort by start_row for deterministic output order.
        let mut groups: Vec<(BufferId, (u32, u32))> = by_source.into_iter().collect();
        groups.sort_by_key(|(_, (start, _))| *start);
        groups
            .into_iter()
            .map(|(source, (start, end))| lattice_core::Fold {
                start_line: start,
                end_line: end,
                closed: false,
                identity: Some(source.0 as u64),
            })
            .collect()
    }
}

#[cfg(test)]
mod excerpt_fold_tests {
    #![allow(clippy::unwrap_used)]
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use lattice_core::Document as CoreDocument;
    use lattice_core::{FoldOverlayService, FoldOverlayServiceHandle, FoldSource};
    use lattice_grammar::CommandRegistry;
    use lattice_runtime::spawn_document;

    use super::*;

    fn reg() -> lattice_grammar::CommandRegistryHandle {
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn compute_folds_returns_one_fold_per_excerpt() {
        let r = reg();
        let buf_a = BufferId::next();
        let buf_b = BufferId::next();
        let doc_a = spawn_document(buf_a, CoreDocument::from_text("a\nb\nc\n"), r.clone());
        let doc_b = spawn_document(buf_b, CoreDocument::from_text("x\ny\n"), r.clone());
        let mut sources: HashMap<BufferId, Arc<dyn Document>> = HashMap::new();
        sources.insert(buf_a, Arc::new(doc_a));
        sources.insert(buf_b, Arc::new(doc_b));
        let excerpts = vec![
            Excerpt::new(buf_a, 0, 2), // 3 lines → composed rows 0–2
            Excerpt::new(buf_b, 0, 1), // 2 lines → composed rows 3–4
        ];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, r).unwrap();
        let buf_id = mb.buffer_id();
        let provider = ExcerptFoldProvider::new(mb, buf_id);
        let folds = provider.compute_folds();
        assert_eq!(folds.len(), 2);
        assert_eq!(folds[0].start_line, 0);
        assert_eq!(folds[0].end_line, 2);
        assert!(!folds[0].closed);
        assert!(folds[0].identity.is_some());
        assert_eq!(folds[1].start_line, 3);
        assert_eq!(folds[1].end_line, 4);
        assert!(!folds[1].closed);
        assert!(folds[1].identity.is_some());
    }

    #[test]
    fn excerpt_fold_namespace_distinct_from_header_and_status() {
        let buf = BufferId(42);
        let fold_id = ExcerptFoldProvider::new(MultibufferDocumentHandle::empty(reg()), buf).id;
        // header/status ProviderId uses lattice_cells::virtual_rows::ProviderId;
        // compare raw u64 values to verify no namespace collision.
        let header_raw: u64 = multibuffer_excerpt_header_provider_id(buf);
        let status_raw: u64 = multibuffer_status_provider_id(buf);
        assert_ne!(fold_id.0, header_raw);
        assert_ne!(fold_id.0, status_raw);
        assert_eq!(fold_id.0 >> 32, 0xBBBB_0003);
        assert_eq!(fold_id.0 & 0xFFFF_FFFF, 42);
    }

    #[test]
    fn compute_folds_empty_when_no_excerpts() {
        let mb = MultibufferDocumentHandle::empty(reg());
        let buf_id = mb.buffer_id();
        let provider = ExcerptFoldProvider::new(mb, buf_id);
        assert!(provider.compute_folds().is_empty());
    }

    // ── MultibufferModeGuard drop ───────────────────────────────────────

    struct MockFoldService {
        removed: Arc<AtomicBool>,
    }
    impl FoldOverlayService for MockFoldService {
        fn add_source(
            &self,
            _source: Arc<dyn FoldSource>,
            _buffer_id: BufferId,
        ) -> lattice_core::ProviderId {
            lattice_core::ProviderId(1)
        }
        fn remove_source(&self, _id: lattice_core::ProviderId) {
            self.removed.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn mode_guard_drop_calls_remove_source() {
        let removed = Arc::new(AtomicBool::new(false));
        let svc: FoldOverlayServiceHandle = Arc::new(MockFoldService {
            removed: removed.clone(),
        });
        {
            let _guard = crate::mode::MultibufferModeGuard {
                fold_registrations: vec![(svc, lattice_core::ProviderId(1))],
                // Empty: this test covers the fold-registration half
                // of Drop only. The action-handler tokens unregister
                // themselves via their own Drop impl, which
                // `lattice-mode` tests separately.
                _action_handler_registrations: Vec::new(),
            };
        }
        assert!(
            removed.load(Ordering::SeqCst),
            "remove_source must fire on guard drop"
        );
    }

    // ── FileBoundaryFoldProvider ────────────────────────────────────────

    #[tokio::test(flavor = "current_thread")]
    async fn file_boundary_folds_one_fold_per_source_file() {
        let r = reg();
        let buf_a = BufferId::next();
        let buf_b = BufferId::next();
        let doc_a = spawn_document(buf_a, CoreDocument::from_text("a\nb\nc\n"), r.clone());
        let doc_b = spawn_document(buf_b, CoreDocument::from_text("x\ny\n"), r.clone());
        let mut sources: HashMap<BufferId, Arc<dyn Document>> = HashMap::new();
        sources.insert(buf_a, Arc::new(doc_a));
        sources.insert(buf_b, Arc::new(doc_b));
        // Two excerpts from buf_a, one from buf_b.
        // buf_a excerpt 1: rows 0-2 (3 lines); buf_a excerpt 2: rows 3-4 (2 lines);
        // buf_b excerpt:   rows 5-6 (2 lines)
        let excerpts = vec![
            Excerpt::new(buf_a, 0, 2),
            Excerpt::new(buf_a, 0, 1),
            Excerpt::new(buf_b, 0, 1),
        ];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, r).unwrap();
        let buf_id = mb.buffer_id();
        let provider = FileBoundaryFoldProvider::new(mb, buf_id);
        let folds = provider.compute_folds();
        assert_eq!(folds.len(), 2, "one fold per source file");
        // buf_a fold spans rows 0-4 (first excerpt start to second excerpt end).
        let a_fold = folds
            .iter()
            .find(|f| f.start_line == 0)
            .expect("buf_a fold");
        assert_eq!(a_fold.end_line, 4);
        assert_eq!(a_fold.identity, Some(buf_a.0 as u64));
        // buf_b fold spans rows 5-6.
        let b_fold = folds
            .iter()
            .find(|f| f.start_line == 5)
            .expect("buf_b fold");
        assert_eq!(b_fold.end_line, 6);
        assert_eq!(b_fold.identity, Some(buf_b.0 as u64));
    }

    #[test]
    fn file_boundary_fold_namespace_distinct() {
        let buf = BufferId(42);
        let fold_id =
            FileBoundaryFoldProvider::new(MultibufferDocumentHandle::empty(reg()), buf).id;
        let excerpt_id = ExcerptFoldProvider::new(MultibufferDocumentHandle::empty(reg()), buf).id;
        let header_raw: u64 = multibuffer_excerpt_header_provider_id(buf);
        let status_raw: u64 = multibuffer_status_provider_id(buf);
        assert_ne!(fold_id.0, excerpt_id.0);
        assert_ne!(fold_id.0, header_raw);
        assert_ne!(fold_id.0, status_raw);
        assert_eq!(fold_id.0 >> 32, 0xBBBB_0004);
        assert_eq!(fold_id.0 & 0xFFFF_FFFF, 42);
    }

    #[test]
    fn file_boundary_folds_empty_when_no_excerpts() {
        let mb = MultibufferDocumentHandle::empty(reg());
        let buf_id = mb.buffer_id();
        let provider = FileBoundaryFoldProvider::new(mb, buf_id);
        assert!(provider.compute_folds().is_empty());
    }
}
