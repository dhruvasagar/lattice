//! Shared test fixture: an in-process LSP server that the actor
//! integration tests can drive deterministically.
//!
//! ## Topology
//!
//! ```text
//! +---------+   duplex   +---------+
//! |  actor  |<---------->|  mock   |
//! +---------+            +---------+
//! ```
//!
//! `MockServer::start()` returns the editor-side `ServerHandle`
//! plus a `MockController` that lets the test:
//!
//! - register canned responses for specific methods,
//! - send server-initiated notifications,
//! - assert what the actor sent.

#![allow(clippy::unwrap_used, clippy::panic, dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use lsp_types::{
    InitializeResult, PositionEncodingKind, ServerCapabilities, ServerInfo,
    TextDocumentSyncCapability, TextDocumentSyncKind,
};
use serde_json::{Value, json};
use tokio::io::duplex;
use tokio::sync::Mutex;

use lattice_lsp::{
    LspLogger, LspReader, LspWriter, Message, Notification, Request, Response, ResponseError,
    ServerConfig, ServerHandle, jsonrpc::RequestId, spawn_with_io,
};

/// Per-method mock-response handler. Returns either a result
/// payload or a structured error.
pub type MockHandler = Arc<dyn Fn(Value) -> MockResult + Send + Sync>;

#[derive(Debug, Clone)]
pub enum MockResult {
    Ok(Value),
    Err(ResponseError),
}

/// Test-side controller for the mock server. Cheap to clone via
/// `Arc`-internal state; multiple test paths can drive the same
/// mock concurrently.
#[derive(Clone)]
pub struct MockController {
    state: Arc<Mutex<MockState>>,
    /// Channel into the mock task -- lets the test push
    /// server-initiated messages.
    push_tx: tokio::sync::mpsc::UnboundedSender<Message>,
}

struct MockState {
    /// Per-method response handlers. Methods not in the map get
    /// a generic METHOD_NOT_FOUND error.
    handlers: HashMap<String, MockHandler>,
    /// History of every request the actor sent (in arrival
    /// order). Tests assert against this.
    received_requests: Vec<Request>,
    /// History of every notification the actor sent.
    received_notifications: Vec<Notification>,
    /// History of every response the actor sent (e.g. to a
    /// server-initiated request).
    received_responses: Vec<Response>,
    /// Server-advertised capabilities; configurable per test.
    server_capabilities: ServerCapabilities,
}

impl MockController {
    /// Register a canned response handler for `method`.
    pub async fn on(
        &self,
        method: impl Into<String>,
        handler: impl Fn(Value) -> MockResult + Send + Sync + 'static,
    ) {
        self.state
            .lock()
            .await
            .handlers
            .insert(method.into(), Arc::new(handler));
    }

    /// Push a server-initiated notification into the mock --
    /// the actor will see it on its next read.
    pub fn push_notification(&self, method: impl Into<String>, params: Value) {
        let n = Notification::new(method, Some(params));
        let _ = self.push_tx.send(Message::Notification(n));
    }

    /// Push a server-initiated request.
    pub fn push_request(&self, id: i64, method: impl Into<String>, params: Value) {
        let r = Request::new(RequestId::Number(id), method, Some(params));
        let _ = self.push_tx.send(Message::Request(r));
    }

    /// Snapshot of every request the actor has sent so far.
    pub async fn requests(&self) -> Vec<Request> {
        self.state.lock().await.received_requests.clone()
    }

    pub async fn notifications(&self) -> Vec<Notification> {
        self.state.lock().await.received_notifications.clone()
    }

    pub async fn responses(&self) -> Vec<Response> {
        self.state.lock().await.received_responses.clone()
    }

    /// Override the server-advertised capabilities returned in
    /// the initialize response. Must be called before the actor
    /// is spawned.
    pub async fn set_server_capabilities(&self, caps: ServerCapabilities) {
        self.state.lock().await.server_capabilities = caps;
    }
}

/// Spawn a mock LSP server + actor pair joined by a duplex pipe.
/// Returns the editor-side `ServerHandle` and a `MockController`
/// for the test side.
pub struct MockServer {
    pub handle: ServerHandle,
    pub mock: MockController,
}

impl MockServer {
    pub async fn start() -> Self {
        Self::start_with_capabilities(default_server_capabilities()).await
    }

    /// Start with a caller-supplied logger so the test can
    /// inspect the per-server / global rings.
    pub async fn start_with_logger(logger: LspLogger) -> Self {
        Self::start_with_caps_and_logger(default_server_capabilities(), logger).await
    }

    pub async fn start_with_capabilities(caps: ServerCapabilities) -> Self {
        Self::start_with_caps_and_logger(caps, LspLogger::with_defaults()).await
    }

    /// Start with a custom server id (e.g. so two mocks
    /// running side-by-side don't collide in a `(uri,
    /// server_id)`-keyed layer).
    pub async fn start_with_id(id: impl Into<String>, logger: LspLogger) -> Self {
        Self::start_with_caps_logger_id(default_server_capabilities(), logger, id.into()).await
    }

    /// Inner helper -- both `start_with_capabilities` and
    /// `start_with_logger` route here.
    pub async fn start_with_caps_and_logger(caps: ServerCapabilities, logger: LspLogger) -> Self {
        Self::start_with_caps_logger_id(caps, logger, "mock".into()).await
    }

