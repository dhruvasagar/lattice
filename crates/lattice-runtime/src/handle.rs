//! `RopeDocumentHandle` -- the public API for talking to a document
//! actor. Cheap to clone (an `mpsc::Sender` + an
//! `Arc<PublishedSnapshot>`); pass to any thread, hold for any
//! lifetime, give to plugins.
//!
//! ## Operations
//!
//! Mutating methods (`apply_edit`, `undo`, `redo`, `save`,
//! `save_as`, `set_selections`) all return [`Pending<T>`]. Per
//! DESIGN.md §5.2.1 the dispatcher MUST NOT block the caller --
//! so these never `await` on the actor; they enqueue and return.
//!
//! M.0 (2026-05-31): there is no `replace(...)` method. "The
//! active document changes" is expressed as slot replacement —
//! assign a fresh handle to the `Editor.document` slot; the old
//! handle drops and its actor task exits cleanly when no other
//! caller holds it. See `docs/dev/architecture/multibuffer-views
//! .md` §3.1 "Why no `replace`."
//!
//! Read methods ([`RopeDocumentHandle::snapshot`] and the convenience
//! pass-throughs `text`, `path`, `dirty`, `version`,
//! `text_version`, `selections`) are wait-free and return immediately
//! from the published snapshot. They never round-trip the actor.
//!
//! ## Mailbox semantics
//!
//! The mailbox is `tokio::sync::mpsc::unbounded_channel` (audit
//! slice 6 / H3). Mutating methods send synchronously; the only
//! failure mode is [`RuntimeError::ActorGone`], surfaced when
//! the actor task has terminated. The previous bounded-channel +
//! `RuntimeError::Busy` design dropped edits silently when the
//! App's `apply_edit_blocking` discarded the Busy variant under
//! bursts; the unbounded channel makes this class of bug
//! structurally impossible. Queue depth bounds itself by edit
//! rate × actor stall (typing is human-paced; a few KB at most).

use std::path::PathBuf;
use std::sync::Arc;

use lattice_core::Document;
use lattice_grammar::{CancellationToken, CommandInvocation, CommandRegistry, Effect};
use lattice_protocol::edit::Edit;
use lattice_protocol::ids::DocumentId;
use lattice_protocol::position::Position;
use lattice_protocol::selection::SelectionSet;
use tokio::sync::{mpsc, oneshot};

use crate::actor::{ActorMsg, AppliedEdit, DocumentActor};
use crate::pending::{InvocationId, Pending, RuntimeError};
use crate::runtime::shared_runtime;
use crate::snapshot::{DocumentSnapshot, PublishedSnapshot};

/// Cheap-clone handle to one document actor. All callers (App,
/// renderer, future LSP clients, plugins) talk to the actor through
/// a `RopeDocumentHandle` -- there is no other way to reach the
/// document's writable state.
#[derive(Clone)]
pub struct RopeDocumentHandle {
    /// See the module-level "Mailbox semantics" doc for the
    /// rationale behind the unbounded channel (audit slice 6 /
    /// H3).
    sender: mpsc::UnboundedSender<ActorMsg>,
    snapshot_cell: Arc<PublishedSnapshot>,
    /// M.2.b.0.A (2026-05-31): registry-level identity of this
    /// handle's buffer. Distinct from
    /// `DocumentSnapshot::id` (the per-actor `DocumentId`);
    /// `BufferId` is what `BufferRegistry` keys by and what
    /// `MotionContext::buffer_id` carries to kind-specific motion
    /// handlers. Stored on the handle so trait-method dispatch
    /// (`Document::dispatch_with_cancel`) can thread the id
    /// through `ActorMsg::Dispatch` without the caller needing
    /// to know it. The placeholder handle (`Default::default()`)
    /// uses `BufferId(0)` — placeholders never traffic real
    /// dispatch.
    buffer_id: lattice_core::BufferId,
}

