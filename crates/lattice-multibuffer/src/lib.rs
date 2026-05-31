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
use lattice_grammar::{CancellationToken, CommandInvocation, Effect};
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
    sources: HashMap<BufferId, Arc<dyn Document>>,
    excerpts: Vec<Excerpt>,
    snapshot_cell: Arc<PublishedSnapshot>,
    row_translation: ArcSwap<RowTranslation>,
}

/// A multibuffer document handle. Composes N source
/// `Arc<dyn Document>`s into one read-only composed view; impls
/// [`Document`] so dispatch / motion / render code paths serve
/// it the same as a regular `RopeDocumentHandle`.
#[derive(Clone)]
pub struct MultibufferDocumentHandle {
    inner: Arc<MultibufferInner>,
}

impl MultibufferDocumentHandle {
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

    pub fn buffer_id(&self) -> BufferId {
        self.inner.buffer_id
    }

    pub fn row_translation(&self) -> Arc<RowTranslation> {
        self.inner.row_translation.load_full()
    }

    pub fn excerpts(&self) -> &[Excerpt] {
        &self.inner.excerpts
    }

    pub fn source_buffer_ids(&self) -> impl Iterator<Item = BufferId> + '_ {
        self.inner.sources.keys().copied()
    }

    /// Recompose the snapshot from current source state.
    /// Rebuilds the composed buffer + row translation, then
    /// publishes via `ArcSwap::store`.
    ///
    /// M.1 ships this as a manual API; M.4 wires automatic
    /// invocation via source-edit event subscriptions.
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

#[derive(Debug, thiserror::Error)]
pub enum MultibufferError {
    #[error("multibuffer must have at least one excerpt")]
    EmptyExcerpts,
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
        compose_header_rows(self.multibuffer.excerpts(), default_header_cells)
    }
}

/// Pure function from excerpt list → header virtual rows. Each
/// excerpt contributes one row, anchored `Above` its first
/// composed line.
pub fn compose_header_rows(
    excerpts: &[Excerpt],
    mut render_cells: impl FnMut(&Excerpt) -> Arc<[Cell]>,
) -> Vec<VirtualRow> {
    let mut rows = Vec::with_capacity(excerpts.len());
    let mut composed_cursor: u32 = 0;
    for excerpt in excerpts {
        let cells = render_cells(excerpt);
        rows.push(VirtualRow {
            anchor_line: composed_cursor,
            position: AnchorPosition::Above,
            cells,
            height: 1,
            kind: VirtualRowKind::Generic,
        });
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
        let mb = MultibufferDocumentHandle::new(sources, excerpts).unwrap();
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
        let mb = MultibufferDocumentHandle::new(sources, excerpts).unwrap();
        let snap = mb.snapshot();
        assert_eq!(snap.buffer.as_string(), "a1\na2\nb3\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn writes_are_rejected() {
        let (sources, ids) = make_sources(&["x"]);
        let excerpts = vec![Excerpt::new(ids[0], 0, 0)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts).unwrap();

        assert!(matches!(
            mb.apply_edit(Edit::insert(Position::ZERO, "y")).await,
            Err(RuntimeError::ReadOnly)
        ));
        assert!(matches!(mb.undo().await, Err(RuntimeError::ReadOnly)));
        assert!(matches!(mb.save().await, Err(RuntimeError::ReadOnly)));
        assert!(matches!(
            mb.set_selections(SelectionSet::default()).await,
            Err(RuntimeError::ReadOnly)
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_excerpts_returns_error() {
        let (sources, _ids) = make_sources(&["x"]);
        let err = MultibufferDocumentHandle::new(sources, Vec::new()).unwrap_err();
        assert!(matches!(err, MultibufferError::EmptyExcerpts));
    }

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

    #[tokio::test(flavor = "multi_thread")]
    async fn source_edit_propagates_after_recompose() {
        let (sources, ids) = make_sources(&["alpha\nbeta\ngamma\n"]);
        let source_handle = sources.get(&ids[0]).expect("source present").clone();
        let excerpts = vec![Excerpt::new(ids[0], 0, 2)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts).unwrap();
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
    fn header_rows_anchor_at_each_excerpts_first_composed_row() {
        let mb_source = BufferId::next();
        let excerpts = vec![
            Excerpt::new(mb_source, 0, 2).with_header(ExcerptHeader::new("a")),
            Excerpt::new(mb_source, 0, 1).with_header(ExcerptHeader::new("b")),
            Excerpt::new(mb_source, 0, 0).with_header(ExcerptHeader::new("c")),
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
    async fn provider_collects_one_row_per_excerpt() {
        let (sources, ids) = make_sources(&["alpha\nbeta\ngamma\n"]);
        let excerpts = vec![
            Excerpt::new(ids[0], 0, 1).with_header(ExcerptHeader::new("first")),
            Excerpt::new(ids[0], 2, 2).with_header(ExcerptHeader::new("second")),
        ];
        let mb = MultibufferDocumentHandle::new(sources, excerpts).unwrap();
        let provider = MultibufferHeaderProvider::new(mb);
        let rows = provider.collect();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].anchor_line, 0);
        assert_eq!(rows[1].anchor_line, 2);
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
        let mb = MultibufferDocumentHandle::new(sources, excerpts).unwrap();
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
