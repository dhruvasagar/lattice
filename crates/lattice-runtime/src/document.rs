//! M.0 (2026-05-31): `Document` trait — the handle-layer
//! abstraction over a buffer that the rest of the editor talks
//! to.
//!
//! Today's `RopeDocumentHandle` (a rope-backed actor handle) impls
//! this trait; M.1 lands `MultibufferDocumentHandle` as a sibling
//! impl composing N source handles. Dispatch / motion / render
//! code paths hold `Arc<dyn Document>` so they serve both kinds
//! without kind-branching at the buffer boundary.
//!
//! See `docs/dev/architecture/multibuffer-views.md` §3.1 for the
//! design and the Path-A/B/C alternatives that were considered
//! and rejected.
//!
//! ## Slot replacement is the only path
//!
//! There is intentionally no `replace(...)` method on this trait.
//! Per the M.0 design (§3.1 "Why no `replace`"), "the active
//! document changes" is expressed by replacing the slot's
//! `Arc<dyn Document>` with a freshly spawned handle, not by
//! mutating the existing handle in place. The old handle drops
//! when its last Arc reference goes away; the actor task exits
//! cleanly through its mailbox-close branch. One uniform code
//! path for `:edit foo` / `:edit bar` / regular ↔ multibuffer
//! transitions / `:b N` switches.
//!
//! ## Object safety
//!
//! Trait is `dyn`-safe: `Arc<dyn Document>` is the canonical
//! reference shape. All methods take `&self` (writes return
//! `Pending<T>` which round-trips the impl's internal write
//! path — actor mpsc for `RopeDocumentHandle`, fan-out for
//! `MultibufferDocumentHandle`).
//!
//! Most read methods carry default implementations derived from
//! `snapshot()` so impls only need to provide `snapshot()` plus
//! the write methods. `MultibufferDocumentHandle` may override
//! `text()` if it can compose more cheaply than `snapshot()
//! .text()`.

use std::path::PathBuf;
use std::sync::Arc;

use lattice_grammar::{CancellationToken, CommandInvocation, Effect};
use lattice_protocol::edit::Edit;
use lattice_protocol::ids::DocumentId;
use lattice_protocol::position::Position;
use lattice_protocol::selection::SelectionSet;

use crate::actor::AppliedEdit;
use crate::handle::RopeDocumentHandle;
use crate::pending::Pending;
use crate::snapshot::{DocumentSnapshot, SnapshotCache};

/// Handle-layer abstraction over a buffer. See module docs.
///
/// `Debug` is a supertrait so containers holding `dyn Document`
/// (e.g., `lattice_host::buffer_registry::DocumentEntry`) can
/// derive `Debug` without hand-rolling per-field formatters.
pub trait Document: Send + Sync + 'static + std::fmt::Debug {
    // ---- Reads (wait-free, snapshot-backed) ----

    /// Load the current snapshot. The returned `Arc` lives as
    /// long as the caller needs it; subsequent publishes don't
    /// invalidate it.
    fn snapshot(&self) -> Arc<DocumentSnapshot>;

    /// Per-thread cache for hot loops that load the snapshot
    /// many times between edits. The cache reduces per-load cost
    /// from ~17 ns to ~2 ns when the writer hasn't published
    /// since the last load.
    fn snapshot_cache(&self) -> SnapshotCache;

    fn id(&self) -> DocumentId {
        self.snapshot().id
    }

    /// Rendered text. Allocates — prefer `snapshot().buffer
    /// .as_string()` on a held snapshot when looping.
    fn text(&self) -> String {
        self.snapshot().text()
    }

    fn path(&self) -> Option<PathBuf> {
        self.snapshot().path.as_ref().map(|a| (**a).clone())
    }

    fn dirty(&self) -> bool {
        self.snapshot().dirty
    }

    fn version(&self) -> u64 {
        self.snapshot().version
    }

    fn text_version(&self) -> u64 {
        self.snapshot().text_version
    }

    fn selections(&self) -> Arc<SelectionSet> {
        self.snapshot().selections.clone()
    }

    // ---- Writes (enqueue + Pending) ----

    fn apply_edit(&self, edit: Edit) -> Pending<AppliedEdit>;

    fn apply_edit_batch(&self, edits: Vec<Edit>) -> Pending<Vec<AppliedEdit>>;

    fn undo(&self) -> Pending<Vec<AppliedEdit>>;

    fn redo(&self) -> Pending<Vec<AppliedEdit>>;

    fn save(&self) -> Pending<PathBuf>;

    fn save_as(&self, path: PathBuf) -> Pending<()>;

    fn set_selections(&self, selections: SelectionSet) -> Pending<()>;

    // ---- Grammar dispatch ----

    /// Dispatch a [`CommandInvocation`] through the impl's
    /// internal grammar execution path. `MultibufferDocument
    /// Handle` (M.1) routes the invocation through its
    /// row-translation table to the underlying source(s).
    fn dispatch_with_cancel(
        &self,
        invocation: CommandInvocation,
        cursor: Position,
        cancel: CancellationToken,
    ) -> Pending<Effect>;

    /// Convenience: dispatch with a never-cancelled token.
    fn dispatch(&self, invocation: CommandInvocation, cursor: Position) -> Pending<Effect> {
        self.dispatch_with_cancel(invocation, cursor, CancellationToken::never())
    }

    /// K.4.6 follow-up (2026-06-02): per-composed-row source line
    /// number lookup for the gutter. `None` (default) = identity:
    /// the composed-row index IS the source line number, so the
    /// gutter formats `composed_row` directly. `Some(arr)` =
    /// composed→source map: `arr[composed_row]` gives the source
    /// line number to display.
    ///
    /// Regular `RopeDocumentHandle` keeps the default None impl —
    /// for a single-file buffer, composed_row == source_row.
    /// `MultibufferDocumentHandle` (lattice-multibuffer) overrides
    /// to return the flattened `RowTranslation` so the gutter
    /// shows the original file's line numbers (e.g. 429, 430,
    /// 432 — skipping non-hit rows) rather than the meaningless
    /// composed indices (0, 1, 2).
    ///
    /// Substrate-aligned per [[feedback_buffers_no_special_case]]:
    /// the publisher reads `self.document.display_line_numbers()`
    /// uniformly across all BufferKinds; no renderer-side or
    /// publish-side kind branch needed.
    fn display_line_numbers(&self) -> Option<Arc<[u32]>> {
        None
    }

    /// K.4.7: per-excerpt highlight entries for multibuffer panes.
    /// Default returns empty — regular single-file documents carry
    /// no excerpt structure.  `MultibufferDocumentHandle` overrides
    /// to return one entry per excerpt that has a `SyntaxHandle`.
    ///
    /// The cells worker calls this uniformly on every pane's document;
    /// no `BufferKind` branch needed in `publish_render_state`.
    fn excerpt_highlights(&self) -> Vec<lattice_cells::ExcerptHighlight> {
        Vec::new()
    }

    /// Monotonic counter that bumps whenever the excerpt highlight set
    /// changes (new sources added, lang registry wired).  Default: 0.
    fn excerpt_syntax_version(&self) -> u64 {
        0
    }
}