/// Placeholder handle for `Editor::default()` headless / test
/// scaffolding. The receiver is dropped immediately, so any
/// message sent through this handle silently fails (the
/// caller's reply oneshot drops without being completed,
/// reported as "actor gone" by the typed wrappers).
/// Production handles come from
/// [`spawn_document`]; `Editor::new(...)` overwrites this
/// slot before any traffic flows.
impl Default for RopeDocumentHandle {
    fn default() -> Self {
        let (sender, _rx) = mpsc::unbounded_channel();
        Self {
            sender,
            snapshot_cell: Arc::new(PublishedSnapshot::new(DocumentSnapshot::default())),
            // Placeholder handle: any actual traffic will fail
            // with `RuntimeError::ActorGone` because the
            // receiver is already dropped. The buffer_id value
            // is observable only via `Document::buffer_id` in
            // tests; production code overwrites the slot before
            // any traffic flows.
            buffer_id: lattice_core::BufferId(0),
        }
    }
}

/// Spawn a fresh document actor on the shared runtime and return
/// the handle. Calling this is the *only* way to obtain a
/// `RopeDocumentHandle`. Document moves into the actor; once spawned
/// the document is reachable only through the handle.
///
/// `registry` is shared by `Arc` so the actor can run grammar
/// dispatches without coupling to the App's lifetime. Cloning the
/// `Arc` is one atomic increment.
///
/// The actor task survives until every clone of the returned
/// handle is dropped; on the last drop the mailbox closes, the
/// actor's `recv` loop exits, and the task returns.
pub fn spawn_document(
    buffer_id: lattice_core::BufferId,
    document: Document,
    registry: Arc<CommandRegistry>,
) -> RopeDocumentHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    let snapshot_cell = Arc::new(PublishedSnapshot::new(DocumentSnapshot::from_document(
        &document,
    )));
    let actor = DocumentActor::new(document, registry, rx, snapshot_cell.clone());
    shared_runtime().spawn(actor.run());
    RopeDocumentHandle {
        sender: tx,
        snapshot_cell,
        buffer_id,
    }
}

impl RopeDocumentHandle {
    // ---- Reads (wait-free, snapshot-backed) ----

    /// Load the current snapshot. The returned `Arc` lives as long
    /// as the caller needs it; the actor's subsequent publishes
    /// don't invalidate it. Renderers call this once per visible
    /// document per frame.
    ///
    /// Costs ~17ns (one atomic acquire-load + one Arc bump). For
    /// hot paths that read the snapshot many times between
    /// edits, prefer [`Self::snapshot_cache`] -- it caches the
    /// `Arc` per thread and brings the per-load cost to ~2ns.
    pub fn snapshot(&self) -> Arc<DocumentSnapshot> {
        self.snapshot_cell.load()
    }

    /// Build a per-thread cache for the snapshot read path.
    /// Each call returns a fresh [`crate::snapshot::SnapshotCache`]; threads that
    /// read the snapshot every frame should hold one of these on
    /// the stack of the read loop and call `load()` on it instead
    /// of going through [`Self::snapshot`].
    ///
    /// Wait-free thread-local-cached: when the writer hasn't
    /// changed the snapshot since the last load, the call is one
    /// `Relaxed` atomic compare and returns a borrowed `Arc`
    /// reference at no further cost (~2ns). When the writer has
    /// published, the next load reloads + caches the new `Arc`.
    pub fn snapshot_cache(&self) -> crate::snapshot::SnapshotCache {
        crate::snapshot::SnapshotCache::new(self.snapshot_cell.clone())
    }

    /// Convenience: id (stable for the actor's lifetime).
    pub fn id(&self) -> DocumentId {
        self.snapshot().id
    }

    /// Convenience: rendered text. Allocates -- prefer
    /// `snapshot().buffer.as_string()` on a held snapshot when
    /// looping.
    pub fn text(&self) -> String {
        self.snapshot().text()
    }

