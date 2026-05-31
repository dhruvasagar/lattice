//! M.1 (2026-05-31): `MultibufferDocumentHandle` — the
//! handle-layer sibling to `RopeDocumentHandle` that composes N
//! source documents into one read-only view.
//!
//! ## What this slice ships (M.1)
//!
//! * Core types: `ExcerptId`, `Excerpt`, and the basic geometry
//!   for "show source rows [start..=end] of buffer B in the
//!   composed view." Headers / separators / partial-line edges
//!   are M.2 rendering concerns; this slice keeps the composed
//!   buffer as pure concatenated source content.
//! * `MultibufferDocumentHandle` impl of [`crate::Document`].
//!   `snapshot()` returns a `DocumentSnapshot` whose `buffer`
//!   field is the composed rope of all excerpt content.
//! * Writes (`apply_edit`, `apply_edit_batch`, `undo`, `redo`,
//!   `save`, `save_as`, `set_selections`, `dispatch_with_cancel`)
//!   all reject with `Pending::ready(Err(RuntimeError::
//!   ReadOnly))`. M.3 replaces the rejections with translation-
//!   table-driven propagation to source handles.
//!
//! ## What lands in later M-slices
//!
//! * **M.2** — excerpt headers / separators as virtual rows; the
//!   composed buffer's text remains unchanged, headers render
//!   alongside via the existing virtual-rows pipeline.
//! * **M.3** — write propagation: edit at multibuffer row →
//!   translation lookup → source handle's `apply_edit`. Removes
//!   the `ReadOnly` rejection.
//! * **M.4** — live updates from sources: anchor-driven excerpt
//!   tracking + debounced rebuild + cross-pane consistency.
//!   M.1 ships with manual `recompose()` for tests; auto-rebuild
//!   on source change lands in M.4.
//! * **M.5** — expand-context affordance.
//! * **M.6** — `MultibufferProvider` trait + first consumer.
//!
//! ## Anchor semantics — deliberately deferred
//!
//! The design fragment §3.2 specifies excerpts use anchors with
//! generation tracking so they slide on source edits. M.1 ships
//! with simple `(start_line, end_line)` integer ranges; anchors
//! get introduced in M.4 alongside the slide-on-edits behaviour
//! they enable. Defining Anchor in M.1 with no slide logic would
//! be a placeholder promising semantics it doesn't have. The
//! Excerpt struct's field shape is preserved when M.4 swaps in
//! `Anchor` (the field names change but the shape — two
//! character-positions — doesn't).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use arc_swap::ArcSwap;
use lattice_core::buffer::AppliedEdit;
use lattice_core::{Buffer, BufferId};
use lattice_grammar::{CancellationToken, CommandInvocation, Effect};
use lattice_protocol::edit::Edit;
use lattice_protocol::ids::DocumentId;
use lattice_protocol::position::Position;
use lattice_protocol::selection::SelectionSet;

use crate::document::Document;
use crate::pending::{Pending, RuntimeError};
use crate::snapshot::{DocumentSnapshot, PublishedSnapshot, SnapshotCache};

/// Unique identity for an excerpt within a multibuffer. Stable
/// for the excerpt's lifetime; survives reorders / source-edit
/// rebuilds. M.6's `MultibufferProvider` uses this to match
/// re-emitted excerpts against existing ones (avoiding spurious
/// removal + re-addition when the provider's set is unchanged
/// modulo ordering).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExcerptId(pub u64);

impl ExcerptId {
    /// Allocate the next id. Lock-free.
    pub fn next() -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(1);
        Self(SEQ.fetch_add(1, Ordering::Relaxed))
    }
}

/// One excerpt of a source document, identified by its source
/// `BufferId` and an inclusive line range `[start_line, end_line]`.
///
/// M.1 keeps the range as integer line numbers; M.4 swaps to
/// `Anchor`-based positions that slide on source edits.
#[derive(Debug, Clone)]
pub struct Excerpt {
    pub id: ExcerptId,
    /// The source buffer this excerpt projects rows from.
    pub source: BufferId,
    /// First source row included in the composed view
    /// (0-indexed, inclusive).
    pub start_line: u32,
    /// Last source row included in the composed view
    /// (0-indexed, inclusive). When `start_line == end_line`,
    /// the excerpt is one row tall.
    pub end_line: u32,
}

