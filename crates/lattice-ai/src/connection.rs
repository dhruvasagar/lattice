//! ACP connection adapter over the `agent-client-protocol` crate.
//!
//! The crate frames JSON-RPC over stdio itself (newline-delimited JSON) and drives a
//! connection through a closure-based API: `Client.builder()...connect_with(transport,
//! async |cx| { ... })`. That closure owns the connection for as long as it runs — once it
//! returns, the whole connection (including its background dispatch loop) shuts down. That
//! doesn't fit the shape Tasks 4/5/7 need: a `Connection` handle with independent async
//! methods (`initialize`, `new_session`, `prompt`) callable at any time from any task.
//!
//! This module bridges the two shapes: [`Connection::spawn`] starts a background "driver"
//! task that runs the crate's `connect_with` closure as a command loop for the connection's
//! whole lifetime, and the public methods send commands into that loop over a channel and
//! await the reply.
//!
//! ## Threading
//!
//! `agent-client-protocol` builds its connection state on `futures::channel::mpsc` (not
//! tokio) and requires spawned work to be `Send + 'static` (see the crate's internal
//! `Task::new` bound). That makes the whole stack executor-agnostic and `Send`, so the driver
//! task runs on a plain `tokio::spawn` — no dedicated thread or `LocalSet` is required.
//!
//! `Connection::spawn` takes generic tokio `AsyncRead`/`AsyncWrite` halves and adapts them to
//! the `futures::io` traits the crate's `ByteStreams` transport expects via
//! `tokio_util::compat`.
//!
//! ## Notifications
//!
//! `session/update` notifications are handled once, connection-wide, via a single
//! `on_receive_notification` handler registered on the builder (the crate does not scope
//! notification handlers per-session at this layer). Every notification the agent sends for
//! the life of the connection is forwarded to the `mpsc::UnboundedReceiver<SessionNotification>`
//! returned by [`Connection::spawn`]; callers that run multiple sessions on one connection
//! must filter by `SessionNotification::session_id` themselves. The channel is unbounded (not
//! bounded) because `on_receive_notification` runs inside the crate's single dispatch loop —
//! see the comment at the channel construction site in [`Connection::spawn`] for why a bounded
//! channel would risk deadlocking every in-flight request.

use std::sync::Arc;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest, TextContent,
};
use agent_client_protocol::{ByteStreams, Client};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::error::{AiError, Result};

/// Re-exported so callers (Task 6) can match on `session/update` payloads without depending
/// on `agent-client-protocol` directly.
pub use agent_client_protocol::schema::v1::SessionNotification;

/// A lattice-local session identifier.
///
/// Kept distinct from `agent_client_protocol::schema::v1::SessionId` (which wraps an
/// `Arc<str>` and carries protocol-schema derives) so the rest of `lattice-ai` doesn't need
/// to depend on the wire type directly.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(pub String);

/// A command sent from a `Connection`'s public async methods to the driver task.
enum DriverCommand {
    Initialize {
        reply: oneshot::Sender<Result<()>>,
    },
    NewSession {
        cwd: String,
        reply: oneshot::Sender<Result<SessionId>>,
    },
    Prompt {
        session_id: SessionId,
        text: String,
        reply: oneshot::Sender<Result<()>>,
    },
}

/// A handle to a live ACP connection driven by the `agent-client-protocol` crate.
///
/// Constructed with [`Connection::spawn`], which owns the transport and runs the crate's
/// JSON-RPC dispatch loop on a background task for the connection's whole lifetime.
///
/// ## Single-flight, single-session limitation
///
/// The driver's command loop (spawned by [`Connection::spawn`]) processes one
/// `DriverCommand` end-to-end at a time: it awaits the reply to `initialize` / `new_session`
/// / `prompt` before pulling the next command off the channel. Notifications are
/// connection-wide, not scoped to a session (see the module-level docs). A future
/// multi-session `Connection` needs to (a) filter delivered notifications by
/// `SessionNotification::session_id` and (b) revisit this single-flight loop so one
/// session's in-flight request doesn't block another session's commands.
pub struct Connection {
    commands: mpsc::Sender<DriverCommand>,
}

impl Connection {
    /// Spawn a driver task that adapts `reader`/`writer` into an ACP transport and drives
    /// the `agent-client-protocol` client connection over it.
    ///
    /// Returns a `Connection` handle plus a receiver that yields every `session/update`
    /// notification the agent sends for the lifetime of the connection.
    pub fn spawn<R, W>(
        reader: R,
        writer: W,
    ) -> (
        Arc<Connection>,
        mpsc::UnboundedReceiver<SessionNotification>,
    )
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<DriverCommand>(32);
        // Unbounded, not bounded: `on_receive_notification` below runs *inside* the crate's
        // single dispatch loop, which also routes responses to in-flight `initialize` /
        // `new_session` / `prompt` calls. A bounded channel's `.send().await` would block that
        // loop (and therefore every pending/future request) if the notification consumer ever
        // falls behind by a full channel's worth of messages — very plausible during
        // token-by-token `session/update` streaming. Spawning a task per notification instead
        // would avoid blocking but let streaming chunks complete out of order, corrupting
        // assistant message text; unbounded `send` is both non-blocking and order-preserving.
        let (notif_tx, notif_rx) = mpsc::unbounded_channel::<SessionNotification>();

