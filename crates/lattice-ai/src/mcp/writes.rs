//! IDE-protocol I3: the three write tools.
//!
//! Thin MCP envelope builders over [`lattice_agent::EditorAccess`]'s write
//! half (`open_file` / `save_document` / `close_tab` / `close_session_diffs`):
//! marshal arguments, call the port, and shape the result. All the
//! validation + Effect mapping + the bus send/await/timeout lives in the
//! port (`lattice_agent::write_bus` + `EditorAccess::run_write`); these
//! functions only parse arguments and translate the port's [`AgentError`]
//! back into the MCP reply strings the agent has always seen.
//!
//! Every failure path is graceful: missing/absent bus, a dropped receiver
//! (server stopped), or a timeout all return `success: false` with a
//! message — never a hang, never a panic.

use std::path::PathBuf;

use serde_json::{Value, json};

use lattice_agent::{AgentError, EditorAccess};
use lattice_grammar::Utf16Pos;

/// `openFile`: open `filePath`, optionally at a `selection` start position.
pub async fn open_file(editor: &EditorAccess, args: &Value) -> Value {
    let Some(path) = args.get("filePath").and_then(|v| v.as_str()) else {
        return result(false, "openFile: missing filePath");
    };
    let column = parse_position(args);
    match editor.open_file(PathBuf::from(path), column).await {
        Ok(()) => result(true, "ok"),
        Err(e) => result(false, &write_error_message(e)),
    }
}

/// `saveDocument`: save the document for `filePath`.
pub async fn save_document(editor: &EditorAccess, args: &Value) -> Value {
    let Some(path) = args.get("filePath").and_then(|v| v.as_str()) else {
        return result(false, "saveDocument: missing filePath");
    };
    match editor.save_document(PathBuf::from(path)).await {
        Ok(()) => result(true, "ok"),
        Err(e) => result(false, &write_error_message(e)),
    }
}

/// `close_tab`: close the tab named `tab_name`, scoped to connection
/// `conn_id` (D-fix.6) — the host rejects THAT connection's diff session(s).
pub async fn close_tab(editor: &EditorAccess, args: &Value, conn_id: u64) -> Value {
    let Some(tab) = args.get("tab_name").and_then(|v| v.as_str()) else {
        return result(false, "close_tab: missing tab_name");
    };
    match editor.close_tab(conn_id, tab.to_string()).await {
        Ok(()) => result(true, "ok"),
        Err(e) => result(false, &write_error_message(e)),
    }
}

/// D-fix.6 `closeAllDiffTabs`: reject every programmatic diff connection
/// `conn_id` opened. Takes no args (the connection id IS the scope).
pub async fn close_all_diff_tabs(editor: &EditorAccess, conn_id: u64) -> Value {
    match editor.close_session_diffs(conn_id).await {
        Ok(()) => result(true, "ok"),
        Err(e) => result(false, &write_error_message(e)),
    }
}

/// Unwrap an [`AgentError`] back to the ORIGINAL message text, discarding
/// `Display`'s variant prefix (`"editor not reachable: "` / `"editor io
/// error: "`). Using `e.to_string()` here would double-prefix the reply the
/// agent sees (e.g. `"editor not reachable: write failed: editor not
/// reachable"`) — a silent wire-format regression. This is the one place
/// that translation happens, so every write tool stays byte-identical to
/// the pre-port replies.
fn write_error_message(e: AgentError) -> String {
    match e {
        AgentError::Bus(m) | AgentError::Cancelled(m) | AgentError::Io(m) => m,
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
    use std::sync::{Arc, Mutex};

    fn editor_without_bus() -> EditorAccess {
        EditorAccess::new(
            Arc::new(Mutex::new(lattice_agent::EditorStateCache::default())),
            None,
            vec![],
            None,
        )
    }

    #[tokio::test]
    async fn write_with_no_bus_is_graceful_failure() {
        let v = open_file(&editor_without_bus(), &json!({ "filePath": "/a.rs" })).await;
        assert_eq!(v["success"], false);
        assert!(
            v["message"]
                .as_str()
                .unwrap()
                .contains("not fully initialized")
        );
    }

    #[tokio::test]
    async fn missing_argument_is_graceful_failure() {
        let v = open_file(&editor_without_bus(), &json!({})).await;
        assert_eq!(v["success"], false);
        assert!(v["message"].as_str().unwrap().contains("missing filePath"));
        let v = close_tab(&editor_without_bus(), &json!({}), 0).await;
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
        use lattice_agent::EditorStateCache;
        use lattice_mode::inbound::make_inbound;
        use std::time::Duration;
        use tokio::sync::Notify;

        let cache = Arc::new(Mutex::new(EditorStateCache::default()));
        let (bus, mut drain) = make_inbound(
            Arc::new(Notify::new()),
            lattice_agent::make_handler(cache.clone()),
        );
        let editor = EditorAccess::new(cache, None, vec![], Some(bus));

        // The write awaits the reply; here we drive the drain (the "tick") that
        // resolves it. Poll the drain until the queued request produces its
        // Effect, then collect the tool's result.
        let call =
            tokio::spawn(async move { open_file(&editor, &json!({ "filePath": "/a.rs" })).await });
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