impl Excerpt {
    /// Build an excerpt covering `[start_line..=end_line]` of
    /// `source`. Allocates a fresh `ExcerptId`.
    pub fn new(source: BufferId, start_line: u32, end_line: u32) -> Self {
        Self {
            id: ExcerptId::next(),
            source,
            start_line,
            end_line,
        }
    }

    /// Number of source rows this excerpt covers. Always
    /// `>= 1` for a well-formed excerpt.
    pub fn line_count(&self) -> u32 {
        self.end_line.saturating_sub(self.start_line) + 1
    }
}

/// One row in the composed multibuffer view, mapped back to its
/// source. M.2 (excerpt rendering) adds `Header` / `Separator`
/// variants for virtual rows; M.1 only emits `Excerpt` rows
/// because the composed buffer holds pure source content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowEntry {
    /// A row of source content. `excerpt_id` identifies which
    /// excerpt this row belongs to; `source_row` is the row
    /// number in the original source buffer.
    Excerpt {
        excerpt_id: ExcerptId,
        source_row: u32,
    },
}

/// Composed-row → source-row mapping. One entry per composed
/// row, in display order. Rebuilt fresh from the current excerpt
/// list on every recompose; M.3 (edit propagation) and M.2
/// (rendering) both lean on this to dispatch composed rows back
/// to their source.
///
/// M.1 doesn't render or dispatch edits, so the translation is
/// constructed but mostly unused; M.2 / M.3 wire it into their
/// hot paths.
#[derive(Debug, Clone, Default)]
pub struct RowTranslation {
    pub entries: Vec<RowEntry>,
}

impl RowTranslation {
    /// Build a fresh translation table from `excerpts` in order.
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

/// The owned state of one multibuffer. Held inside an `Arc` by
/// every clone of [`MultibufferDocumentHandle`].
struct MultibufferInner {
    /// Stable identity for the composed buffer. Distinct from
    /// any source's `DocumentId`. Allocated from a high
    /// counter range so it can't collide with `DocumentId`s
    /// minted by `lattice_core::Document::next_document_id()`.
    id: DocumentId,
    /// Per-process unique buffer id, parallel to what a regular
    /// `RopeDocumentHandle` registers under in `BufferRegistry`.
    /// Exposed via [`MultibufferDocumentHandle::buffer_id`] —
    /// callers that need to register the handle in a registry
    /// keyed by `BufferId` use this.
    buffer_id: BufferId,
    /// Source handles indexed by `BufferId`. An excerpt's
    /// `source` field is looked up here. M.1 takes the map at
    /// construction; M.4 will extend it via subscription.
    sources: HashMap<BufferId, Arc<dyn Document>>,
    /// Excerpts in composed-display order. M.1 stores them as
    /// a frozen `Vec`; M.6 will rebuild this from a
    /// `MultibufferProvider`.
    excerpts: Vec<Excerpt>,
    /// Composed snapshot publish cell — readers `load()` from
    /// this to get the current state; `recompose()` builds a
    /// new snapshot and `store()`s it.
    snapshot_cell: Arc<PublishedSnapshot>,
    /// Cached row-translation table. Rebuilt alongside the
    /// snapshot on every recompose. Published via `ArcSwap` so
    /// downstream consumers (M.2 renderer / M.3 dispatch) get a
    /// lock-free load.
    row_translation: ArcSwap<RowTranslation>,
}

/// M.1 (2026-05-31): a multibuffer document handle. Composes N
/// source `Arc<dyn Document>`s into one read-only composed
/// view; impls [`Document`] so dispatch / motion / render code
/// paths serve it the same as a regular `RopeDocumentHandle`.
///
/// Cheap to clone (one atomic Arc bump). Lives in the
/// `Editor.document` slot (via `ActiveDocument::new`) or in
/// `BufferRegistry::DocumentEntry::handle` (via direct `Arc`).
///
/// Writes are rejected with `RuntimeError::ReadOnly` until M.3
/// lands translation-table-driven propagation to source handles.
#[derive(Clone)]
pub struct MultibufferDocumentHandle {
    inner: Arc<MultibufferInner>,
}

impl MultibufferDocumentHandle {
    /// Build a multibuffer from a fixed map of source handles
    /// (indexed by `BufferId`) and an ordered list of
    /// excerpts. Composes the initial snapshot eagerly. M.4
    /// will add an auto-rebuild path that subscribes to source
    /// edit events; for M.1 callers invoke [`Self::recompose`]
    /// manually after mutating sources.
    ///
    /// Errors:
    /// * [`MultibufferError::EmptyExcerpts`] if `excerpts` is
    ///   empty — a multibuffer with no excerpts is degenerate.
    /// * [`MultibufferError::UnknownSource`] if any excerpt
    ///   references a `BufferId` not present in `sources`.
    pub fn new(
        sources: HashMap<BufferId, Arc<dyn Document>>,
        excerpts: Vec<Excerpt>,
    ) -> Result<Self, MultibufferError> {
        if excerpts.is_empty() {
            return Err(MultibufferError::EmptyExcerpts);
        }
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
        let composed = compose_snapshot(id, &sources, &excerpts);
        let snapshot_cell = Arc::new(PublishedSnapshot::new(composed));

        Ok(Self {
            inner: Arc::new(MultibufferInner {
                id,
                buffer_id,
                sources,
                excerpts,
                snapshot_cell,
                row_translation: ArcSwap::from_pointee(row_translation),
            }),
        })
    }