        tokio::spawn(async move {
            let transport = ByteStreams::new(writer.compat_write(), reader.compat());

            let result = Client
                .builder()
                .on_receive_notification(
                    async move |notification: SessionNotification, _cx| {
                        // Synchronous, non-blocking send on the unbounded channel: this
                        // handler runs inside the crate's single dispatch loop, so it must
                        // never await here (see the channel-construction comment above).
                        // Ignore send errors: if the caller dropped the receiver they've
                        // opted out of notifications, not out of the connection.
                        let _ = notif_tx.send(notification);
                        Ok(())
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                .connect_with(transport, async move |cx| {
                    while let Some(cmd) = cmd_rx.recv().await {
                        match cmd {
                            DriverCommand::Initialize { reply } => {
                                let outcome = cx
                                    .send_request(InitializeRequest::new(ProtocolVersion::V1))
                                    .block_task()
                                    .await
                                    .map(|_response| ())
                                    .map_err(map_acp_error);
                                let _ = reply.send(outcome);
                            }
                            DriverCommand::NewSession { cwd, reply } => {
                                let outcome = cx
                                    .send_request(NewSessionRequest::new(cwd))
                                    .block_task()
                                    .await
                                    .map(|response| SessionId(response.session_id.0.to_string()))
                                    .map_err(map_acp_error);
                                let _ = reply.send(outcome);
                            }
                            DriverCommand::Prompt {
                                session_id,
                                text,
                                reply,
                            } => {
                                let outcome = cx
                                    .send_request(PromptRequest::new(
                                        session_id.0,
                                        vec![ContentBlock::Text(TextContent::new(text))],
                                    ))
                                    .block_task()
                                    .await
                                    .map(|_response| ())
                                    .map_err(map_acp_error);
                                let _ = reply.send(outcome);
                            }
                        }
                    }
                    Ok(())
                })
                .await;

            if let Err(err) = result {
                tracing::debug!(%err, "ACP connection driver exited");
            }
        });

        (Arc::new(Connection { commands: cmd_tx }), notif_rx)
    }

    /// Send the ACP `initialize` handshake.
    pub async fn initialize(&self) -> Result<()> {
        self.call(|reply| DriverCommand::Initialize { reply }).await
    }

    /// Create a new session rooted at `cwd`.
    pub async fn new_session(&self, cwd: &str) -> Result<SessionId> {
        let cwd = cwd.to_string();
        self.call(|reply| DriverCommand::NewSession { cwd, reply })
            .await
    }

    /// Send a text prompt to `session`.
    pub async fn prompt(&self, session: &SessionId, text: &str) -> Result<()> {
        let session_id = session.clone();
        let text = text.to_string();
        self.call(|reply| DriverCommand::Prompt {
            session_id,
            text,
            reply,
        })
        .await
    }

    /// Send `make_command(reply)` to the driver task and await its reply.
    async fn call<T>(
        &self,
        make_command: impl FnOnce(oneshot::Sender<Result<T>>) -> DriverCommand,
    ) -> Result<T> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.commands
            .send(make_command(reply_tx))
            .await
            .map_err(|_| AiError::Transport("ACP connection driver task is gone".into()))?;
        reply_rx.await.map_err(|_| {
            AiError::Transport("ACP connection driver dropped the reply channel".into())
        })?
    }
}

fn map_acp_error(err: agent_client_protocol::Error) -> AiError {
    AiError::Protocol(err.to_string())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::*;

    /// Drives the "agent" side of a mocked duplex ACP connection: reads newline-delimited
    /// JSON-RPC requests, replies with canned responses, and pushes an unsolicited
    /// `session/update` notification right after answering `session/new` — mirroring how a
    /// real agent streams updates once a session exists.
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
                    "result": { "sessionId": "sess-1" },
                })),
                "session/prompt" => Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "stopReason": "end_turn" },
                })),
                _ => None,
            };

            if let Some(response) = response {
                write_line(reader.get_mut(), &response).await;
            }

            if method == "session/new" {
                let notification = json!({
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": "sess-1",
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": { "type": "text", "text": "hello from agent" },
                        },
                    },
                });
                write_line(reader.get_mut(), &notification).await;
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
    async fn initialize_completes_the_handshake() {
        let (connection, _notif_rx) = spawn_connection_with_mock_peer();
        connection
            .initialize()
            .await
            .expect("initialize should succeed");
    }

    #[tokio::test]
    async fn new_session_returns_the_peers_session_id() {
        let (connection, _notif_rx) = spawn_connection_with_mock_peer();
        connection
            .initialize()
            .await
            .expect("initialize should succeed");

        let session = connection
            .new_session("/tmp")
            .await
            .expect("new_session should succeed");
        assert_eq!(session.0, "sess-1");
    }

    #[tokio::test]
    async fn session_update_notification_reaches_the_receiver() {
        let (connection, mut notif_rx) = spawn_connection_with_mock_peer();
        connection
            .initialize()
            .await
            .expect("initialize should succeed");
        connection
            .new_session("/tmp")
            .await
            .expect("new_session should succeed");

        let notification = tokio::time::timeout(Duration::from_secs(5), notif_rx.recv())
            .await
            .expect("a session/update notification should arrive before the timeout")
            .expect("the notification channel should still be open");

        assert_eq!(notification.session_id.0.to_string(), "sess-1");
    }

    #[tokio::test]
    async fn prompt_round_trips_through_the_mock_peer() {
        let (connection, _notif_rx) = spawn_connection_with_mock_peer();
        connection
            .initialize()
            .await
            .expect("initialize should succeed");
        let session = connection
            .new_session("/tmp")
            .await
            .expect("new_session should succeed");

        connection
            .prompt(&session, "hi")
            .await
            .expect("prompt should succeed");
    }
}
