//! The handshake echoes the `mcp` subprotocol to a client that asks for it,
//! as `claude` does: Bun's WebSocket client drops a connection whose `101`
//! does not name the protocol it requested ("Missing client protocol"). A
//! client that asks for none gets none.
#![cfg(feature = "mcp")]
#![allow(clippy::unwrap_used, clippy::panic)]

use std::time::Duration;

use lattice_ai::mcp::auth::AUTH_HEADER;
use lattice_ai::mcp::lockfile::LockfileContents;
use lattice_ai::mcp::{ServerConfig, spawn};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

async fn port_and_token(dir: &std::path::Path) -> (u16, String) {
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

/// The `Sec-WebSocket-Protocol` the server answered with, when the client
/// sent `protocols` (or none).
async fn answered(protocols: Option<&str>) -> Option<String> {
    let dir = std::env::temp_dir().join(format!(
        "lattice-mcp-subproto-{}-{}",
        std::process::id(),
        protocols.unwrap_or("none").replace([',', ' '], "_")
    ));
    let handle = spawn(
        ServerConfig {
            workspace_folders: vec![],
            lock_dir: dir.clone(),
        },
        std::sync::Arc::new(lattice_runtime::EventBus::new()),
        &tokio::runtime::Handle::current(),
    );
    handle.start();
    let (port, token) = port_and_token(&dir).await;
    let mut request = format!("ws://127.0.0.1:{port}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert(AUTH_HEADER, HeaderValue::from_str(&token).unwrap());
    if let Some(p) = protocols {
        request
            .headers_mut()
            .insert("sec-websocket-protocol", HeaderValue::from_str(p).unwrap());
    }
    // tungstenite's client, like Bun's, refuses a `101` that names no
    // protocol when one was asked for, so connecting at all is half the test.
    let (_ws, response) = tokio_tungstenite::connect_async(request)
        .await
        .expect("the handshake succeeds");
    handle.stop();
    let _ = std::fs::remove_dir_all(&dir);
    response
        .headers()
        .get("sec-websocket-protocol")
        .map(|v| v.to_str().unwrap().to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_is_echoed_when_asked_for_and_nothing_otherwise() {
    assert_eq!(answered(Some("mcp")).await.as_deref(), Some("mcp"));
    // Unspaced: tungstenite's client does not trim its own list, so it would
    // take `, mcp` for ` mcp` and reject the answer. The server trims.
    assert_eq!(answered(Some("other,mcp")).await.as_deref(), Some("mcp"));
    assert_eq!(answered(None).await, None);
}