    /// Per-process unique buffer id for registry use. Stable
    /// across the multibuffer's lifetime.
    pub fn buffer_id(&self) -> BufferId {
        self.inner.buffer_id
    }

    /// Current row-translation table snapshot. Lock-free.
    /// Renderer + edit-dispatch use this to map composed rows
    /// back to source rows.
    pub fn row_translation(&self) -> Arc<RowTranslation> {
        self.inner.row_translation.load_full()
    }

    /// Read-side accessor for the underlying excerpt list.
    /// Returned by reference into the `Arc<MultibufferInner>`
    /// so callers don't pay an allocation. M.1 holds the
    /// excerpt list immutably; M.6 swaps it via `ArcSwap` when
    /// providers re-emit.
    pub fn excerpts(&self) -> &[Excerpt] {
        &self.inner.excerpts
    }

    /// Source `BufferId`s this multibuffer composes from, in
    /// no particular order. M.4 / M.5 / M.6 lean on this to
    /// wire subscriptions / propagate updates / handle source
    /// removal.
    pub fn source_buffer_ids(&self) -> impl Iterator<Item = BufferId> + '_ {
        self.inner.sources.keys().copied()
    }

    /// Recompose the snapshot from current source state.
    /// Rebuilds the composed buffer + row translation, then
    /// publishes the new snapshot via `ArcSwap::store`.
    ///
    /// M.1 ships with this as a manual API (test fixtures call
    /// it explicitly after mutating sources). M.4 wires
    /// automatic invocation via source-edit event
    /// subscriptions.
    pub fn recompose(&self) {
        let new_snapshot = compose_snapshot(
            self.inner.id,
            &self.inner.sources,
            &self.inner.excerpts,
        );
        let new_translation = RowTranslation::build(&self.inner.excerpts);
        self.inner.snapshot_cell.store(new_snapshot);
        self.inner
            .row_translation
            .store(Arc::new(new_translation));
    }
}

impl Document for MultibufferDocumentHandle {
    fn snapshot(&self) -> Arc<DocumentSnapshot> {
        self.inner.snapshot_cell.load()
    }

    fn snapshot_cache(&self) -> SnapshotCache {
        SnapshotCache::new(self.inner.snapshot_cell.clone())
    }

    fn apply_edit(&self, _edit: Edit) -> Pending<AppliedEdit> {
        Pending::ready(Err(RuntimeError::ReadOnly))
    }

    fn apply_edit_batch(&self, _edits: Vec<Edit>) -> Pending<Vec<AppliedEdit>> {
        Pending::ready(Err(RuntimeError::ReadOnly))
    }

    fn undo(&self) -> Pending<Vec<AppliedEdit>> {
        Pending::ready(Err(RuntimeError::ReadOnly))
    }

    fn redo(&self) -> Pending<Vec<AppliedEdit>> {
        Pending::ready(Err(RuntimeError::ReadOnly))
    }

    fn save(&self) -> Pending<std::path::PathBuf> {
        Pending::ready(Err(RuntimeError::ReadOnly))
    }

    fn save_as(&self, _path: std::path::PathBuf) -> Pending<()> {
        Pending::ready(Err(RuntimeError::ReadOnly))
    }

    fn set_selections(&self, _selections: SelectionSet) -> Pending<()> {
        Pending::ready(Err(RuntimeError::ReadOnly))
    }