    /// Convenience: path (clone of the `Arc<PathBuf>` slice).
    pub fn path(&self) -> Option<PathBuf> {
        self.snapshot().path.as_ref().map(|a| (**a).clone())
    }

    /// Convenience: dirty flag.
    pub fn dirty(&self) -> bool {
        self.snapshot().dirty
    }

    pub fn version(&self) -> u64 {
        self.snapshot().version
    }

    pub fn text_version(&self) -> u64 {
        self.snapshot().text_version
    }

    pub fn selections(&self) -> Arc<SelectionSet> {
        self.snapshot().selections.clone()
    }

    // ---- Mutations (enqueue + Pending) ----

    /// Apply one edit. Resolves to the resulting `AppliedEdit`
    /// (range info + replaced text) once the actor commits.
    pub fn apply_edit(&self, edit: Edit) -> Pending<AppliedEdit> {
        self.send(|reply| ActorMsg::ApplyEdit { edit, reply })
    }

    /// Apply multiple edits as one undo unit.
    pub fn apply_edit_batch(&self, edits: Vec<Edit>) -> Pending<Vec<AppliedEdit>> {
        self.send(|reply| ActorMsg::ApplyEditBatch { edits, reply })
    }

    pub fn undo(&self) -> Pending<Vec<AppliedEdit>> {
        self.send(|reply| ActorMsg::Undo { reply })
    }

    pub fn redo(&self) -> Pending<Vec<AppliedEdit>> {
        self.send(|reply| ActorMsg::Redo { reply })
    }

    pub fn save(&self) -> Pending<PathBuf> {
        self.send(|reply| ActorMsg::Save { reply })
    }

    pub fn save_as(&self, path: PathBuf) -> Pending<()> {
        self.send(|reply| ActorMsg::SaveAs { path, reply })
    }

    pub fn set_selections(&self, selections: SelectionSet) -> Pending<()> {
        self.send(|reply| ActorMsg::SetSelections { selections, reply })
    }

    /// Dispatch a [`CommandInvocation`] through
    /// [`lattice_grammar::execute`] inside the actor. The actor
    /// holds the only `&mut Document` so all grammar-driven
    /// mutations route here. The returned `Effect` is for the App
    /// to apply to its session-scoped state (registers, modal
    /// transitions, marks, etc.).
    ///
    /// This form uses a no-op [`CancellationToken::never()`]; the
    /// dispatch will run to completion. Use
    /// [`Self::dispatch_with_cancel`] when the caller needs to
    /// cancel a long-running motion / operator on user Esc.
    pub fn dispatch(&self, invocation: CommandInvocation, cursor: Position) -> Pending<Effect> {
        self.dispatch_with_cancel(invocation, cursor, CancellationToken::never())
    }

    /// Like [`Self::dispatch`] but routes a caller-owned
    /// [`CancellationToken`] into the grammar `execute` call. The
    /// caller keeps a clone and flips it (e.g. on user Esc) to
    /// short-circuit a long-running motion / operator. Per
    /// DESIGN.md §5.7, cancellation is cooperative -- the grammar
    /// polls at quantisation points (per-row in blockwise ops, per
    /// match in search loops, etc.).
    pub fn dispatch_with_cancel(
        &self,
        invocation: CommandInvocation,
        cursor: Position,
        cancel: CancellationToken,
    ) -> Pending<Effect> {
        self.dispatch_with_env(invocation, cursor, cancel, crate::document::DispatchEnv::default())
    }

