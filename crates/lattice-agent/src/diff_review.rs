//! The diff-review seam: propose an edit, block until the user rules on it.
//!
//! This is the one operation both agent protocols share verbatim. MCP's
//! `openDiff` and ACP's `session/request_permission` are two encodings of it.
//! The producer/awaiter lives here; the host owns the matching receiver and
//! resolves the reply when the user runs `:diff-accept` / `:diff-reject`.

use std::path::PathBuf;

use lattice_diff::subsystem::DiffOutcome;
use lattice_diff::{ProgrammaticDiffBus, ProgrammaticDiffRequest};
use tokio::sync::oneshot;

use crate::error::{AgentError, Result};

/// A proposed edit awaiting the user's verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffReviewRequest {
    /// Baseline path; its on-disk content is the left (read-only) side.
    pub old_file_path: PathBuf,
    /// Path the proposed content carries. An Accept writes the right side here.
    /// Usually equals `old_file_path` (an in-place edit).
    pub new_file_path: PathBuf,
    /// The proposed text — the editable right side.
    pub new_contents: String,
    /// Display label for the diff. Presentation only; teardown keys on
    /// `origin_session`.
    pub tab_name: String,
    /// The originating agent session. Tags the diff so a later session-scoped
    /// close from *this* session tears it down. `0` means "no origin".
    pub origin_session: u64,
}

/// Send `req` to the editor and block until the user resolves it.
///
/// No timeout: the user reviews at their own pace. A dropped reply channel
/// means the session was cancelled (the diff was closed, `:diffoff`, the editor
/// went away) and surfaces as [`AgentError::Cancelled`] — never a hang.
pub async fn review_diff(bus: &ProgrammaticDiffBus, req: DiffReviewRequest) -> Result<DiffOutcome> {
    let (tx, rx) = oneshot::channel::<DiffOutcome>();
    let request = ProgrammaticDiffRequest {
        old_file_path: req.old_file_path,
        new_file_path: req.new_file_path,
        new_contents: req.new_contents,
        tab_name: req.tab_name,
        origin_session: req.origin_session,
        response: tx,
    };
    if bus.send(request).is_err() {
        return Err(AgentError::Bus("programmatic diff receiver is gone".into()));
    }
    rx.await
        .map_err(|_| AgentError::Cancelled("diff review was dismissed".into()))
}

#[cfg(test)]
mod tests {
    use lattice_mode::inbound::make_inbound_raw;
    use std::sync::Arc;
    use tokio::sync::Notify;

    use super::*;

    fn req(session: u64) -> DiffReviewRequest {
        DiffReviewRequest {
            old_file_path: PathBuf::from("/tmp/a.rs"),
            new_file_path: PathBuf::from("/tmp/a.rs"),
            new_contents: "fn main() {}\n".to_string(),
            tab_name: "openDiff".to_string(),
            origin_session: session,
        }
    }

    /// The request reaches the host with every field intact, and the host's
    /// verdict comes back to the caller.
    #[tokio::test]
    async fn accept_round_trips_through_the_bus() {
        let (bus, mut rx) = make_inbound_raw::<ProgrammaticDiffRequest>(Arc::new(Notify::new()));

        let host = tokio::spawn(async move {
            let request = rx.recv().await.expect("a request should arrive");
            assert_eq!(request.old_file_path, PathBuf::from("/tmp/a.rs"));
            assert_eq!(request.new_contents, "fn main() {}\n");
            assert_eq!(request.origin_session, 7);
            request
                .response
                .send(DiffOutcome::Accept)
                .expect("caller is still awaiting");
        });

        let outcome = review_diff(&bus, req(7)).await.expect("accept");
        assert_eq!(outcome, DiffOutcome::Accept);
        host.await.expect("host task");
    }

    #[tokio::test]
    async fn reject_round_trips_through_the_bus() {
        let (bus, mut rx) = make_inbound_raw::<ProgrammaticDiffRequest>(Arc::new(Notify::new()));
        tokio::spawn(async move {
            let request = rx.recv().await.expect("a request should arrive");
            let _ = request.response.send(DiffOutcome::Reject);
        });
        assert_eq!(
            review_diff(&bus, req(1)).await.expect("reject"),
            DiffOutcome::Reject
        );
    }

    /// A dropped reply channel is a cancelled review, not a hang. This is the
    /// case that fires when the user closes the diff or the session dies.
    #[tokio::test]
    async fn dropped_reply_channel_is_cancelled_not_a_hang() {
        let (bus, mut rx) = make_inbound_raw::<ProgrammaticDiffRequest>(Arc::new(Notify::new()));
        tokio::spawn(async move {
            let request = rx.recv().await.expect("a request should arrive");
            drop(request); // the host gave up without answering
        });
        assert!(matches!(
            review_diff(&bus, req(1)).await,
            Err(AgentError::Cancelled(_))
        ));
    }

    /// No host draining the bus at all — a boot misconfiguration.
    #[tokio::test]
    async fn dropped_receiver_is_a_bus_error() {
        let (bus, rx) = make_inbound_raw::<ProgrammaticDiffRequest>(Arc::new(Notify::new()));
        drop(rx);
        assert!(matches!(
            review_diff(&bus, req(1)).await,
            Err(AgentError::Bus(_))
        ));
    }
}
