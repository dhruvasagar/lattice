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
//! * **Header provider**: `MultibufferHeaderProvider` (impl
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

pub mod mode;
pub mod motions;
pub mod providers;
pub mod registry;
pub mod view;

pub use crate::mode::{
    MultibufferMode, register_multibuffer_ex_commands, register_multibuffer_modes,
};
pub use crate::motions::{MultibufferMotionIds, register_multibuffer_motions};
pub use crate::registry::{
    InMemoryMultibufferRegistry, MultibufferRegistry, MultibufferRegistryHandle,
};
pub use crate::view::create_multibuffer_view;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use arc_swap::ArcSwap;
use lattice_cells::cell::Cell;
use lattice_cells::virtual_rows::{
    AnchorPosition, ProviderId, VirtualRow, VirtualRowKind, VirtualRowProvider,
};
use lattice_core::buffer::AppliedEdit;
use lattice_core::{Buffer, BufferId};
use lattice_grammar::{CancellationToken, CommandInvocation, CommandRegistry, Effect};
use lattice_protocol::edit::Edit;
use lattice_protocol::ids::DocumentId;
use lattice_protocol::position::Position;
use lattice_protocol::selection::SelectionSet;
use lattice_runtime::{
    Document, DocumentSnapshot, Pending, PublishedSnapshot, RuntimeError, SnapshotCache,
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

/// Header presentation for an excerpt — title + style.
#[derive(Debug, Clone)]
pub struct ExcerptHeader {
    /// Human-readable label. Conventionally
    /// `"<path> : <start_line+1>-<end_line+1>"` for a regular
    /// file excerpt (1-indexed for display). Empty string = no
    /// header rendered.
    pub title: String,
    pub style: ExcerptHeaderStyle,
}

impl ExcerptHeader {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            style: ExcerptHeaderStyle::default(),
        }
    }
}

impl Default for ExcerptHeader {
    fn default() -> Self {
        Self {
            title: String::new(),
            style: ExcerptHeaderStyle::default(),
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
        let mut entries = Vec::new();
        for excerpt in excerpts {
            for row in excerpt.start_line..=excerpt.end_line {
                entries.push(RowEntry::Excerpt {
                    excerpt_id: excerpt.id,
                    source_row: row,
                });
            }
        }
        Self { entries }
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
    registry: Arc<CommandRegistry>,
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
        if let Ok(mut book) = self.subscriptions.lock() {
            if let Some(bus) = book.bus.take() {
                for id in book.ids.drain(..) {
                    let _ = bus.unsubscribe(id);
                }
            }
        }
    }
}

struct MultibufferState {
    sources: HashMap<BufferId, Arc<dyn Document>>,
    excerpts: Vec<Excerpt>,
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
/// excerpt (M.2.a `MultibufferHeaderProvider` extends to handle
/// the view header in a later renderer slice).
///
/// Async providers transition `Idle → InProgress → Complete` /
/// `Failed` as their scan progresses. See
/// `multibuffer-views.md` §3.7.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderlineStatus {
    /// No status rendered. The view-header virtual row is empty.
    Idle,
    /// A scan / fetch / computation is running. `label` describes
    /// it; `count` is an optional running tally (hits found so
    /// far, files scanned, etc.).
    InProgress { label: String, count: Option<usize> },
    /// The operation completed successfully. `summary` is the
    /// terminal label rendered to the user.
    Complete { summary: String },
    /// The operation failed. `reason` is the terminal label
    /// rendered to the user.
    Failed { reason: String },
}

impl Default for HeaderlineStatus {
    fn default() -> Self {
        Self::Idle
    }
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
        registry: Arc<CommandRegistry>,
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
        let composed = compose_snapshot(
            id,
            &sources,
            &excerpts,
            Arc::new(SelectionSet::default()),
        );
        let snapshot_cell = Arc::new(PublishedSnapshot::new(composed));