    /// N.1.4b / N.1.6 (2026-06-10): the env-carrying dispatch entry.
    /// Threads the [`DispatchEnv`](crate::document::DispatchEnv) (the
    /// tree-sitter `scope_resolver` for af/ac/aa/al + the `comment_syntax`
    /// for aC/iC) into [`ActorMsg::Dispatch`] so the actor hands it to
    /// `execute_with_env`. The env's Arc fields cross the actor channel
    /// as Arc bumps; the snapshot is immutable so the actor reads it
    /// wait-free. Staleness note: the resolver reflects the
    /// last-published syntax tree, which may trail the actor's rope by an
    /// in-flight edit -- acceptable eventual consistency (CLAUDE.md),
    /// never a blocking reparse on the hot path (paramount #1).
    pub fn dispatch_with_env(
        &self,
        invocation: CommandInvocation,
        cursor: Position,
        cancel: CancellationToken,
        env: crate::document::DispatchEnv,
    ) -> Pending<Effect> {
        let buffer_id = self.buffer_id;
        self.send(|reply| ActorMsg::Dispatch {
            buffer_id,
            invocation,
            cursor,
            cancel,
            env,
            reply,
        })
    }

    /// M.2.b.0.A (2026-05-31): registry-level identity of this
    /// handle. Stable for the handle's lifetime.
    pub fn buffer_id(&self) -> lattice_core::BufferId {
        self.buffer_id
    }

    // ---- internals ----

    /// Common scaffolding for every mutating method: allocate the
    /// `oneshot`, build the `ActorMsg`, `try_send`, and wrap the
    /// receiver in a `Pending`. On `Full`, build a `Pending` whose
    /// receiver is pre-loaded with `Busy` so the caller observes
    /// the failure through the same await/`blocking_recv` path.
    fn send<T, F>(&self, build: F) -> Pending<T>
    where
        F: FnOnce(oneshot::Sender<Result<T, RuntimeError>>) -> ActorMsg,
    {
        let id = InvocationId::next();
        let (tx, rx) = oneshot::channel();
        let msg = build(tx);
        if self.sender.send(msg).is_err() {
            // Receiver closed -> actor gone. The original `tx`
            // was consumed into `msg` and dropped on send failure;
            // mint a fresh oneshot so the caller's pending
            // resolves immediately with the right error.
            let (gone_tx, gone_rx) = oneshot::channel();
            let _ = gone_tx.send(Err(RuntimeError::ActorGone));
            return Pending::new(id, gone_rx);
        }
        Pending::new(id, rx)
    }
}

/// M.0 (2026-05-31): handle-layer [`Document`] trait impl.
/// Delegates every method to the inherent method of the same
/// name on `RopeDocumentHandle` via fully-qualified syntax — the
/// trait is a thin abstraction over the existing API surface;
/// callers gain `Arc<dyn Document>` polymorphism for free.
impl crate::document::Document for RopeDocumentHandle {
    fn snapshot(&self) -> Arc<DocumentSnapshot> {
        RopeDocumentHandle::snapshot(self)
    }

    fn snapshot_cache(&self) -> crate::snapshot::SnapshotCache {
        RopeDocumentHandle::snapshot_cache(self)
    }

    fn id(&self) -> DocumentId {
        RopeDocumentHandle::id(self)
    }

    fn text(&self) -> String {
        RopeDocumentHandle::text(self)
    }

    fn path(&self) -> Option<PathBuf> {
        RopeDocumentHandle::path(self)
    }

    fn dirty(&self) -> bool {
        RopeDocumentHandle::dirty(self)
    }

    fn version(&self) -> u64 {
        RopeDocumentHandle::version(self)
    }

    fn text_version(&self) -> u64 {
        RopeDocumentHandle::text_version(self)
    }

    fn selections(&self) -> Arc<SelectionSet> {
        RopeDocumentHandle::selections(self)
    }

    fn apply_edit(&self, edit: Edit) -> Pending<AppliedEdit> {
        RopeDocumentHandle::apply_edit(self, edit)
    }

    fn apply_edit_batch(&self, edits: Vec<Edit>) -> Pending<Vec<AppliedEdit>> {
        RopeDocumentHandle::apply_edit_batch(self, edits)
    }

    fn undo(&self) -> Pending<Vec<AppliedEdit>> {
        RopeDocumentHandle::undo(self)
    }

    fn redo(&self) -> Pending<Vec<AppliedEdit>> {
        RopeDocumentHandle::redo(self)
    }

