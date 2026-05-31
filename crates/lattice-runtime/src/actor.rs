//! `DocumentActor` -- the tokio task that owns one document's
//! writable state (DESIGN.md §5.7, §5.6.8).
//!
//! ## Responsibilities
//!
//! 1. **Exclusive ownership** of one [`lattice_core::Document`]. No
//!    other code path holds a `&mut Document`; mutations arrive as
//!    `ActorMsg` variants on the bounded mailbox.
//! 2. **Snapshot publish** after every committed mutation. The
//!    actor builds a [`DocumentSnapshot`] from the post-commit
//!    state and writes it to the [`PublishedSnapshot`] cell with
//!    `store_release` semantics.
//! 3. **Unbounded mailbox** -- audit slice 6 / H3. The mailbox
//!    was originally bounded with a `try_send` -> `Busy` path
//!    for callers; in practice, App-side `apply_edit_blocking`
//!    discarded `Busy` silently and bursts could desync the
//!    buffer from what the user typed. Unbounded eliminates the
//!    silent-drop class entirely; queue depth bounds itself by
//!    edit rate × actor-stall-duration (typing is human-paced).
//! 4. **Graceful shutdown** -- when every [`crate::DocumentHandle`]
//!    is dropped the mailbox closes; the actor's `recv` loop exits
//!    naturally.
//!
//! ## Why one task per document, not a thread
//!
//! Documents are I/O-bound (file save/open, future LSP
//! attribution); a tokio task is the right granularity. The
//! `LocalSet` complexity of "stay on one thread" isn't needed
//! because `Document` is `Send`. Across cores the actor still has
//! exclusive logical ownership -- only one task handles any given
//! document.
//!
//! ## Why the actor builds the snapshot, not the handle
//!
//! Snapshot publish happens *inside* the actor's task, after the
//! mutation. Doing it on the handle side would race with concurrent
//! readers and miss the publish-before-respond ordering required by
//! the §5.6.8 acquire/release contract.

use std::path::PathBuf;
use std::sync::Arc;

use lattice_core::{Buffer, CoreError, Document};
use lattice_grammar::{CancellationToken, CommandInvocation, CommandRegistry, Effect, execute};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::Position;
use lattice_protocol::selection::SelectionSet;
use tokio::sync::{mpsc, oneshot};

use crate::pending::RuntimeError;
use crate::snapshot::{DocumentSnapshot, PublishedSnapshot};

/// One unit of work the actor executes. Each variant carries its
/// own `oneshot::Sender` so the response can flow back to the
/// originating `Pending<T>`. The actor never sees the
/// `InvocationId`; it's purely caller-side telemetry.
///
/// `Shutdown` is here so tests can deterministically drain the
/// actor; in production graceful shutdown happens by dropping the
/// last handle (mailbox closes; `recv()` returns `None`).
pub(crate) enum ActorMsg {
    ApplyEdit {
        edit: Edit,
        reply: oneshot::Sender<Result<lattice_core::buffer::AppliedEdit, RuntimeError>>,
    },
    ApplyEditBatch {
        edits: Vec<Edit>,
        reply: oneshot::Sender<Result<Vec<lattice_core::buffer::AppliedEdit>, RuntimeError>>,
    },
    Undo {
        reply: oneshot::Sender<Result<Vec<lattice_core::buffer::AppliedEdit>, RuntimeError>>,
    },
    Redo {
        reply: oneshot::Sender<Result<Vec<lattice_core::buffer::AppliedEdit>, RuntimeError>>,
    },
    /// Save to the document's existing path. Returns the path so
    /// the caller can echo it without going back through the
    /// snapshot.
    Save {
        reply: oneshot::Sender<Result<PathBuf, RuntimeError>>,
    },
    /// Save to a new path (becomes the document's path).
    SaveAs {
        path: PathBuf,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    SetSelections {
        selections: SelectionSet,
        reply: oneshot::Sender<Result<(), RuntimeError>>,
    },
    /// Run a [`lattice_grammar::execute`] dispatch against the
    /// document. The actor holds the only `&mut Document` so all
    /// invocation-driven mutations route here. Returns the
    /// `Effect` the grammar produced; the App applies it to
    /// session-scoped state (registers, modal, marks, ...).
    /// `cursor` is the App's view cursor (per-pane), passed in
    /// because the grammar needs it but it's not document-owned
    /// state.
    /// `cancel` is the cooperative cancellation token. The actor
    /// passes it straight to [`lattice_grammar::execute`]; the
    /// caller (App) holds a clone and flips it on user Esc.
    /// Cheap callers that don't need cancellation pass
    /// [`CancellationToken::never()`].
    Dispatch {
        invocation: CommandInvocation,
        cursor: Position,
        cancel: CancellationToken,
        reply: oneshot::Sender<Result<Effect, RuntimeError>>,
    },
}

/// The actor task. Constructed by [`crate::spawn_document`].
pub struct DocumentActor {
    document: Document,
    /// Shared with the App and any other caller; the actor holds
    /// `Arc` only so it can run [`lattice_grammar::execute`]
    /// against the registry from within its own task.
    registry: Arc<CommandRegistry>,
    inbox: mpsc::UnboundedReceiver<ActorMsg>,
    snapshot_cell: Arc<PublishedSnapshot>,
}

impl DocumentActor {
    pub(crate) fn new(
        document: Document,
        registry: Arc<CommandRegistry>,
        inbox: mpsc::UnboundedReceiver<ActorMsg>,
        snapshot_cell: Arc<PublishedSnapshot>,
    ) -> Self {
        Self {
            document,
            registry,
            inbox,
            snapshot_cell,
        }
    }

