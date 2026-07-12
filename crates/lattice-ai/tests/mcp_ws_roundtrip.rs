//! IDE-protocol I1.3 integration test: a real in-process WebSocket
//! round-trip against the spawned MCP server. Compiled only with the `mcp`
//! feature (it drives `lattice_ai::mcp`).
//!
//! Spawns the server supervisor, starts it, reads the discovery lockfile
//! to learn the ephemeral port + token, connects a `tokio-tungstenite`
//! client with the auth header, and drives `initialize` + `tools/list`.
//! Also covers auth rejection and the lockfile start/stop lifecycle —
//! the parts of `transport` + `server` not reachable by the pure
//! unit tests.
#![cfg(feature = "mcp")]
#![allow(clippy::unwrap_used, clippy::panic, clippy::collapsible_if)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use lattice_ai::mcp::auth::AUTH_HEADER;
use lattice_ai::mcp::lockfile::LockfileContents;
use lattice_ai::mcp::{ServerConfig, spawn};
use lattice_protocol::jsonrpc::{Message, Request, RequestId};
use lattice_runtime::EventBus;
use serde_json::json;
use std::sync::Arc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

fn unique_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!(
        "lattice-cc-it-{}-{}-{}",
        tag,
        std::process::id(),
        n
    ))
}

/// A fresh, empty event bus — these tests exercise the handshake / auth /
/// lockfile / dispatch routing, not the read cache, so no events are driven.
fn bus() -> Arc<EventBus> {
    Arc::new(EventBus::new())
}