/// M.0 (2026-05-31): the active-document slot held by
/// `Editor.document`. Wraps an `Arc<dyn Document>` so that:
///
/// * The slot can hold either a regular rope-backed handle
///   (today: `RopeDocumentHandle`, renamed to `RopeDocumentHandle`
///   in M.0 Phase E) or a `MultibufferDocumentHandle` (M.1)
///   without kind-branching at the use site — dispatch /
///   motion / render code paths just call `Document` trait
///   methods through `Deref<Target = dyn Document>`.
///
/// * The slot impls `Default` (initialised to a placeholder
///   rope handle via `RopeDocumentHandle::default()`) so consumers
///   that `#[derive(Default)]` over a struct containing this
///   field work without hand-rolling the impl. The
///   placeholder's actor receiver is closed immediately at
///   construction, so any traffic sent through the placeholder
///   reports `RuntimeError::ActorGone` — production code
///   overwrites the slot before any real traffic flows.
///
/// * The newtype is cheap to clone (one atomic refcount bump
///   on the inner `Arc`).
///
/// `Arc<dyn Document>` directly cannot implement `Default`
/// (orphan rule on `Arc` + `dyn Trait: !Sized`), so this
/// newtype is the smallest viable wrapper that keeps the
/// derive ergonomics intact without forcing a manual
/// `impl Default` over every struct that holds the slot.
#[derive(Clone)]
pub struct ActiveDocument(Arc<dyn Document>);

impl ActiveDocument {
    /// Wrap a concrete document handle. The handle must impl
    /// `Document` (today: `RopeDocumentHandle` / future
    /// `RopeDocumentHandle`; M.1: `MultibufferDocumentHandle`).
    pub fn new<D: Document>(handle: D) -> Self {
        Self(Arc::new(handle))
    }

    /// Wrap an already-`Arc`'d handle. Useful when an
    /// `Arc<dyn Document>` is constructed elsewhere (e.g., by
    /// the buffer registry) and the slot just takes ownership
    /// of it.
    pub fn from_arc(arc: Arc<dyn Document>) -> Self {
        Self(arc)
    }

    /// Cheap clone of the inner `Arc`. Use when handing the
    /// reference off to a long-lived consumer that wants to
    /// store it independently.
    pub fn as_arc(&self) -> Arc<dyn Document> {
        Arc::clone(&self.0)
    }
}

impl Default for ActiveDocument {
    fn default() -> Self {
        Self(Arc::new(RopeDocumentHandle::default()))
    }
}

impl std::ops::Deref for ActiveDocument {
    type Target = dyn Document;

    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}

impl std::fmt::Debug for ActiveDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ActiveDocument")
            .field(&self.0.id())
            .finish()
    }
}