    /// Innermost helper. Lets a test customise everything.
    pub async fn start_with_caps_logger_id(
        caps: ServerCapabilities,
        logger: LspLogger,
        id: String,
    ) -> Self {
        // The actor side reads from `actor_read` and writes to
        // `actor_write`; the mock side reads from `mock_read` and
        // writes to `mock_write`. The duplex pipe ties the two
        // sides together so that bytes written on one are
        // readable on the other.
        let (actor_side, mock_side) = duplex(64 * 1024);
        let (actor_read, actor_write) = tokio::io::split(actor_side);
        let (mock_read, mock_write) = tokio::io::split(mock_side);

        let state = Arc::new(Mutex::new(MockState {
            handlers: HashMap::new(),
            received_requests: Vec::new(),
            received_notifications: Vec::new(),
            received_responses: Vec::new(),
            server_capabilities: caps,
        }));
        let (push_tx, push_rx) = tokio::sync::mpsc::unbounded_channel::<Message>();

        // Spawn the mock task: reads from the actor's writes,
        // dispatches per method.
        tokio::spawn(run_mock(
            LspReader::new(mock_read),
            LspWriter::new(mock_write),
            Arc::clone(&state),
            push_rx,
        ));

        let mock = MockController { state, push_tx };

        // Build a minimal config. Workspace root is the cwd of
        // the test runner -- doesn't matter for in-process tests
        // since we don't spawn a real binary.
        let config = ServerConfig::new(id, std::path::PathBuf::from("mock-server"), "rust");
        let workspace_root = std::env::current_dir().expect("cwd");

        let handle = spawn_with_io(
            config,
            workspace_root,
            LspReader::new(actor_read),
            LspWriter::new(actor_write),
            None,
            None,
            logger,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("mock spawn handshake should succeed");

        Self { handle, mock }
    }
}

/// Build default ServerCapabilities -- minimal LSP 3.17 surface
/// every test starts from. Tests override before `start_with_capabilities`.
pub fn default_server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        position_encoding: Some(PositionEncodingKind::UTF8),
        text_document_sync: Some(TextDocumentSyncCapability::Kind(
            TextDocumentSyncKind::INCREMENTAL,
        )),
        hover_provider: Some(lsp_types::HoverProviderCapability::Simple(true)),
        definition_provider: Some(lsp_types::OneOf::Left(true)),
        ..Default::default()
    }
}

/// The mock server task: reads messages from the actor and
/// either responds via canned handler or pushes a not-found
/// error.
async fn run_mock<R, W>(
    mut reader: LspReader<R>,
    mut writer: LspWriter<W>,
    state: Arc<Mutex<MockState>>,
    mut push_rx: tokio::sync::mpsc::UnboundedReceiver<Message>,
) where
    R: tokio::io::AsyncBufRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    loop {
        tokio::select! {
            biased;
            // Test pushed a server-initiated message.
            push = push_rx.recv() => {
                match push {
                    Some(msg) => {
                        if writer.write_message(&msg).await.is_err() { return; }
                    }
                    None => return,
                }
            }
            // Actor sent a message.
            res = reader.read_message() => {
                match res {
                    Ok(Some(Message::Request(req))) => {
                        let resp = build_response(&req, &state).await;
                        state.lock().await.received_requests.push(req);
                        if writer.write_message(&Message::Response(resp)).await.is_err() {
                            return;
                        }
                    }
                    Ok(Some(Message::Notification(n))) => {
                        state.lock().await.received_notifications.push(n);
                    }
                    Ok(Some(Message::Response(r))) => {
                        // The actor responded to one of OUR
                        // server-initiated requests. Capture it.
                        state.lock().await.received_responses.push(r);
                    }
                    Ok(None) => return, // actor closed the pipe
                    Err(_) => return,
                }
            }
        }
    }
}

async fn build_response(req: &Request, state: &Arc<Mutex<MockState>>) -> Response {
    // initialize is hard-coded to return the configured server
    // capabilities -- tests don't have to register a handler for
    // it on every spawn.
    if req.method == "initialize" {
        let caps = state.lock().await.server_capabilities.clone();
        let result = InitializeResult {
            capabilities: caps,
            server_info: Some(ServerInfo {
                name: "mock-server".into(),
                version: Some("0.0.0".into()),
            }),
        };
        let value = serde_json::to_value(result).unwrap();
        return Response::ok(req.id.clone(), value);
    }
    if req.method == "shutdown" {
        return Response::ok(req.id.clone(), json!(null));
    }
    let handler = state.lock().await.handlers.get(&req.method).cloned();
    match handler {
        Some(h) => match h(req.params.clone().unwrap_or(Value::Null)) {
            MockResult::Ok(v) => Response::ok(req.id.clone(), v),
            MockResult::Err(e) => Response::err(req.id.clone(), e),
        },
        None => Response::err(
            req.id.clone(),
            ResponseError {
                code: lattice_lsp::jsonrpc::error_codes::METHOD_NOT_FOUND,
                message: format!("mock: no handler for {}", req.method),
                data: None,
            },
        ),
    }
}