/// Poll the lock dir until a `<port>.lock` appears; return (port, token).
async fn wait_for_lockfile(dir: &Path) -> (u16, String) {
    for _ in 0..500 {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("lock") {
                    if let Ok(raw) = std::fs::read(&path) {
                        if let Ok(contents) = serde_json::from_slice::<LockfileContents>(&raw) {
                            let port: u16 = path
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .and_then(|s| s.parse().ok())
                                .expect("port from lock filename");
                            return (port, contents.auth_token);
                        }
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("lockfile did not appear under {}", dir.display());
}

async fn send_request<S>(ws: &mut S, id: u64, method: &str, params: Option<serde_json::Value>)
where
    S: SinkExt<WsMessage> + Unpin,
    <S as futures::Sink<WsMessage>>::Error: std::fmt::Debug,
{
    let req = Request::new(RequestId::from_u64(id), method, params);
    let text = serde_json::to_string(&req).unwrap();
    ws.send(WsMessage::Text(text)).await.expect("send");
}

async fn read_response<S>(ws: &mut S) -> lattice_protocol::jsonrpc::Response
where
    S: StreamExt<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let frame = ws.next().await.expect("a frame").expect("ok frame");
    let text = frame.to_text().expect("text frame");
    match Message::from_json(text.as_bytes()).expect("valid json-rpc") {
        Message::Response(r) => r,
        other => panic!("expected a response, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ws_handshake_initialize_and_tools_list() {
    let dir = unique_dir("rt");
    let handle = spawn(
        ServerConfig {
            workspace_folders: vec!["/tmp/ws-project".to_string()],
            lock_dir: dir.clone(),
        },
        bus(),
        &tokio::runtime::Handle::current(),
    );
    handle.start();
    let (port, token) = wait_for_lockfile(&dir).await;

    let mut request = format!("ws://127.0.0.1:{port}")
        .into_client_request()
        .expect("client request");
    request
        .headers_mut()
        .insert(AUTH_HEADER, HeaderValue::from_str(&token).unwrap());
    let (mut ws, _resp) = tokio_tungstenite::connect_async(request)
        .await
        .expect("authorized connect succeeds");

    // initialize handshake.
    send_request(
        &mut ws,
        1,
        "initialize",
        Some(json!({ "protocolVersion": "2024-11-05" })),
    )
    .await;
    let resp = read_response(&mut ws).await;
    assert_eq!(resp.id, RequestId::Number(1));
    let result = resp.result.expect("initialize result");
    assert_eq!(result["protocolVersion"], "2024-11-05");
    assert_eq!(result["capabilities"]["tools"]["listChanged"], json!(true));

    // tools/list enumerates the catalog.
    send_request(&mut ws, 2, "tools/list", None).await;
    let resp = read_response(&mut ws).await;
    let tools = resp.result.expect("tools result")["tools"]
        .as_array()
        .expect("tools array")
        .len();
    assert!(tools >= 9, "expected the full tool catalog, got {tools}");

    // tools/call getWorkspaceFolders → routed end-to-end through dispatch +
    // the MCP content envelope; the configured workspace folder appears.
    send_request(
        &mut ws,
        3,
        "tools/call",
        Some(json!({ "name": "getWorkspaceFolders", "arguments": {} })),
    )
    .await;
    let resp = read_response(&mut ws).await;
    let result = resp.result.expect("tool result");
    assert_eq!(result["isError"], json!(false));
    let text = result["content"][0]["text"]
        .as_str()
        .expect("text content block");
    assert!(text.contains("/tmp/ws-project"), "got {text}");

    handle.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broadcast_notification_reaches_a_connected_client() {
    // I6.0: a server-initiated frame pushed via `handle.notify` is delivered to
    // a connected client over the same writer that carries responses.
    let dir = unique_dir("notify");
    let handle = spawn(
        ServerConfig {
            workspace_folders: vec![],
            lock_dir: dir.clone(),
        },
        bus(),
        &tokio::runtime::Handle::current(),
    );
    handle.start();
    let (port, token) = wait_for_lockfile(&dir).await;

    let mut request = format!("ws://127.0.0.1:{port}")
        .into_client_request()
        .expect("client request");
    request
        .headers_mut()
        .insert(AUTH_HEADER, HeaderValue::from_str(&token).unwrap());
    let (mut ws, _resp) = tokio_tungstenite::connect_async(request)
        .await
        .expect("authorized connect succeeds");

    // Wait until the connection has subscribed its broadcast receiver (the
    // subscribe happens in the accept loop right after accept).
    let mut subscribed = false;
    for _ in 0..500 {
        if handle.connection_count() >= 1 {
            subscribed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(subscribed, "the connection subscribed a broadcast receiver");

    // Push a server-initiated notification frame; the client receives it.
    let frame = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "method": "selection_changed",
        "params": { "marker": "i6-broadcast" }
    }))
    .unwrap();
    handle.notify(frame);

    let received = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("a frame arrives within the timeout")
        .expect("a frame")
        .expect("ok frame");
    let text = received.to_text().expect("text frame");
    assert!(text.contains("i6-broadcast"), "got {text}");

    handle.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_closes_live_connections() {
    // I7: `:claude-code-stop` must disconnect a live agent, not leave it
    // functional against a stopped server. Dropping `RunningServer` drops the
    // shutdown sender → the connection's read-loop select closes the socket.
    let dir = unique_dir("close");
    let handle = spawn(
        ServerConfig {
            workspace_folders: vec![],
            lock_dir: dir.clone(),
        },
        bus(),
        &tokio::runtime::Handle::current(),
    );
    handle.start();
    let (port, token) = wait_for_lockfile(&dir).await;

    let mut request = format!("ws://127.0.0.1:{port}")
        .into_client_request()
        .expect("client request");
    request
        .headers_mut()
        .insert(AUTH_HEADER, HeaderValue::from_str(&token).unwrap());
    let (mut ws, _resp) = tokio_tungstenite::connect_async(request)
        .await
        .expect("authorized connect succeeds");

    // Wait until the connection is established (subscribed its receivers).
    for _ in 0..500 {
        if handle.connection_count() >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(handle.connection_count() >= 1, "connection established");

    handle.stop();

    // The client's read stream ends (or errors / receives a close frame) — the
    // server closed the connection.
    let closed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match ws.next().await {
                None => return true,                        // stream ended
                Some(Err(_)) => return true,                // socket error
                Some(Ok(f)) if f.is_close() => return true, // close frame
                Some(Ok(_)) => continue,                    // ignore stray frames
            }
        }
    })
    .await
    .expect("the connection closes within the timeout");
    assert!(closed, "stop closed the live connection");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_token_is_rejected() {
    let dir = unique_dir("auth");
    let handle = spawn(
        ServerConfig {
            workspace_folders: vec![],
            lock_dir: dir.clone(),
        },
        bus(),
        &tokio::runtime::Handle::current(),
    );
    handle.start();
    let (port, _token) = wait_for_lockfile(&dir).await;

    let mut request = format!("ws://127.0.0.1:{port}")
        .into_client_request()
        .expect("client request");
    request
        .headers_mut()
        .insert(AUTH_HEADER, HeaderValue::from_static("not-the-real-token"));
    let result = tokio_tungstenite::connect_async(request).await;
    assert!(
        result.is_err(),
        "connect with a wrong token must be rejected at the handshake"
    );

    handle.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Resilience: a blocking `openDiff` review must NOT freeze the connection,
/// and dropping the connection mid-review must reject + close that session's
/// diffs (so the agent is never stranded and panes don't leak). This is the
/// `(A)` decoupling — `openDiff` runs on its own task while the read loop keeps
/// serving other frames; teardown fires `closeAllDiffTabs` for the connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_diff_does_not_block_loop_and_drop_rejects_pending_diff() {
    use lattice_agent::{EditorWriteRequest, InboundKind};
    use lattice_diff::{ProgrammaticDiffBus, ProgrammaticDiffRequest};
    use lattice_mode::inbound::make_inbound_raw;
    use tokio::sync::Notify;

    let dir = unique_dir("resilient");
    let handle = spawn(
        ServerConfig {
            workspace_folders: vec![],
            lock_dir: dir.clone(),
        },
        bus(),
        &tokio::runtime::Handle::current(),
    );
    // Wire a writes bus + diff bus so `openDiff` reaches a (test-played) host
    // and the teardown's `closeAllDiffTabs` is observable.
    let (writes_bus, mut writes_rx) =
        make_inbound_raw::<EditorWriteRequest>(Arc::new(Notify::new()));
    let (diff_bus, mut diff_rx): (ProgrammaticDiffBus, _) =
        make_inbound_raw::<ProgrammaticDiffRequest>(Arc::new(Notify::new()));
    handle.install_services(None, None, writes_bus, Some(diff_bus));
    handle.start();
    let (port, token) = wait_for_lockfile(&dir).await;

    let mut request = format!("ws://127.0.0.1:{port}")
        .into_client_request()
        .expect("client request");
    request
        .headers_mut()
        .insert(AUTH_HEADER, HeaderValue::from_str(&token).unwrap());
    let (mut ws, _resp) = tokio_tungstenite::connect_async(request)
        .await
        .expect("authorized connect succeeds");

    // Fire openDiff (id=10). It blocks server-side awaiting the verdict.
    send_request(
        &mut ws,
        10,
        "tools/call",
        Some(json!({
            "name": "openDiff",
            "arguments": { "old_file_path": "/a.rs", "new_file_contents": "x", "tab_name": "t" },
        })),
    )
    .await;
    // The host (this test) receives the programmatic diff request — DON'T
    // resolve it; the review stays pending.
    let diff_req = tokio::time::timeout(Duration::from_secs(5), diff_rx.recv())
        .await
        .expect("diff request arrives")
        .expect("diff bus open");
    assert_eq!(diff_req.origin_session, 1, "first connection's conn_id");

    // The loop is NOT blocked: a follow-up tools/list responds while the review
    // is still pending. (Before the fix this would deadlock.)
    send_request(&mut ws, 11, "tools/list", None).await;
    let resp = tokio::time::timeout(Duration::from_secs(5), read_response(&mut ws))
        .await
        .expect("tools/list responds while a review is pending");
    assert_eq!(resp.id, RequestId::Number(11));

    // Drop the connection mid-review. Teardown must reject + close this
    // connection's diffs via `closeAllDiffTabs` on the writes bus.
    drop(ws);
    let inbound = tokio::time::timeout(Duration::from_secs(5), writes_rx.recv())
        .await
        .expect("teardown fires a write")
        .expect("writes bus open");
    assert!(
        matches!(inbound.kind, InboundKind::CloseAllDiffTabs { origin_session } if origin_session == 1),
        "dropped connection rejects its own pending diffs",
    );

    handle.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_writes_lockfile_and_stop_removes_it() {
    let dir = unique_dir("life");
    let handle = spawn(
        ServerConfig {
            workspace_folders: vec![],
            lock_dir: dir.clone(),
        },
        bus(),
        &tokio::runtime::Handle::current(),
    );
    handle.start();
    let (port, _token) = wait_for_lockfile(&dir).await;
    let lock_path = dir.join(format!("{port}.lock"));
    assert!(lock_path.exists(), "start writes the discovery lockfile");
    assert!(handle.snapshot().running);

    handle.stop();
    // The supervisor drops RunningServer asynchronously; wait briefly.
    let mut removed = false;
    for _ in 0..500 {
        if !lock_path.exists() {
            removed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(removed, "stop unlinks the discovery lockfile");
    let _ = std::fs::remove_dir_all(&dir);
}