    fn save(&self) -> Pending<PathBuf> {
        RopeDocumentHandle::save(self)
    }

    fn save_as(&self, path: PathBuf) -> Pending<()> {
        RopeDocumentHandle::save_as(self, path)
    }

    fn set_selections(&self, selections: SelectionSet) -> Pending<()> {
        RopeDocumentHandle::set_selections(self, selections)
    }

    fn dispatch_with_cancel(
        &self,
        invocation: CommandInvocation,
        cursor: Position,
        cancel: CancellationToken,
    ) -> Pending<Effect> {
        RopeDocumentHandle::dispatch_with_cancel(self, invocation, cursor, cancel)
    }

    fn dispatch_with_env(
        &self,
        invocation: CommandInvocation,
        cursor: Position,
        cancel: CancellationToken,
        env: crate::document::DispatchEnv,
    ) -> Pending<Effect> {
        RopeDocumentHandle::dispatch_with_env(self, invocation, cursor, cancel, env)
    }
}

impl std::fmt::Debug for RopeDocumentHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let snap = self.snapshot();
        f.debug_struct("RopeDocumentHandle")
            .field("id", &snap.id)
            .field("version", &snap.version)
            .field("text_version", &snap.text_version)
            .field("dirty", &snap.dirty)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lattice_protocol::position::Position;

    fn empty_registry() -> Arc<CommandRegistry> {
        Arc::new(CommandRegistry::new())
    }

    /// M.0 (2026-05-31): `Arc<dyn Document>` works — the trait
    /// impl on `RopeDocumentHandle` is dyn-safe and delegates to
    /// the inherent methods, so callers holding the trait
    /// object see the same observable behaviour as callers
    /// holding the concrete handle.
    #[tokio::test(flavor = "multi_thread")]
    async fn handle_is_usable_as_dyn_document() {
        // Bring the trait into scope so trait-object method
        // resolution works on `&dyn Document`. The `Document`
        // type at module scope is the inner struct
        // (`lattice_core::Document`); the trait lives at
        // `crate::document::Document`.
        use crate::document::Document as DocumentTrait;

        let handle = spawn_document(lattice_core::BufferId(0), Document::from_text("hello"), empty_registry());
        let dyn_doc: std::sync::Arc<dyn DocumentTrait> =
            std::sync::Arc::new(handle.clone());

        // Reads go through the trait method (default impl).
        assert_eq!(dyn_doc.text(), "hello");
        assert_eq!(dyn_doc.version(), 0);
        assert!(!dyn_doc.dirty());

        // Writes go through the trait method (delegated to actor).
        dyn_doc
            .apply_edit(Edit::insert(Position::new(0, 5), "!"))
            .await
            .unwrap();

        // Trait-object read sees the new state; concrete handle
        // sees the same snapshot (same actor underneath).
        assert_eq!(dyn_doc.text(), "hello!");
        assert_eq!(handle.text(), "hello!");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn handle_clone_shares_actor() {
        let h1 = spawn_document(lattice_core::BufferId(0), Document::from_text("a"), empty_registry());
        let h2 = h1.clone();
        h1.apply_edit(Edit::insert(Position::new(0, 1), "b"))
            .await
            .unwrap();
        // Both handles see the same published snapshot.
        assert_eq!(h1.snapshot().text(), "ab");
        assert_eq!(h2.snapshot().text(), "ab");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn save_as_updates_path_in_snapshot() {
        let dir = std::env::temp_dir();
        let target = dir.join("lattice-handle-save-as.txt");
        let h = spawn_document(lattice_core::BufferId(0), Document::from_text("payload"), empty_registry());
        h.save_as(target.clone()).await.unwrap();
        let snap = h.snapshot();
        assert_eq!(snap.path(), Some(target.as_path()));
        assert!(!snap.dirty);
        let _ = std::fs::remove_file(&target);
    }
}
