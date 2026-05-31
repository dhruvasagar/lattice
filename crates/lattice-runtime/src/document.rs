//! M.0 (2026-05-31): `Document` trait — the handle-layer
//! abstraction over a buffer that the rest of the editor talks
//! to.
//!
//! Today's `DocumentHandle` (a rope-backed actor handle) impls
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
use crate::pending::Pending;
use crate::snapshot::{DocumentSnapshot, SnapshotCache};

/// Handle-layer abstraction over a buffer. See module docs.
pub trait Document: Send + Sync + 'static {
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
}