        Ok(Self {
            inner: Arc::new(MultibufferInner {
                id,
                buffer_id,
                state: std::sync::Mutex::new(MultibufferState {
                    sources,
                    excerpts,
                    selections: Arc::new(SelectionSet::default()),
                }),
                snapshot_cell,
                row_translation: ArcSwap::from_pointee(row_translation),
                headerline: ArcSwap::from_pointee(HeaderlineStatus::Idle),
                subscriptions: std::sync::Mutex::new(SubscriptionBookkeeping::default()),
                registry,
            }),
        })
    }

    /// Convenience constructor for the async-provider pattern:
    /// build an empty view with no sources and no excerpts. The
    /// provider streams content in via [`Self::append_excerpts`].
    /// Infallible.
    ///
    /// K.4.11 (2026-06-02): takes the same `Arc<CommandRegistry>`
    /// as the full [`Self::new`] constructor. The multibuffer is
    /// grammar-capable from creation — empty-view or not.
    pub fn empty(registry: Arc<CommandRegistry>) -> Self {
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
        let source_line_count = source.snapshot().buffer.line_count() as i64;
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
        for ex in excerpts {
            if !state.sources.contains_key(&ex.source) {
                // Silently drop — the provider is responsible
                // for adding the source first via `add_source`
                // if it's a new file. M.6 SearchProvider does
                // this in its scan task.
                continue;
            }
            state.excerpts.push(ex);
        }
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
        state.sources = sources;
        state.excerpts = excerpts;
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

    /// M.2.b.2 (2026-06-01): add a source buffer to the view's
    /// source map. Subsequent `append_excerpts` calls can
    /// reference it. Idempotent: re-adding an existing source
    /// updates the handle reference (which may have been
    /// replaced via slot-replacement upstream).
    pub fn add_source(&self, id: BufferId, source: Arc<dyn Document>) {
        let mut state = self.lock_state();
        state.sources.insert(id, source);
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

        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<lattice_protocol::Event>();
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
                    lattice_protocol::Event::DocumentChanged {
                        id, edits, ..
                    } => {
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
/// line-bounded). A first-class `Anchor` primitive (line + col
/// + generation) can land later if column-precise tracking
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
        let state = self.lock_state();
        let Some(target) = resolve_edit_target(&state, edit.range.start) else {
            return Pending::ready(Err(RuntimeError::ReadOnly));
        };
        let source_edit = build_source_edit(&target, &edit);
        let source_handle = target.source_handle.clone();
        // 2026-06-02 cursor-jump fix: the row offset between
        // composed coords (what the host's cursor lives in) and
        // source coords (what `source_handle.apply_edit` produces
        // in the returned `AppliedEdit`). Without translating
        // back, the host's insert-mode path reads
        // `applied.inserted_range.end` (dispatch.rs:5422) and
        // sets `editor.cursor = source_row` — cursor jumps to
        // composed row 429 (or wherever the source row lands)
        // and subsequent inserts go off into a void.
        //
        // 2026-06-02 freeze fix: this used to wrap in
        // `Pending::spawn(...)` which calls `tokio::spawn` —
        // tokio::spawn captures the caller's runtime, and
        // `apply_edit`'s synchronous body runs on the editor
        // actor's current_thread runtime BEFORE `block_on`
        // swaps to the bridge thread. The spawned task got
        // scheduled on the editor actor's current_thread
        // runtime; that runtime then blocks in `block_on`
        // waiting for the new oneshot — but the spawned task
        // can't progress because the runtime is blocked.
        // Deadlock on the first user-visible apply_edit; UI
        // freeze.
        //
        // Pending::map_ok attaches the transform without a new
        // task: the transform runs on whichever thread polls
        // the Pending (the bridge thread inside `block_on`'s
        // shared-runtime context). No spawn, no deadlock,
        // "UI never blocks" honoured.
        let row_delta = target.composed_start.line as i64 - target.source_start.line as i64;
        let inner_for_recompose = self.inner.clone();
        drop(state);
        source_handle
            .apply_edit(source_edit)
            .map_ok(move |applied| {
                // 2026-06-02 stale-snapshot fix: after the source's
                // apply_edit lands, the source's rope reflects the
                // new content but the multibuffer's composed
                // snapshot is still the pre-edit version. The M.4
                // auto-recompose forwarder listens for
                // `DocumentChanged` events under the SOURCE's id,
                // but the host's `apply_edit_blocking` publishes
                // `DocumentChanged` under the ACTIVE doc's id
                // (the multibuffer itself) — the forwarder
                // ignores it. Result: cursor advances correctly
                // (translate_applied_to_composed produces fresh
                // composed coords) but rendered text stays
                // unchanged. Recompose synchronously here so the
                // host's post-apply_edit_blocking re-render reads
                // the updated snapshot. Idempotent vs. any
                // forwarder-driven recompose from cross-pane edits.
                recompose_inner(&inner_for_recompose);
                translate_applied_to_composed(applied, row_delta)
            })
    }

    /// M.3 (2026-06-01): translate + forward each edit to its
    /// source. The batch is serialised through `apply_edit`
    /// per-edit and combined via `Pending::spawn` so the
    /// returned `Pending` resolves asynchronously without
    /// blocking the runtime. Multi-source batches dispatch
    /// each sub-edit sequentially; per-edit parallelism is a
    /// later refinement once a consumer needs it.
    fn apply_edit_batch(&self, edits: Vec<Edit>) -> Pending<Vec<AppliedEdit>> {
        // Translate up-front (cheap, requires the state lock).
        // 2026-06-02 cursor-jump fix: keep `row_delta` per call
        // so the per-result `AppliedEdit` can be translated back
        // to composed coords (same fix as single `apply_edit`).
        let state = self.lock_state();
        let mut calls: Vec<(Arc<dyn Document>, Edit, i64)> = Vec::with_capacity(edits.len());
        for edit in edits {
            if let Some(target) = resolve_edit_target(&state, edit.range.start) {
                let source_edit = build_source_edit(&target, &edit);
                let handle = target.source_handle.clone();
                let row_delta =
                    target.composed_start.line as i64 - target.source_start.line as i64;
                calls.push((handle, source_edit, row_delta));
            }
        }
        drop(state);

        Pending::spawn(async move {
            let mut results = Vec::with_capacity(calls.len());
            for (handle, edit, row_delta) in calls {
                match handle.apply_edit(edit).await {
                    Ok(applied) => {
                        results.push(translate_applied_to_composed(applied, row_delta))
                    }
                    Err(RuntimeError::ReadOnly) => continue,
                    Err(e) => return Err(e),
                }
            }
            Ok(results)
        })
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
        let sources: Vec<Arc<dyn Document>> =
            self.lock_state().sources.values().cloned().collect();
        Pending::spawn(async move {
            let mut all = Vec::new();
            for source in sources {
                match source.undo().await {
                    Ok(mut rs) => all.append(&mut rs),
                    Err(_) => continue,
                }
            }
            Ok(all)
        })
    }

    fn redo(&self) -> Pending<Vec<AppliedEdit>> {
        let sources: Vec<Arc<dyn Document>> =
            self.lock_state().sources.values().cloned().collect();
        Pending::spawn(async move {
            let mut all = Vec::new();
            for source in sources {
                match source.redo().await {
                    Ok(mut rs) => all.append(&mut rs),
                    Err(_) => continue,
                }
            }
            Ok(all)
        })
    }

    fn save(&self) -> Pending<std::path::PathBuf> {
        // Multibuffers aren't on-disk files; `:w` is a no-op
        // until a provider attaches save semantics (e.g.
        // M.6 SearchProvider's "save all sources" wrapper).
        Pending::ready(Err(RuntimeError::ReadOnly))
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
            compose_snapshot(
                self.inner.id,
                &state.sources,
                &state.excerpts,
                selections,
            )
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
        let registry = Arc::clone(&self.inner.registry);
        let result = lattice_grammar::execute(
            &registry,
            &mut scratch,
            buffer_id,
            cursor,
            invocation,
            &cancel,
        )
        .map_err(RuntimeError::Grammar);
        Pending::ready(result)
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

/// 2026-06-02 cursor-jump fix: translate a source-coord
/// [`AppliedEdit`] back to composed coords by shifting every
/// `Position`'s `.line` by `row_delta` (composed_row -
/// source_row at the excerpt's start). The host's insert path
/// (`dispatch.rs:5422`) sets the cursor from
/// `applied.inserted_range.end`; without this translation the
/// cursor jumps to source row N (e.g. 429 for an excerpt
/// pointing at line 429 of foo.rs) instead of staying at
/// composed row M (e.g. 0 — the first composed row of the
/// multibuffer view).
///
/// The byte fields (`start_byte` / `old_end_byte` /
/// `new_end_byte`) are tree-sitter incremental-edit hints
/// keyed to the SOURCE rope's byte axis. The multibuffer
/// doesn't run its own tree-sitter parse against the composed
/// rope (composed Lang is `Plain`), so no downstream consumer
/// needs these in composed-byte coords. Leaving them as
/// source-byte values: inert for the multibuffer's consumers,
/// still correct for any subsystem that joins back through
/// the source handle.
fn translate_applied_to_composed(applied: AppliedEdit, row_delta: i64) -> AppliedEdit {
    let shift = |p: Position| -> Position {
        let line = (p.line as i64 + row_delta).max(0) as u32;
        Position { line, byte: p.byte }
    };
    let shift_range = |r: lattice_protocol::position::Range| {
        lattice_protocol::position::Range {
            start: shift(r.start),
            end: shift(r.end),
        }
    };
    AppliedEdit {
        original_range: shift_range(applied.original_range),
        inserted_range: shift_range(applied.inserted_range),
        replaced_text: applied.replaced_text,
        inserted_text: applied.inserted_text,
        delta: lattice_protocol::edit::EditDelta {
            start_byte: applied.delta.start_byte,
            old_end_byte: applied.delta.old_end_byte,
            new_end_byte: applied.delta.new_end_byte,
            start_position: shift(applied.delta.start_position),
            old_end_position: shift(applied.delta.old_end_position),
            new_end_position: shift(applied.delta.new_end_position),
        },
    }
}


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
const MULTIBUFFER_HEADER_NAMESPACE: u64 = 0xBBBB_0001_0000_0000;

pub fn multibuffer_header_provider_id(buffer_id: BufferId) -> ProviderId {
    MULTIBUFFER_HEADER_NAMESPACE | u64::from(buffer_id.0)
}

/// Emits one virtual row per excerpt header, anchored above the
/// excerpt's first composed row. Cheap-clone reference to the
/// multibuffer handle; re-reads excerpts on each `collect()`.
#[derive(Debug)]
pub struct MultibufferHeaderProvider {
    multibuffer: MultibufferDocumentHandle,
}

impl MultibufferHeaderProvider {
    pub fn new(multibuffer: MultibufferDocumentHandle) -> Self {
        Self { multibuffer }
    }
}

impl VirtualRowProvider for MultibufferHeaderProvider {
    fn id(&self) -> ProviderId {
        multibuffer_header_provider_id(self.multibuffer.buffer_id())
    }

    fn version(&self) -> u64 {
        self.multibuffer.snapshot().version
    }

    fn collect(&self) -> Vec<VirtualRow> {
        compose_header_rows(&self.multibuffer.excerpts(), default_header_cells)
    }
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
            });
            last_source = Some(excerpt.source);
        }
        composed_cursor = composed_cursor.saturating_add(excerpt.line_count());
    }
    rows
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