    fn dispatch_with_cancel(
        &self,
        _invocation: CommandInvocation,
        _cursor: Position,
        _cancel: CancellationToken,
    ) -> Pending<Effect> {
        Pending::ready(Err(RuntimeError::ReadOnly))
    }
}

impl std::fmt::Debug for MultibufferDocumentHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MultibufferDocumentHandle")
            .field("id", &self.inner.id)
            .field("buffer_id", &self.inner.buffer_id)
            .field("sources", &self.inner.sources.len())
            .field("excerpts", &self.inner.excerpts.len())
            .finish()
    }
}

/// Errors surfaced by multibuffer construction.
#[derive(Debug, thiserror::Error)]
pub enum MultibufferError {
    /// Construction was passed an empty excerpt list. A
    /// multibuffer needs at least one excerpt to be a valid
    /// composed view.
    #[error("multibuffer must have at least one excerpt")]
    EmptyExcerpts,
    /// An excerpt references a source `BufferId` not present in
    /// the `sources` map at construction time. (`source_buffer`
    /// is named to avoid `thiserror`'s magic `source` field
    /// behaviour, which would require `BufferId: std::error::
    /// Error`.)
    #[error("excerpt {excerpt:?} references unknown source buffer {source_buffer:?}")]
    UnknownSource {
        excerpt: ExcerptId,
        source_buffer: BufferId,
    },
}

// ─────────────────────────────────────────────────────────────────
// Internals
// ─────────────────────────────────────────────────────────────────

