//! IDE-protocol I4: the `openDiff` tool — open an interactive diff and BLOCK
//! until the user Keeps or Rejects it.
//!
//! Unlike the I3 write tools (which send an `Effect` on the generic handler bus
//! and return an optimistic ack), `openDiff` is *blocking* and opens a diff
//! whose host-side machinery is irreducibly `&mut Editor` + lattice-diff types.
//! So it rides a SECOND, host-drained bus
//! ([`lattice_diff::ProgrammaticDiffBus`], built via `boot.inbound_raw` and
//! registered as a service): this tool delegates to
//! [`lattice_agent::review_diff`], which builds a
//! [`lattice_diff::ProgrammaticDiffRequest`], `send`s it (which wakes the
//! editor), and `await`s the request's completion oneshot — with **no
//! timeout** (the user reviews at their own pace). The host opens a
//! side-by-side diff, binds the oneshot to the session, and fires the
//! [`DiffOutcome`](lattice_diff::subsystem::DiffOutcome) on
//! `:diff-accept` / `:diff-reject` (or a close-tab cancel, which drops the
//! sender → a graceful reject here).
//!
//! Reply shape (PROVISIONAL until validated against a live `claude` CLI): the
//! MCP `CallToolResult` content carries a `FILE_SAVED` marker on Accept (the
//! host already wrote the proposed content to `old_file_path` — the review IS
//! the save) or `DIFF_REJECTED` on Reject. The agent branches on these markers.

use std::path::PathBuf;

use serde_json::{Value, json};

use lattice_agent::{AgentError, DiffReviewRequest, review_diff};
use lattice_diff::ProgrammaticDiffBus;
use lattice_diff::subsystem::DiffOutcome;

/// `openDiff`: open an interactive diff between `old_file_path`'s on-disk
/// content and `new_file_contents`, blocking until the user resolves it.
///
/// Returns the full MCP `CallToolResult` envelope (NOT wrapped by the dispatch's
/// `tool_text_result`, since the content markers are the contract). Every
/// failure path is graceful — missing args, an absent bus, or a dropped
/// receiver all return a result rather than hanging or panicking.
pub async fn open_diff(
    bus: Option<&ProgrammaticDiffBus>,
    args: &Value,
    // D-fix.6: the originating connection id — tags the diff so a later
    // session-scoped close from THIS connection tears it down.
    conn_id: u64,
    // D-fix.6 follow-up: the shared pending-review tracker. A guard is held
    // across the blocking `await` so the modeline shows a `◆ review` badge
    // while the agent is blocked on the user, cleared on any outcome.
    review: &crate::status::ReviewHandle,
) -> Value {
    let Some(bus) = bus else {
        return error_result("openDiff unavailable: IDE server not fully initialized");
    };
    // `old_file_path` (baseline) + `new_file_contents` (proposed) are required;
    // `new_file_path` defaults to `old_file_path` (an in-place edit).
    let Some(old) = args.get("old_file_path").and_then(|v| v.as_str()) else {
        return error_result("openDiff: missing old_file_path");
    };
    let Some(contents) = args.get("new_file_contents").and_then(|v| v.as_str()) else {
        return error_result("openDiff: missing new_file_contents");
    };
    let new_path = args
        .get("new_file_path")
        .and_then(|v| v.as_str())
        .unwrap_or(old);
    let tab = args
        .get("tab_name")
        .and_then(|v| v.as_str())
        .unwrap_or("openDiff");

    let request = DiffReviewRequest {
        old_file_path: PathBuf::from(old),
        new_file_path: PathBuf::from(new_path),
        new_contents: contents.to_string(),
        tab_name: tab.to_string(),
        origin_session: conn_id,
    };

    // D-fix.6 follow-up: a review is now pending for the modeline badge. The
    // guard clears it on ANY exit below -- resolve, cancel, or the task being
    // dropped -- so the count can never leak high. The badge is a modeline
    // concern, so it stays adapter-side rather than moving into the port.
    let _review = review.begin();

    match review_diff(bus, request).await {
        Ok(DiffOutcome::Accept) => saved_result(),
        // Reject, a cancelled review (dropped sender), or any future
        // `DiffOutcome` variant (the enum is `#[non_exhaustive]`) -> "not
        // saved": a reject reply, so the agent never hangs.
        Ok(_) | Err(AgentError::Cancelled(_)) => rejected_result(tab),
        Err(_) => error_result("openDiff failed: editor not reachable"),
    }
}

/// Accept reply: the host already persisted the proposed content.
fn saved_result() -> Value {
    text_result("FILE_SAVED", false)
}

/// Reject reply: the marker plus the tab name (so the agent can correlate).
fn rejected_result(tab: &str) -> Value {
    json!({
        "content": [
            { "type": "text", "text": "DIFF_REJECTED" },
            { "type": "text", "text": tab },
        ],
        "isError": false,
    })
}

