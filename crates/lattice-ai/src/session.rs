//! ACP session lifecycle: the `initialize -> session/new` handshake and prompt,
//! as free functions over a `Connection` (Tasks 6/7 import these paths).

use crate::connection::Connection;
use crate::error::Result;

pub use crate::connection::SessionId;

/// Run the ACP handshake and open a session rooted at `cwd`.
pub async fn handshake(conn: &Connection, cwd: &str) -> Result<SessionId> {
    conn.initialize().await?;
    conn.new_session(cwd).await
}

/// Send a user prompt into `session`.
pub async fn prompt(conn: &Connection, session: &SessionId, text: &str) -> Result<()> {
    conn.prompt(session, text).await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::sync::mpsc;

    use super::*;
    use crate::connection::SessionNotification;

    /// Drives the "agent" side of a mocked duplex ACP connection: reads
    /// newline-delimited JSON-RPC requests and replies with canned responses.
    /// Mirrors `connection::tests::run_mock_peer` (same framing, scoped to the
    /// requests the handshake sends).
    async fn run_mock_peer(peer: tokio::io::DuplexStream) {
        let mut reader = BufReader::new(peer);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let request: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let method = request.get("method").and_then(Value::as_str).unwrap_or("");
            let id = request.get("id").cloned().unwrap_or(Value::Null);

            let response = match method {
                "initialize" => Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "protocolVersion": 1 },
                })),
                "session/new" => Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "sessionId": "sess-42" },
                })),
                _ => None,
            };

            if let Some(response) = response {
                write_line(reader.get_mut(), &response).await;
            }
        }
    }

    async fn write_line(writer: &mut tokio::io::DuplexStream, value: &Value) {
        let mut line = value.to_string();
        line.push('\n');
        let _ = writer.write_all(line.as_bytes()).await;
    }

    fn spawn_connection_with_mock_peer() -> (
        Arc<Connection>,
        mpsc::UnboundedReceiver<SessionNotification>,
    ) {
        let (ours, mock) = tokio::io::duplex(8192);
        let (reader, writer) = tokio::io::split(ours);
        let (connection, notif_rx) = Connection::spawn(reader, writer);
        tokio::spawn(run_mock_peer(mock));
        (connection, notif_rx)
    }

    #[tokio::test]
    async fn handshake_returns_session_id() {
        let (connection, _notif_rx) = spawn_connection_with_mock_peer();

        let session = handshake(&connection, "/work")
            .await
            .expect("handshake should succeed");

        assert_eq!(session, SessionId("sess-42".to_string()));
    }

    /// Live end-to-end check against a real `opencode acp` subprocess. Not run
    /// in CI (requires the opencode binary + an authenticated session).
    ///
    /// Run via `cargo test -p lattice-ai -- --ignored opencode_end_to_end`.
    #[ignore]
    #[tokio::test(flavor = "multi_thread")]
    async fn opencode_end_to_end() {
        use tokio::process::Command;

        let mut child = Command::new("/Users/dhruva/.opencode/bin/opencode")
            .arg("acp")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("opencode acp should spawn");

        let stdin = child.stdin.take().expect("child stdin should be piped");
        let stdout = child.stdout.take().expect("child stdout should be piped");

        let (connection, mut notif_rx) = Connection::spawn(stdout, stdin);

        let session = handshake(&connection, ".")
            .await
            .expect("handshake should succeed");

        prompt(&connection, &session, "reply with the single word: pong")
            .await
            .expect("prompt should succeed");

        let notification = tokio::time::timeout(Duration::from_secs(30), notif_rx.recv())
            .await
            .expect("a session/update notification should arrive before the timeout")
            .expect("the notification channel should still be open");

        assert_eq!(notification.session_id.0.to_string(), session.0);

        let _ = child.kill().await;
    }
}
