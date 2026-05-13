//! Server-initiated `workspace/applyEdit` plumbing (Phase 4.3).
//!
//! When a language server sends `workspace/applyEdit` (most
//! commonly during `workspace/executeCommand` callbacks for
//! code actions) the client must:
//!
//! 1. Apply the supplied [`lsp_types::WorkspaceEdit`] to the
//!    affected buffers.
//! 2. Reply with `ApplyWorkspaceEditResponse { applied,
//!    failure_reason, failed_change }` so the server knows
//!    whether to roll back its own state.
//!
//! Step 1 needs the App's mutable buffer state, which lives on
//! the synchronous UI thread; step 2 must come back on the
//! tokio actor's task. The bridge is this module's unbounded
//! mpsc channel: the actor receives the request, packages it
//! into [`InboundApplyEdit`] (with a oneshot for the response),
//! and dispatches it through [`ApplyEditBus::dispatch`]. The
//! App drains the receiver each frame, applies the edit, and
//! writes the [`ApplyEditOutcome`] back through the oneshot.
//! The actor's spawned response-task reads the oneshot, builds
//! the LSP `Response`, and ferries it to the wire.
//!
//! The bus is Optional on actor spawn -- pre-applyEdit tests
//! and embedded transports that don't care about server-
//! initiated edits skip the channel and the actor falls back
//! to a `MethodNotFound` response. The supervisor builds one
//! bus at init and clones the sender into every spawned actor;
//! the App owns the receiver and drains it from its runtime tick.

use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

/// One server-initiated `workspace/applyEdit` request, ferried
/// from the LSP actor's task to the App's drain. Carries the
/// untyped LSP `WorkspaceEdit` (the App reuses its existing
/// flatten + apply path) plus a oneshot the App fills with the
/// outcome.
#[derive(Debug)]
pub struct InboundApplyEdit {
    /// Server that sent the request. Used by the App's echo /
    /// log so the user can tell which language server is
    /// asking. Cheap to clone (`Arc<str>`).
    pub server_id: Arc<str>,
    /// Optional descriptive label the server attached to the
    /// edit (e.g. `"organize imports"`). Spec field; we surface
    /// it in the App's echo and the log entry.
    pub label: Option<String>,
    /// The edit to apply. The App's existing
    /// `flatten_workspace_edit_for_apply` path turns this into
    /// per-file edit batches.
    pub edit: lsp_types::WorkspaceEdit,
    /// Oneshot the App fills after applying. The actor task
    /// awaits this and converts the outcome into the LSP
    /// `Response`.
    pub response: oneshot::Sender<ApplyEditOutcome>,
}

/// Result the App reports back to the actor's response task.
/// Mirrors `ApplyWorkspaceEditResponse` minus the `failed_change`
/// index which the App doesn't track today (every per-file edit
/// is a separate batch; partial-apply with a failure echoes a
/// warning but doesn't roll back). Future atomic-rollback work
/// fills `failed_change`.
#[derive(Debug, Clone)]
pub struct ApplyEditOutcome {
    /// Whether ANY edit was applied. `false` when nothing
    /// landed (parse error, no buffer matched the URI, etc.);
    /// `true` for full success AND for partial success.
    pub applied: bool,
    /// Optional human-readable description -- spec lets the
    /// client send this when `applied: false` and surfaces it
    /// in the server's failure handling. We also send it on
    /// partial success so server logs explain the situation.
    pub failure_reason: Option<String>,
}

/// Multiplexed sender end of the apply-edit channel. Every LSP
/// actor holds a clone; the App holds the matching receiver.
/// Dropping the receiver disables future dispatches (the actor
/// falls back to method-not-found responses).
#[derive(Clone)]
pub struct ApplyEditBus {
    tx: mpsc::UnboundedSender<InboundApplyEdit>,
}

impl ApplyEditBus {
    /// Build a fresh bus + receiver pair. The App owns the
    /// receiver; the supervisor stores the bus and clones it
    /// into each actor it spawns.
    pub fn new() -> (Self, mpsc::UnboundedReceiver<InboundApplyEdit>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }

    /// Dispatch a request to the App's drain. Returns `Err`
    /// (with the unsent payload) when the receiver has been
    /// dropped -- the actor's response task catches this and
    /// replies with an `applied: false` response so the server
    /// doesn't hang.
    pub fn dispatch(&self, ev: InboundApplyEdit) -> Result<(), InboundApplyEdit> {
        self.tx.send(ev).map_err(|e| e.0)
    }
}

impl std::fmt::Debug for ApplyEditBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApplyEditBus").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dispatch_round_trips_to_receiver() {
        let (bus, mut rx) = ApplyEditBus::new();
        let (tx, _resp_rx) = oneshot::channel();
        let edit = lsp_types::WorkspaceEdit::default();
        bus.dispatch(InboundApplyEdit {
            server_id: Arc::from("test"),
            label: Some("organize imports".into()),
            edit,
            response: tx,
        })
        .expect("receiver alive");
        let got = rx.recv().await.expect("payload arrived");
        assert_eq!(&*got.server_id, "test");
        assert_eq!(got.label.as_deref(), Some("organize imports"));
    }

    #[tokio::test]
    async fn dispatch_returns_err_when_receiver_dropped() {
        let (bus, rx) = ApplyEditBus::new();
        drop(rx);
        let (tx, _resp_rx) = oneshot::channel();
        let result = bus.dispatch(InboundApplyEdit {
            server_id: Arc::from("test"),
            label: None,
            edit: lsp_types::WorkspaceEdit::default(),
            response: tx,
        });
        assert!(result.is_err(), "dispatch surfaces drop as Err");
    }

    #[tokio::test]
    async fn outcome_response_round_trips_via_oneshot() {
        let (resp_tx, resp_rx) = oneshot::channel();
        let outcome = ApplyEditOutcome {
            applied: true,
            failure_reason: None,
        };
        resp_tx.send(outcome).expect("rx alive");
        let got = resp_rx.await.expect("oneshot completes");
        assert!(got.applied);
        assert!(got.failure_reason.is_none());
    }
}