/// A graceful failure envelope (`isError: true`) — never a hang or panic.
fn error_result(message: &str) -> Value {
    text_result(message, true)
}

/// One-text-block `CallToolResult` envelope.
fn text_result(text: &str, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lattice_diff::ProgrammaticDiffRequest;
    use lattice_mode::inbound::make_inbound_raw;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Notify;

    /// A throwaway pending-review tracker for the openDiff tests.
    fn review() -> crate::status::ReviewHandle {
        crate::status::ReviewState::new(Arc::new(Notify::new()))
    }

    #[tokio::test]
    async fn no_bus_is_graceful_error() {
        let v = open_diff(
            None,
            &json!({ "old_file_path": "/a.rs", "new_file_contents": "x" }),
            0,
            &review(),
        )
        .await;
        assert_eq!(v["isError"], true);
        assert!(v["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("not fully initialized"));
    }

    #[tokio::test]
    async fn missing_required_args_are_graceful_errors() {
        let (bus, _rx) = make_inbound_raw::<ProgrammaticDiffRequest>(Arc::new(Notify::new()));
        // Missing new_file_contents.
        let v = open_diff(Some(&bus), &json!({ "old_file_path": "/a.rs" }), 0, &review()).await;
        assert_eq!(v["isError"], true);
        assert!(v["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("missing new_file_contents"));
        // Missing old_file_path.
        let v = open_diff(Some(&bus), &json!({ "new_file_contents": "x" }), 0, &review()).await;
        assert!(v["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("missing old_file_path"));
    }

    #[tokio::test]
    async fn accept_yields_file_saved() {
        let (bus, mut rx) = make_inbound_raw::<ProgrammaticDiffRequest>(Arc::new(Notify::new()));
        let call = tokio::spawn({
            let bus = bus.clone();
            async move {
                open_diff(
                    Some(&bus),
                    &json!({
                        "old_file_path": "/a.rs",
                        "new_file_contents": "new",
                        "tab_name": "demo",
                    }),
                    0,
                    &review(),
                )
                .await
            }
        });
        // Play the host: drain the request, resolve its oneshot with Accept.
        let req = recv_soon(&mut rx).await;
        assert_eq!(req.old_file_path, PathBuf::from("/a.rs"));
        assert_eq!(req.new_file_path, PathBuf::from("/a.rs"), "defaults to old");
        assert_eq!(req.new_contents, "new");
        req.response.send(DiffOutcome::Accept).unwrap();

        let v = call.await.unwrap();
        assert_eq!(v["isError"], false);
        assert_eq!(v["content"][0]["text"], "FILE_SAVED");
    }

    #[tokio::test]
    async fn reject_yields_diff_rejected_with_tab() {
        let (bus, mut rx) = make_inbound_raw::<ProgrammaticDiffRequest>(Arc::new(Notify::new()));
        let call = tokio::spawn({
            let bus = bus.clone();
            async move {
                open_diff(
                    Some(&bus),
                    &json!({
                        "old_file_path": "/a.rs",
                        "new_file_path": "/b.rs",
                        "new_file_contents": "new",
                        "tab_name": "the-tab",
                    }),
                    0,
                    &review(),
                )
                .await
            }
        });
        let req = recv_soon(&mut rx).await;
        assert_eq!(req.new_file_path, PathBuf::from("/b.rs"), "explicit new path");
        req.response.send(DiffOutcome::Reject).unwrap();

        let v = call.await.unwrap();
        assert_eq!(v["content"][0]["text"], "DIFF_REJECTED");
        assert_eq!(v["content"][1]["text"], "the-tab");
    }

    #[tokio::test]
    async fn dropped_sender_is_treated_as_reject() {
        // A close-tab cancel drops the bound sender; the awaiting agent must get
        // a graceful reject, never a hang.
        let (bus, mut rx) = make_inbound_raw::<ProgrammaticDiffRequest>(Arc::new(Notify::new()));
        let call = tokio::spawn({
            let bus = bus.clone();
            async move {
                open_diff(
                    Some(&bus),
                    &json!({
                        "old_file_path": "/a.rs",
                        "new_file_contents": "new",
                        "tab_name": "cancelled",
                    }),
                    0,
                    &review(),
                )
                .await
            }
        });
        let req = recv_soon(&mut rx).await;
        drop(req.response); // session cancelled without an explicit outcome
        let v = call.await.unwrap();
        assert_eq!(v["content"][0]["text"], "DIFF_REJECTED");
        assert_eq!(v["content"][1]["text"], "cancelled");
    }

    /// Poll the receiver briefly until the spawned `open_diff` has sent its
    /// request (the `send` happens after the task is scheduled).
    async fn recv_soon(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<ProgrammaticDiffRequest>,
    ) -> ProgrammaticDiffRequest {
        for _ in 0..100 {
            if let Ok(req) = rx.try_recv() {
                return req;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("openDiff request never arrived on the bus");
    }
}