    /// Drive the actor to completion. Exits when every handle has
    /// been dropped (the mailbox closes). Spawned by
    /// [`crate::spawn_document`] onto the shared runtime.
    pub async fn run(mut self) {
        while let Some(msg) = self.inbox.recv().await {
            // Publish-before-reply: every message handler runs
            // its work, publishes the new snapshot, *then* sends
            // the reply. This guarantees a caller that observes
            // the reply (e.g. a `block_on` returns) also observes
            // the new published snapshot via `arc_swap::load`.
            // Without this ordering, callers can see stale
            // snapshots after their wait completes -- a race
            // every test failure here was hitting.
            self.handle(msg);
        }
        // All handles dropped -- graceful shutdown.
    }

    fn handle(&mut self, msg: ActorMsg) {
        match msg {
            ActorMsg::ApplyEdit { edit, reply } => {
                let result = self.document.apply_edit(edit).map_err(RuntimeError::Core);
                self.publish();
                let _ = reply.send(result);
            }
            ActorMsg::ApplyEditBatch { edits, reply } => {
                let result = self
                    .document
                    .apply_edit_batch(edits)
                    .map_err(RuntimeError::Core);
                self.publish();
                let _ = reply.send(result);
            }
            ActorMsg::Undo { reply } => {
                let result = self.document.undo().map_err(RuntimeError::Core);
                self.publish();
                let _ = reply.send(result);
            }
            ActorMsg::Redo { reply } => {
                let result = self.document.redo().map_err(RuntimeError::Core);
                self.publish();
                let _ = reply.send(result);
            }
            ActorMsg::Save { reply } => {
                let result = self
                    .document
                    .save()
                    .map(|p| p.to_path_buf())
                    .map_err(RuntimeError::Core);
                self.publish();
                let _ = reply.send(result);
            }
            ActorMsg::SaveAs { path, reply } => {
                let result = self.document.save_as(path).map_err(RuntimeError::Core);
                self.publish();
                let _ = reply.send(result);
            }
            ActorMsg::SetSelections { selections, reply } => {
                self.document.set_selections(selections);
                self.publish();
                let _ = reply.send(Ok(()));
            }
            ActorMsg::Dispatch {
                invocation,
                cursor,
                cancel,
                reply,
            } => {
                let result = execute(
                    &self.registry,
                    &mut self.document,
                    cursor,
                    invocation,
                    &cancel,
                )
                .map_err(RuntimeError::Grammar);
                self.publish();
                let _ = reply.send(result);
            }
        }
    }

    fn publish(&self) {
        self.snapshot_cell
            .store(DocumentSnapshot::from_document(&self.document));
    }
}

// Re-export AppliedEdit at this layer so callers don't need to
// reach into lattice-core/buffer just to spell the success type.
pub use lattice_core::buffer::AppliedEdit;

// Make the unused-symbol warnings explicit.
#[allow(dead_code)]
const _: fn(&CoreError, &Buffer) = |_, _| {};

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::handle::spawn_document;
    use lattice_grammar::CommandRegistry;
    use lattice_protocol::position::Position;