fn next_multibuffer_document_id() -> DocumentId {
    // Multibuffer ids live in a high range that won't collide
    // with regular per-process `DocumentId`s minted by
    // `lattice_core::Document::next_document_id()` (which
    // starts at 1).
    static NEXT: AtomicU64 = AtomicU64::new(0x1000_0000_0000_0000);
    DocumentId::new(NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Compose the snapshot for the multibuffer from current source
/// snapshots. Builds the composed rope text by concatenating
/// each excerpt's source-row range in order; one trailing
/// newline per source row keeps the line geometry stable.
///
/// Orphaned excerpts (whose `source` is no longer in
/// `sources`) are silently skipped — this is the v1 behaviour
/// for a closed source (per §8 decision); M.1.d wires
/// auto-pruning of the excerpt list when its source closes.
fn compose_snapshot(
    id: DocumentId,
    sources: &HashMap<BufferId, Arc<dyn Document>>,
    excerpts: &[Excerpt],
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
        selections: Arc::new(SelectionSet::default()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::handle::spawn_document;
    use lattice_core::Document as CoreDocument;
    use lattice_grammar::CommandRegistry;

    fn empty_registry() -> Arc<CommandRegistry> {
        Arc::new(CommandRegistry::new())
    }

    /// Build sources keyed by fresh `BufferId`s — returns
    /// `(sources_map, buffer_ids_in_order)` so tests can
    /// reference buffer ids when constructing excerpts.
    fn make_sources(texts: &[&str]) -> (HashMap<BufferId, Arc<dyn Document>>, Vec<BufferId>) {
        let mut map: HashMap<BufferId, Arc<dyn Document>> = HashMap::new();
        let mut ids = Vec::new();
        for text in texts {
            let handle = spawn_document(CoreDocument::from_text(*text), empty_registry());
            let id = BufferId::next();
            map.insert(id, Arc::new(handle));
            ids.push(id);
        }
        (map, ids)
    }

    /// Smoke test: build a multibuffer with one source + one
    /// excerpt; verify the composed snapshot exposes the
    /// excerpt's rows as its rope content.
    #[tokio::test(flavor = "multi_thread")]
    async fn single_source_single_excerpt_composes() {
        let (sources, ids) = make_sources(&["alpha\nbeta\ngamma\ndelta\nepsilon\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 1, 3)];

        let mb = MultibufferDocumentHandle::new(sources, excerpts).unwrap();
        let snap = mb.snapshot();
        assert_eq!(snap.buffer.as_string(), "beta\ngamma\ndelta\n");
        assert_eq!(snap.dirty, false);
        assert!(snap.path.is_none());
        // `SelectionSet::default()` is a single cursor at origin,
        // matching read-only "no selection" semantics.
        assert_eq!(snap.selections.all().len(), 1);
    }

    /// Two excerpts across two sources concatenate in display
    /// order (composed top-to-bottom matches excerpt vec order).
    #[tokio::test(flavor = "multi_thread")]
    async fn multi_source_multi_excerpt_composes_in_order() {
        let (sources, ids) = make_sources(&["a1\na2\na3\n", "b1\nb2\nb3\n"]);
        let excerpts = vec![
            Excerpt::new(ids[0], 0, 1),
            Excerpt::new(ids[1], 2, 2),
        ];

        let mb = MultibufferDocumentHandle::new(sources, excerpts).unwrap();
        let snap = mb.snapshot();
        assert_eq!(snap.buffer.as_string(), "a1\na2\nb3\n");
    }

    /// Write methods all return `Pending::ready(Err(ReadOnly))`.
    #[tokio::test(flavor = "multi_thread")]
    async fn writes_are_rejected() {
        let (sources, ids) = make_sources(&["x"]);
        let excerpts = vec![Excerpt::new(ids[0], 0, 0)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts).unwrap();

        let edit_result = mb.apply_edit(Edit::insert(Position::ZERO, "y")).await;
        assert!(matches!(edit_result, Err(RuntimeError::ReadOnly)));

        let undo_result = mb.undo().await;
        assert!(matches!(undo_result, Err(RuntimeError::ReadOnly)));

        let save_result = mb.save().await;
        assert!(matches!(save_result, Err(RuntimeError::ReadOnly)));

        let select_result = mb.set_selections(SelectionSet::default()).await;
        assert!(matches!(select_result, Err(RuntimeError::ReadOnly)));
    }

    /// Empty excerpt list at construction surfaces a typed error.
    #[tokio::test(flavor = "multi_thread")]
    async fn empty_excerpts_returns_error() {
        let (sources, _ids) = make_sources(&["x"]);
        let err = MultibufferDocumentHandle::new(sources, Vec::new()).unwrap_err();
        assert!(matches!(err, MultibufferError::EmptyExcerpts));
    }

    /// Excerpt referencing a source not in the map surfaces a
    /// typed error.
    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_source_returns_error() {
        let (sources, _ids) = make_sources(&["x"]);
        let bogus = BufferId(99_999);
        let excerpts = vec![Excerpt::new(bogus, 0, 0)];
        let err = MultibufferDocumentHandle::new(sources, excerpts).unwrap_err();
        assert!(matches!(
            err,
            MultibufferError::UnknownSource { source_buffer, .. } if source_buffer == bogus
        ));
    }

    /// Trait-object dispatch: `Arc<dyn Document>` callers see
    /// the same observable behaviour as the concrete handle.
    #[tokio::test(flavor = "multi_thread")]
    async fn dispatches_via_dyn_document() {
        let (sources, ids) = make_sources(&["foo\nbar\nbaz\n"]);
        let excerpts = vec![Excerpt::new(ids[0], 0, 1)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts).unwrap();

        let dyn_doc: Arc<dyn Document> = Arc::new(mb);
        assert_eq!(dyn_doc.text(), "foo\nbar\n");
        assert!(!dyn_doc.dirty());
        assert!(matches!(
            dyn_doc.apply_edit(Edit::insert(Position::ZERO, "x")).await,
            Err(RuntimeError::ReadOnly)
        ));
    }

    /// Source edit + manual `recompose()` reflects in the
    /// multibuffer's snapshot.
    #[tokio::test(flavor = "multi_thread")]
    async fn source_edit_propagates_after_recompose() {
        let (sources, ids) = make_sources(&["alpha\nbeta\ngamma\n"]);
        // Grab the concrete handle so we can drive the actor;
        // we still pass it through the trait-object map.
        let source_handle = sources.get(&ids[0]).expect("source present").clone();

        let excerpts = vec![Excerpt::new(ids[0], 0, 2)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts).unwrap();
        assert_eq!(mb.snapshot().text(), "alpha\nbeta\ngamma\n");

        // Mutate the source via the trait surface. End-of-line
        // 1 (after "beta") is the start of line 2 in 0-indexed
        // terms; the source rope has 3 lines + trailing newline.
        source_handle
            .apply_edit(Edit::insert(Position::new(1, 0), "BB-"))
            .await
            .unwrap();
        // Before recompose: stale.
        assert_eq!(mb.snapshot().text(), "alpha\nbeta\ngamma\n");
        // After recompose: fresh.
        mb.recompose();
        assert_eq!(mb.snapshot().text(), "alpha\nBB-beta\ngamma\n");
    }
}
