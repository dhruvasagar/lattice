//! Server-initiated `workspace/applyEdit` plumbing (Phase 4.3; BC.8d reshape).
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
//! Step 1 needs the editor's mutable buffer state (`&mut Editor`);
//! step 2 must come back on the tokio actor's task. The bridge is
//! a channel: the actor receives the request, packages it into
//! [`InboundApplyEdit`] (with a oneshot for the response), and
//! sends it through [`ApplyEditBus`]. The host drains the receiver
//! each tick, applies the edit, and writes the [`ApplyEditOutcome`]
//! back through the oneshot. The actor's spawned response-task
//! reads the oneshot, builds the LSP `Response`, and ferries it to
//! the wire.
//!
//! **BC.8d (2026-06-24): reshaped onto the generic inbound primitive.**
//! `ApplyEditBus` is now a type alias for the generic
//! [`InboundBus`](lattice_mode::inbound::InboundBus) built via
//! [`make_inbound_raw`](lattice_mode::inbound::make_inbound_raw): its `send`
//! **wakes the editor**, so a server-initiated edit is applied off-keystroke
//! (was: no wake — it only landed on the next keypress). Unlike the
//! configuration / show-document buses, this is the *host-drained* variant: the
//! apply (`Editor::apply_inbound_workspace_edit`) is irreducibly `&mut Editor`
//! and carries `lsp_types`, which cannot cross the [`Effect`] boundary into a
//! mode-owned handler — so the host keeps the receiver
//! (`Editor::pending_apply_edit_rx`) and drains it in `run_tick_pending`, while
//! the bus contributes only the structural wake. This keeps the irreducible
//! apply as documented host residue (the diff-lifecycle / multibuffer
//! Effect-arm class) without introducing an internal-pump `Effect`. The
//! real-outcome reply (`applied` reflects what actually landed) is preserved.
//!
//! [`Effect`]: lattice_grammar::effect::Effect

use std::sync::Arc;

use tokio::sync::oneshot;

/// The bus the supervisor fans out to each actor -- the generic inbound
/// primitive specialised to the apply-edit payload, built host-drained via
/// [`make_inbound_raw`](lattice_mode::inbound::make_inbound_raw). `send` wakes
/// the editor; the host owns the matching receiver and drains it. (Was the
/// bespoke `ApplyEditBus` struct before BC.8d.)
pub type ApplyEditBus = lattice_mode::inbound::InboundBus<InboundApplyEdit>;

/// One server-initiated `workspace/applyEdit` request, ferried
/// from the LSP actor's task to the host's drain. Carries the
/// untyped LSP `WorkspaceEdit` (the host reuses its existing
/// flatten + apply path) plus a oneshot the host fills with the
/// outcome.
#[derive(Debug)]
pub struct InboundApplyEdit {
    /// Server that sent the request. Used by the host's echo /
    /// log so the user can tell which language server is
    /// asking. Cheap to clone (`Arc<str>`).
    pub server_id: Arc<str>,
    /// Workspace root the originating actor was spawned against
    /// (B'.2). Pairs with `server_id` to form the canonical
    /// `(server_id, workspace)` instance key.
    pub workspace: Arc<std::path::Path>,
    /// Optional descriptive label the server attached to the
    /// edit (e.g. `"organize imports"`). Spec field; we surface
    /// it in the host's echo and the log entry.
    pub label: Option<String>,
    /// The edit to apply. The host's existing
    /// `flatten_workspace_edit` path turns this into per-file
    /// edit batches.
    pub edit: lsp_types::WorkspaceEdit,
    /// Oneshot the host fills after applying. The actor task
    /// awaits this and converts the outcome into the LSP
    /// `Response`.
    pub response: oneshot::Sender<ApplyEditOutcome>,
}

/// Result the host reports back to the actor's response task.
/// Mirrors `ApplyWorkspaceEditResponse` minus the `failed_change`
/// index which the host doesn't track today (every per-file edit
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

#[cfg(test)]
mod tests {
    use super::*;

    // BC.8d: the bespoke `ApplyEditBus::new()`/`dispatch()` round-trip + dropped-
    // receiver tests are retired — the bus is now the generic `InboundBus`, whose
    // send/wake/dropped-receiver behaviour is pinned in `lattice-mode`'s inbound
    // tests (incl. `raw_send_wakes_and_receiver_gets_item`). The host-side apply
    // + real-outcome reply stays exercised by `lattice-ui-tui`'s
    // `drain_inbound_apply_edits_*` tests against `Editor::apply_inbound_workspace_edit`.

    /// The outcome still round-trips through the request's oneshot (the shape
    /// the actor's response task awaits).
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