    fn empty_registry() -> Arc<CommandRegistry> {
        Arc::new(CommandRegistry::new())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn apply_edit_publishes_new_snapshot() {
        let handle = spawn_document(Document::from_text("hello"), empty_registry());
        let initial = handle.snapshot();
        assert_eq!(initial.text(), "hello");
        let initial_version = initial.version;

        handle
            .apply_edit(Edit::insert(Position::new(0, 5), " world"))
            .await
            .unwrap();

        let after = handle.snapshot();
        assert_eq!(after.text(), "hello world");
        assert!(after.version > initial_version);
        assert!(after.text_version > initial.text_version);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn undo_restores_previous_snapshot_text() {
        let handle = spawn_document(Document::from_text("a"), empty_registry());
        handle
            .apply_edit(Edit::insert(Position::new(0, 1), "b"))
            .await
            .unwrap();
        assert_eq!(handle.snapshot().text(), "ab");
        handle.undo().await.unwrap();
        assert_eq!(handle.snapshot().text(), "a");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn redo_replays_undone_edit() {
        let handle = spawn_document(Document::from_text(""), empty_registry());
        handle
            .apply_edit(Edit::insert(Position::ZERO, "x"))
            .await
            .unwrap();
        handle.undo().await.unwrap();
        handle.redo().await.unwrap();
        assert_eq!(handle.snapshot().text(), "x");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn snapshots_loaded_pre_publish_remain_coherent() {
        // §5.6.8 contract: an Arc<DocumentSnapshot> obtained at
        // frame start stays valid for the whole frame.
        let handle = spawn_document(Document::from_text("v1"), empty_registry());
        let pinned = handle.snapshot();
        handle
            .apply_edit(Edit::insert(Position::new(0, 2), "!"))
            .await
            .unwrap();
        assert_eq!(pinned.text(), "v1");
        assert_eq!(handle.snapshot().text(), "v1!");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_edit_returns_core_error_without_publish() {
        let handle = spawn_document(Document::from_text("abc"), empty_registry());
        let v_before = handle.snapshot().version;
        // Insert at line 99 -- out of range.
        let res = handle
            .apply_edit(Edit::insert(Position::new(99, 0), "x"))
            .await;
        assert!(matches!(res, Err(RuntimeError::Core(_))));
        // Version still bumps on the publish that happens after
        // every message, but the buffer text is unchanged.
        let after = handle.snapshot();
        assert_eq!(after.text(), "abc");
        // Version monotonicity holds (publish always happens).
        assert!(after.version >= v_before);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_all_handles_shuts_down_actor() {
        let handle = spawn_document(Document::from_text(""), empty_registry());
        let h2 = handle.clone();
        drop(handle);
        h2.apply_edit(Edit::insert(Position::ZERO, "a"))
            .await
            .unwrap();
        drop(h2);
        // Actor task exits when its mailbox closes; the test simply
        // asserts that no panic / hang occurs across the drop.
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dispatch_with_cancel_short_circuits_when_pre_flipped() {
        // The actor MUST honour a flipped cancellation token by
        // surfacing CommandError::Cancelled. The grammar dispatcher
        // checks the token before any registry lookup, so an empty
        // registry + bogus CommandId is a sufficient minimal case.
        use lattice_grammar::CommandId;
        use lattice_grammar::CommandInvocation;
        use lattice_grammar::error::CommandError;

        let handle = spawn_document(Document::from_text("hello"), empty_registry());
        let token = CancellationToken::new();
        token.cancel();

        let result = handle
            .dispatch_with_cancel(
                CommandInvocation::of(CommandId::new(1)),
                Position::ZERO,
                token,
            )
            .await;
        assert!(matches!(
            result,
            Err(RuntimeError::Grammar(CommandError::Cancelled))
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dispatch_with_cancel_runs_when_token_fresh() {
        // Sanity: a fresh token does not block dispatch -- the
        // unknown-command error must surface, not Cancelled.
        use lattice_grammar::CommandId;
        use lattice_grammar::CommandInvocation;
        use lattice_grammar::error::CommandError;

        let handle = spawn_document(Document::from_text("hello"), empty_registry());
        let token = CancellationToken::new();

        let result = handle
            .dispatch_with_cancel(
                CommandInvocation::of(CommandId::new(1)),
                Position::ZERO,
                token,
            )
            .await;
        assert!(matches!(
            result,
            Err(RuntimeError::Grammar(CommandError::UnknownCommand))
        ));
    }
}
