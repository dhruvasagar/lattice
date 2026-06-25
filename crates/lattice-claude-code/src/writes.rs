//! IDE-protocol I3: the three write tools.
//!
//! Thin async plumbing: build a [`ClaudeCodeInboundRequest`], `send` it on the
//! bus (which wakes the actor — see [`crate::inbound`]), `await` the oneshot
//! reply (bounded by a timeout backstop), and shape an MCP result. All the
//! validation + Effect mapping lives in `inbound::map_request` (tested there);
//! these functions only marshal arguments and the reply.
//!
//! Every failure path is graceful: missing/absent bus, a dropped receiver
//! (server stopped) → `send` errors, or a timeout all return `success: false`
//! with a message — never a hang, never a panic.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::inbound::{ClaudeCodeInboundRequest, InboundKind};
use lattice_grammar::Utf16Pos;
use lattice_mode::inbound::InboundBus;

/// Backstop so a write tool can never hang the agent connection even if the
/// editor never resolves the oneshot (it always should — the drain resolves
/// synchronously once the actor wakes).
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// `openFile`: open `filePath`, optionally at a `selection` start position.
pub async fn open_file(bus: Option<&InboundBus<ClaudeCodeInboundRequest>>, args: &Value) -> Value {
    let Some(path) = args.get("filePath").and_then(|v| v.as_str()) else {
        return result(false, "openFile: missing filePath");
    };
    run_write(
        bus,
        InboundKind::OpenFile {
            path: PathBuf::from(path),
            column: parse_position(args),
        },
    )
    .await
}

/// `saveDocument`: save the document for `filePath`.
pub async fn save_document(bus: Option<&InboundBus<ClaudeCodeInboundRequest>>, args: &Value) -> Value {
    let Some(path) = args.get("filePath").and_then(|v| v.as_str()) else {
        return result(false, "saveDocument: missing filePath");
    };
    run_write(
        bus,
        InboundKind::SaveDocument {
            path: PathBuf::from(path),
        },
    )
    .await
}

/// `close_tab`: close the tab named `tab_name`.
pub async fn close_tab(bus: Option<&InboundBus<ClaudeCodeInboundRequest>>, args: &Value) -> Value {
    let Some(tab) = args.get("tab_name").and_then(|v| v.as_str()) else {
        return result(false, "close_tab: missing tab_name");
    };
    run_write(
        bus,
        InboundKind::CloseTab {
            tab_name: tab.to_string(),
        },
    )
    .await
}

/// Send the request, await the reply, shape the result. The single graceful
/// path for all three tools.
async fn run_write(bus: Option<&InboundBus<ClaudeCodeInboundRequest>>, kind: InboundKind) -> Value {
    let Some(bus) = bus else {
        return result(false, "write unavailable: IDE server not fully initialized");
    };
    let (tx, rx) = oneshot::channel();
    if bus
        .send(ClaudeCodeInboundRequest { kind, response: tx })
        .is_err()
    {
        // Receiver dropped — the editor/server is gone.
        return result(false, "write failed: editor not reachable");
    }
    match tokio::time::timeout(WRITE_TIMEOUT, rx).await {
        Ok(Ok(reply)) => result(reply.ok, reply.message.as_deref().unwrap_or("ok")),
        // Sender dropped without replying, or timed out.
        _ => result(false, "write failed: editor did not respond"),
    }
}

/// Parse an optional `selection.start` (`{line, character}`) into a UTF-16
/// column for [`Effect::OpenBufferAtColumn`](lattice_grammar::Effect::OpenBufferAtColumn)
/// — the host converts it to a byte offset against the *opened* line. The
/// agent's `character` is VS Code-style UTF-16, passed through verbatim (BC.8c
/// retired the earlier provisional byte interpretation). Returns `None` when the
/// agent sent no selection: the file opens without forcing the cursor (so
/// re-opening an already-open file keeps its position).
fn parse_position(args: &Value) -> Option<Utf16Pos> {
    let start = args.get("selection").and_then(|s| s.get("start"))?;
    let line = start.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let col = start.get("character").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    Some(Utf16Pos { line, col })
}

/// The write-tool result body. `success` mirrors the drain's optimistic-ack.
fn result(success: bool, message: &str) -> Value {
    json!({ "success": success, "message": message })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[tokio::test]
    async fn write_with_no_bus_is_graceful_failure() {
        let v = open_file(None, &json!({ "filePath": "/a.rs" })).await;
        assert_eq!(v["success"], false);
        assert!(v["message"].as_str().unwrap().contains("not fully initialized"));
    }

    #[tokio::test]
    async fn missing_argument_is_graceful_failure() {
        let v = open_file(None, &json!({})).await;
        assert_eq!(v["success"], false);
        assert!(v["message"].as_str().unwrap().contains("missing filePath"));
        let v = close_tab(None, &json!({})).await;
        assert!(v["message"].as_str().unwrap().contains("missing tab_name"));
    }

    #[test]
    fn parse_position_reads_selection_start_else_none() {
        // No selection → None (open only, don't force the cursor).
        assert_eq!(parse_position(&json!({})), None);
        // A selection's `character` is a UTF-16 column, passed through verbatim.
        let p = parse_position(&json!({ "selection": { "start": { "line": 4, "character": 2 } } }));
        assert_eq!(p, Some(Utf16Pos { line: 4, col: 2 }));
    }

    #[tokio::test]
    async fn open_file_round_trips_through_a_live_bus() {
        use crate::inbound::make_handler;
        use crate::snapshot::ClaudeCodeReadState;
        use lattice_mode::inbound::make_inbound;
        use std::sync::{Arc, Mutex};
        use tokio::sync::Notify;

        let cache = Arc::new(Mutex::new(ClaudeCodeReadState::default()));
        let (bus, mut drain) = make_inbound(Arc::new(Notify::new()), make_handler(cache));

        // The write awaits the reply; here we drive the drain (the "tick") that
        // resolves it. Poll the drain until the queued request produces its
        // Effect, then collect the tool's result.
        let call = tokio::spawn({
            let bus = bus.clone();
            async move { open_file(Some(&bus), &json!({ "filePath": "/a.rs" })).await }
        });
        let mut effects = Vec::new();
        for _ in 0..50 {
            effects = drain();
            if !effects.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(effects.len(), 1, "openFile maps to one Effect");
        let v = call.await.unwrap();
        assert_eq!(v["success"], true);
    }
}