    fn empty_registry() -> Arc<CommandRegistry> {
        Arc::new(CommandRegistry::new())
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
        assert_eq!(snap.dirty, false);
        assert!(snap.path.is_none());
        assert_eq!(snap.selections.all().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn multi_source_multi_excerpt_composes_in_order() {
        let (sources, ids) = make_sources(&["a1\na2\na3\n", "b1\nb2\nb3\n"]);
        let excerpts = vec![
            Excerpt::new(ids[0], 0, 1),
            Excerpt::new(ids[1], 2, 2),
        ];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        let snap = mb.snapshot();
        assert_eq!(snap.buffer.as_string(), "a1\na2\nb3\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn save_still_rejected_post_m3() {
        // M.3 (2026-06-01): apply_edit / undo / redo now
        // propagate. K.4.5 (2026-06-02): set_selections now
        // stores composed-coordinate selections (see
        // `set_selections_stores_composed_selections_post_k_4_5`).
        // save / save_as / dispatch_with_cancel still stay
        // rejected per the design comments in `impl Document`
        // (`:w` is no-op until a provider attaches save
        // semantics; grammar dispatch runs at the host layer).
        let (sources, ids) = make_sources(&["x"]);
        let excerpts = vec![Excerpt::new(ids[0], 0, 0)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        assert!(matches!(mb.save().await, Err(RuntimeError::ReadOnly)));
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
        let (sources, ids) = make_sources(&["alpha\nbeta\ngamma\n"]);
        let source_handle = sources.get(&ids[0]).expect("source present").clone();
        let excerpts = vec![Excerpt::new(ids[0], 0, 2)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        // Insert "X-" at composed position (line=1, byte=0) — should land at
        // source position (line=1, byte=0) since the excerpt starts at line 0.
        let applied = mb
            .apply_edit(Edit::insert(Position::new(1, 0), "X-"))
            .await
            .expect("insert should propagate");
        assert_eq!(applied.inserted_text, "X-");

        // Source reflects after recompose.
        mb.recompose();
        assert_eq!(mb.snapshot().buffer.as_string(), "alpha\nX-beta\ngamma\n");
        // Direct read of the source confirms the edit landed there
        // (not just in some multibuffer-local cache).
        assert_eq!(source_handle.text(), "alpha\nX-beta\ngamma\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m3_insert_translates_when_excerpt_starts_off_zero() {
        // Excerpt starts at source row 2; composed row 0 maps to
        // source row 2.
        let (sources, ids) = make_sources(&["zero\none\ntwo\nthree\nfour\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 2, 4)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        assert_eq!(mb.snapshot().buffer.as_string(), "two\nthree\nfour\n");

        // Insert "Z " at composed (0, 0) → source (2, 0).
        mb.apply_edit(Edit::insert(Position::new(0, 0), "Z "))
            .await
            .expect("insert should propagate");
        mb.recompose();
        assert_eq!(mb.snapshot().buffer.as_string(), "Z two\nthree\nfour\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m3_delete_within_excerpt_translates() {
        let (sources, ids) = make_sources(&["alpha\nbeta\ngamma\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 0, 2)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        // Delete "beta\n" — composed range (1,0)..(2,0).
        use lattice_protocol::position::Range;
        let _ = mb
            .apply_edit(Edit::delete(Range::new(
                Position::new(1, 0),
                Position::new(2, 0),
            )))
            .await
            .expect("delete should propagate");
        mb.recompose();
        assert_eq!(mb.snapshot().buffer.as_string(), "alpha\ngamma\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m3_out_of_range_edit_returns_read_only() {
        let (sources, ids) = make_sources(&["a\nb\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 0, 1)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        // Composed row 50 is way past the view's last row.
        assert!(matches!(
            mb.apply_edit(Edit::insert(Position::new(50, 0), "x")).await,
            Err(RuntimeError::ReadOnly)
        ));
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
        assert_eq!(b_handle.text(), original_b_text, "B source must be untouched");
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
        mb.recompose();
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
        let (sources, ids) = make_sources(&[
            "0\n1\n2\n3\n4\n5\n6\n7\n8\n9\nA\nB\n",
        ]);
        let excerpts = vec![
            Excerpt::new(ids[0], 5, 5),
            Excerpt::new(ids[0], 10, 10),
        ];
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
        });
        match &*mb.headerline() {
            HeaderlineStatus::InProgress { label, count } => {
                assert_eq!(label, "Searching");
                assert_eq!(*count, Some(42));
            }
            other => panic!("expected InProgress, got {other:?}"),
        }

        mb.set_headerline(HeaderlineStatus::Complete {
            summary: "87 hits".into(),
        });
        match &*mb.headerline() {
            HeaderlineStatus::Complete { summary } => assert_eq!(summary, "87 hits"),
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
        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<MultibufferSourceClosed>();
        bus.subscribe_typed::<MultibufferSourceClosed>(tx);

        let (sources, ids) = make_sources(&["a\n", "b\n"]);
        let source_a_handle = sources.get(&ids[0]).unwrap().clone();
        let excerpts = vec![
            Excerpt::new(ids[0], 0, 0),
            Excerpt::new(ids[1], 0, 0),
        ];
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
        assert_eq!(excerpts_after[0].start_line, 4, "excerpt should slide to row 4");
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
        assert_eq!(excerpts_after[0].start_line, 2, "excerpt should slide up to row 2");
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
        });
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn m3_undo_fans_out_to_each_source() {
        let (sources, ids) = make_sources(&["aaa\n", "bbb\n"]);
        let a = sources.get(&ids[0]).unwrap().clone();
        let b = sources.get(&ids[1]).unwrap().clone();
        let excerpts = vec![
            Excerpt::new(ids[0], 0, 0),
            Excerpt::new(ids[1], 0, 0),
        ];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();

        // Edit each source directly so each has something to undo.
        a.apply_edit(Edit::insert(Position::new(0, 0), "A!")).await.unwrap();
        b.apply_edit(Edit::insert(Position::new(0, 0), "B!")).await.unwrap();
        assert_eq!(a.text(), "A!aaa\n");
        assert_eq!(b.text(), "B!bbb\n");

        // Undo on the multibuffer fans out — both sources roll back.
        let applied = mb.undo().await.expect("undo ok");
        assert!(
            !applied.is_empty(),
            "fan-out undo should produce at least one AppliedEdit"
        );
        assert_eq!(a.text(), "aaa\n");
        assert_eq!(b.text(), "bbb\n");
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
        let mb = MultibufferDocumentHandle::new(sources.clone(), Vec::new(), empty_registry()).unwrap();
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
        mb.append_excerpts(vec![
            Excerpt::new(ids[0], 0, 0),
            Excerpt::new(bogus, 0, 0),
        ]);
        assert_eq!(
            mb.excerpt_count(),
            1,
            "unknown-source excerpt should be silently dropped",
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
        let with_title = Excerpt::new(mb_source, 0, 0)
            .with_header(ExcerptHeader::new("hi"));
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
        let provider = MultibufferHeaderProvider::new(mb);
        let rows = provider.collect();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].anchor_line, 0);
    }

    #[test]
    fn provider_id_namespace_is_stable() {
        let buffer_id = BufferId(42);
        let id = multibuffer_header_provider_id(buffer_id);
        assert_eq!(id & 0xFFFF_FFFF, 42);
        assert!(id < 0xD1FF_0000_0000_0000 || id >= 0xD200_0000_0000_0000);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn provider_version_bumps_with_recompose() {
        let (sources, ids) = make_sources(&["alpha\nbeta\n"]);
        let source = sources.get(&ids[0]).unwrap().clone();
        let excerpts = vec![Excerpt::new(ids[0], 0, 0)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts, empty_registry()).unwrap();
        let provider = MultibufferHeaderProvider::new(mb.clone());

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
}
