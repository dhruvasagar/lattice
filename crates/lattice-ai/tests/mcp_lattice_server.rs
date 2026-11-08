//! LL.22: the Claude Code connection on rmcp, beyond the I1 roundtrip
//! (`mcp_ws_roundtrip.rs`): what a real client sees over the socket.
//!
//! The version stays `2024-11-05` when a client asks for newer;
//! `notifications/initialized` is answered with `tools/list_changed`; the
//! tool list on the wire is the catalogue; `selection_changed` and
//! `at_mentioned` still reach the client after rmcp's handshake; an unknown
//! tool is `METHOD_NOT_FOUND`; and a frame rmcp cannot read is answered, not
//! left hanging.
#![cfg(feature = "mcp")]
#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use lattice_ai::mcp::auth::AUTH_HEADER;
use lattice_ai::mcp::lockfile::LockfileContents;
use lattice_ai::mcp::{ClaudeCodeServerHandle, ServerConfig, spawn};
use lattice_runtime::EventBus;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn unique_dir() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    std::env::temp_dir().join(format!(
        "lattice-mcp-ls-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ))
}

async fn wait_for_lockfile(dir: &Path) -> (u16, String) {
    for _ in 0..500 {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("lock") {
                continue;
            }
            if let Some(c) = std::fs::read(&path)
                .ok()
                .and_then(|raw| serde_json::from_slice::<LockfileContents>(&raw).ok())
            {
                let port = path.file_stem().unwrap().to_str().unwrap().parse().unwrap();
                return (port, c.auth_token);
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no lockfile under {}", dir.display());
}

async fn connected() -> (ClaudeCodeServerHandle, Ws, PathBuf) {
    let dir = unique_dir();
    let handle = spawn(
        ServerConfig {
            workspace_folders: vec!["/tmp/ls-project".into()],
            lock_dir: dir.clone(),
        },
        Arc::new(EventBus::new()),
        &tokio::runtime::Handle::current(),
    );
    handle.start();
    let (port, token) = wait_for_lockfile(&dir).await;
    let mut request = format!("ws://127.0.0.1:{port}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert(AUTH_HEADER, HeaderValue::from_str(&token).unwrap());
    let (ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    (handle, ws, dir)
}

async fn send(ws: &mut Ws, v: Value) {
    ws.send(WsMessage::Text(v.to_string())).await.unwrap();
}

async fn send_raw(ws: &mut Ws, text: &str) {
    ws.send(WsMessage::Text(text.to_string())).await.unwrap();
}

/// The next text frame, as JSON, within two seconds.
async fn next(ws: &mut Ws) -> Value {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .expect("a frame within two seconds")
            .expect("the socket is open")
            .unwrap();
        if let Ok(text) = frame.to_text()
            && !text.is_empty()
        {
            return serde_json::from_str(text).unwrap();
        }
    }
}

fn initialize(version: &str) -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": version,
            "capabilities": {},
            "clientInfo": { "name": "claude-code", "version": "2.1.319" },
        },
    })
}

async fn handshake(ws: &mut Ws, version: &str) -> Value {
    send(ws, initialize(version)).await;
    let r = next(ws).await;
    send(
        ws,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    )
    .await;
    r
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_newer_client_still_gets_the_ide_version_and_then_the_tool_list() {
    let (handle, mut ws, dir) = connected().await;
    let init = handshake(&mut ws, "2025-06-18").await;
    assert_eq!(init["id"], 1);
    assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(init["result"]["serverInfo"]["name"], "lattice");

    // `initialized` is answered with `tools/list_changed`, as before.
    let changed = next(&mut ws).await;
    assert_eq!(changed["method"], "notifications/tools/list_changed");

    send(
        &mut ws,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await;
    let list = next(&mut ws).await;
    let served: Vec<Value> = list["result"]["tools"].as_array().unwrap().clone();
    let catalogue: Vec<Value> = lattice_ai::mcp::lattice_server::catalogue()
        .iter()
        .map(|t| serde_json::to_value(t).unwrap())
        .collect();
    assert_eq!(
        served, catalogue,
        "the wire carries the catalogue, by name and schema"
    );
    assert_eq!(served.len(), 10);

    handle.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selection_and_at_mention_frames_reach_the_client() {
    use lattice_protocol::SelectionSet;
    use lattice_protocol::position::Position;
    use lattice_protocol::selection::Selection;

    let (handle, mut ws, dir) = connected().await;
    handshake(&mut ws, "2024-11-05").await;
    assert_eq!(
        next(&mut ws).await["method"],
        "notifications/tools/list_changed"
    );

    let sel = SelectionSet::single(Selection {
        anchor: Position { line: 1, byte: 0 },
        head: Position { line: 3, byte: 2 },
        visual: None,
    });
    let path = Some(Path::new("/tmp/ls-project/a.rs"));
    handle.notify(lattice_ai::mcp::notifications::selection_changed_frame(
        &sel, path,
    ));
    handle.notify(lattice_ai::mcp::notifications::at_mentioned_frame(
        &sel, path,
    ));

    let a = next(&mut ws).await;
    assert_eq!(a["method"], "selection_changed");
    assert_eq!(a["params"]["filePath"], "/tmp/ls-project/a.rs");
    let b = next(&mut ws).await;
    assert_eq!(b["method"], "at_mentioned");

    // Requests still flow beside pushed frames.
    send(
        &mut ws,
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"name": "getWorkspaceFolders", "arguments": {}}}),
    )
    .await;
    let r = next(&mut ws).await;
    assert_eq!(r["id"], 3);
    assert!(
        r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("/tmp/ls-project")
    );

    handle.stop();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_tools_and_unreadable_frames_are_answered() {
    let (handle, mut ws, dir) = connected().await;

    // Before initialize: an `initialize` missing `clientInfo` is refused with
    // its id (rmcp reads it as an unknown request and names the metadata it
    // lacks), not met with silence, and the connection survives it.
    send(
        &mut ws,
        json!({"jsonrpc": "2.0", "id": 7, "method": "initialize",
               "params": {"protocolVersion": "2024-11-05"}}),
    )
    .await;
    let bad = next(&mut ws).await;
    assert_eq!(bad["id"], 7);
    assert_eq!(bad["error"]["code"], -32602, "{bad}");

    send_raw(
        &mut ws,
        r#"{"jsonrpc": "2.0", "id": {"odd": 1}, "method": "tools/list"}"#,
    )
    .await;
    let odd = next(&mut ws).await;
    assert_eq!(odd["id"], json!({"odd": 1}));
    assert_eq!(odd["error"]["code"], -32600, "{odd}");

    send_raw(&mut ws, "{ this is not json").await;
    let garbage = next(&mut ws).await;
    assert_eq!(garbage["id"], Value::Null);
    assert_eq!(garbage["error"]["code"], -32700);

    handshake(&mut ws, "2024-11-05").await;
    assert_eq!(
        next(&mut ws).await["method"],
        "notifications/tools/list_changed"
    );
    send(
        &mut ws,
        json!({"jsonrpc": "2.0", "id": 8, "method": "tools/call",
               "params": {"name": "noSuchTool", "arguments": {}}}),
    )
    .await;
    let unknown = next(&mut ws).await;
    assert_eq!(unknown["id"], 8);
    assert_eq!(unknown["error"]["code"], -32601);

    handle.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
