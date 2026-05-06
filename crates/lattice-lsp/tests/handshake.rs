#![allow(clippy::unwrap_used, clippy::panic)]
//! Integration tests for the language-server actor + initialize
//! handshake (Phase 4.1.b). These run against the in-process
//! `MockServer` fixture so they are fully deterministic and
//! don't require any external binary on PATH.
//!
//! End-to-end coverage:
//!
//! - Successful handshake (capabilities returned + `initialized`
//!   notification observed by mock)
//! - Server-side initialize error → spawn returns
//!   `LspError::HandshakeFailed`
//! - Request → response correlation across many in-flight ids
//! - Server-initiated request gets METHOD_NOT_FOUND response
//! - `client/registerCapability` accepted with null result
//! - `workspace/configuration` returns one entry per requested
//!   item
//! - `$/cancelRequest` resolves the pending oneshot with
//!   `LspError::Cancelled`
//! - `shutdown()` runs the LSP shutdown sequence and the mock
//!   sees `shutdown` request + `exit` notification
//! - All pending requests after shutdown get `ActorGone`
//! - Server crashes (mock pipe closed) -- pending requests
//!   resolve with the appropriate error variant
//!
//! Position-encoding negotiation tests: utf-8 preferred when
//! advertised, utf-16 fallback when not.

mod common;

use std::time::Duration;

use lsp_types::{HoverParams, Position, ServerCapabilities, TextDocumentIdentifier};
use serde_json::{Value, json};

use lattice_lsp::{LspError, jsonrpc::error_codes};

use common::{MockResult, MockServer, default_server_capabilities};

/// The handshake completes: client gets capabilities, mock
/// observed `initialized` notification.
#[tokio::test]
async fn handshake_completes_and_initialized_notification_sent() {
    let server = MockServer::start().await;

    // Capabilities surfaced through the handle.
    let caps = server.handle.capabilities();
    assert!(caps.is_utf8(), "mock advertised utf-8");
    assert!(caps.supports_hover());
    assert!(caps.supports_definition());

    // Mock observed `initialized` after the initialize round-trip.
    // Allow the actor a beat to flush the notification.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let notes = server.mock.notifications().await;
    assert!(
        notes.iter().any(|n| n.method == "initialized"),
        "expected `initialized` notification; got {:?}",
        notes.iter().map(|n| &n.method).collect::<Vec<_>>()
    );
}

/// Server returns an error response to initialize → spawn fails
/// with HandshakeFailed.
#[tokio::test]
async fn handshake_fails_when_server_rejects_initialize() {
    use lattice_lsp::{LspReader, LspWriter, Message, Response, ResponseError, ServerConfig};
    use tokio::io::duplex;

    let (a, b) = duplex(8 * 1024);
    let (a_read, a_write) = tokio::io::split(a);
    let (b_read, b_write) = tokio::io::split(b);

    // Spawn a tiny mock that responds to initialize with an error.
    tokio::spawn(async move {
        let mut r = LspReader::new(b_read);
        let mut w = LspWriter::new(b_write);
        if let Ok(Some(Message::Request(req))) = r.read_message().await {
            assert_eq!(req.method, "initialize");
            let resp = Response::err(
                req.id,
                ResponseError {
                    code: error_codes::INTERNAL_ERROR,
                    message: "mock: refusing to start".into(),
                    data: None,
                },
            );
            let _ = w.write_message(&Message::Response(resp)).await;
        }
    });

    let cfg = ServerConfig::new("mock", "mock-server", "rust");
    let result = lattice_lsp::spawn_with_io(
        cfg,
        std::env::current_dir().unwrap(),
        LspReader::new(a_read),
        LspWriter::new(a_write),
        None,
        None,
        lattice_lsp::LspLogger::with_defaults(),
        None,
        None,
    )
    .await;
    match result {
        Err(LspError::HandshakeFailed(msg)) => {
            assert!(msg.contains("rejected initialize"), "msg = {msg}");
        }
        other => panic!("expected HandshakeFailed, got {other:?}"),
    }
}

/// utf-16 fallback when the server doesn't advertise positionEncodings.
#[tokio::test]
async fn position_encoding_falls_back_to_utf16_when_server_silent() {
    let mut caps = default_server_capabilities();
    caps.position_encoding = None;
    let server = MockServer::start_with_capabilities(caps).await;
    assert!(!server.handle.capabilities().is_utf8());
}

/// Many concurrent requests; responses arrive in any order; each
/// caller resolves with its matching reply.
#[tokio::test]
async fn concurrent_requests_correlate_correctly() {
    let server = MockServer::start().await;
    server
        .mock
        .on("textDocument/hover", |_params| {
            MockResult::Ok(json!({"contents": "hover-result"}))
        })
        .await;
    server
        .mock
        .on("textDocument/definition", |_params| {
            MockResult::Ok(json!([{"uri": "file:///a", "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}}}]))
        })
        .await;

    let hover_params = HoverParams {
        text_document_position_params: lsp_types::TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: <lsp_types::Uri as std::str::FromStr>::from_str("file:///x.rs").unwrap(),
            },
            position: Position {
                line: 0,
                character: 0,
            },
        },
        work_done_progress_params: Default::default(),
    };

    let h1 = server
        .handle
        .request::<_, Value>("textDocument/hover", hover_params.clone());
    let h2 = server
        .handle
        .request::<_, Value>("textDocument/definition", hover_params.clone());
    let h3 = server
        .handle
        .request::<_, Value>("textDocument/hover", hover_params);

    let r1 = h1.await.unwrap();
    let r2 = h2.await.unwrap();
    let r3 = h3.await.unwrap();
    assert_eq!(r1["contents"], "hover-result");
    assert!(r2.is_array());
    assert_eq!(r3["contents"], "hover-result");
}

/// Server pushes a request the client doesn't implement → the
/// actor responds with METHOD_NOT_FOUND.
#[tokio::test]
async fn unknown_server_request_yields_method_not_found() {
    let server = MockServer::start().await;
    server
        .mock
        .push_request(7777, "client/totallyMadeUp", json!({}));
    // Wait for the actor to respond.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let resps = server.mock.responses().await;
    let our = resps.iter().find(|r| {
        matches!(&r.id, lattice_lsp::jsonrpc::RequestId::Number(7777))
    });
    let our = our.expect("actor should have responded to unknown request");
    let err = our.error.as_ref().expect("expected error response");
    assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
}

/// `client/registerCapability` is accepted -- many servers send
/// this at startup to advertise dynamic registrations.
#[tokio::test]
async fn register_capability_is_accepted() {
    let server = MockServer::start().await;
    server.mock.push_request(
        9001,
        "client/registerCapability",
        json!({
            "registrations": [{
                "id": "watch-files",
                "method": "workspace/didChangeWatchedFiles",
                "registerOptions": {}
            }]
        }),
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    let resps = server.mock.responses().await;
    let our = resps
        .iter()
        .find(|r| matches!(&r.id, lattice_lsp::jsonrpc::RequestId::Number(9001)))
        .expect("expected response to registerCapability");
    assert!(our.error.is_none(), "expected ok response, got {:?}", our.error);
}

/// `workspace/configuration` returns one entry per requested
/// item -- LSP requires the response to be an array of length
/// equal to the request.
#[tokio::test]
async fn workspace_configuration_returns_array_matching_items() {
    let server = MockServer::start().await;
    server.mock.push_request(
        4242,
        "workspace/configuration",
        json!({
            "items": [
                {"section": "rust-analyzer.checkOnSave"},
                {"section": "rust-analyzer.cargo.features"},
                {"section": "rust-analyzer.diagnostics.enable"}
            ]
        }),
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    let resps = server.mock.responses().await;
    let our = resps
        .iter()
        .find(|r| matches!(&r.id, lattice_lsp::jsonrpc::RequestId::Number(4242)))
        .expect("expected configuration response");
    let arr = our
        .result
        .as_ref()
        .and_then(|v| v.as_array())
        .expect("array result");
    assert_eq!(arr.len(), 3, "one entry per requested item");
}

/// Notifications fire-and-forget: `notify` returns Ok and the
/// mock sees them in order.
#[tokio::test]
async fn notifications_arrive_at_mock_in_order() {
    let server = MockServer::start().await;
    // Skip the `initialized` notification that handshake sent --
    // measure deltas after a known checkpoint.
    let baseline = server.mock.notifications().await.len();
    server
        .handle
        .notify("textDocument/didOpen", json!({"x": 1}))
        .unwrap();
    server
        .handle
        .notify("textDocument/didChange", json!({"x": 2}))
        .unwrap();
    server
        .handle
        .notify("textDocument/didClose", json!({"x": 3}))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let after = server.mock.notifications().await;
    let methods: Vec<&str> = after.iter().skip(baseline).map(|n| n.method.as_str()).collect();
    assert_eq!(
        methods,
        ["textDocument/didOpen", "textDocument/didChange", "textDocument/didClose"]
    );
}

/// Shutdown sends `shutdown` request + `exit` notification, in
/// that order. After resolution, the handle is unusable.
#[tokio::test]
async fn shutdown_sends_shutdown_then_exit_and_handle_is_dead() {
    let server = MockServer::start().await;
    let r = server.handle.shutdown().await;
    assert!(r.is_ok(), "shutdown returned {r:?}");

    // Mock saw the `shutdown` request + `exit` notification.
    let reqs = server.mock.requests().await;
    assert!(
        reqs.iter().any(|r| r.method == "shutdown"),
        "expected shutdown request"
    );
    let notes = server.mock.notifications().await;
    assert!(
        notes.iter().any(|n| n.method == "exit"),
        "expected exit notification"
    );

    // Subsequent requests fail with ActorGone.
    let dead = server
        .handle
        .request::<_, Value>("textDocument/hover", json!({}))
        .await;
    assert!(matches!(dead, Err(LspError::ActorGone)));
}

/// The actor cancels in-flight requests when the matching
/// JSON-RPC id is cancelled. Mock sees the `$/cancelRequest`
/// notification.
#[tokio::test]
async fn cancel_resolves_pending_with_cancelled_and_emits_notification() {
    let server = MockServer::start().await;
    // Slow handler for hover so we can race a cancel against it.
    server
        .mock
        .on("textDocument/hover", |_| {
            // Mock side delay: simulate a server thinking.
            // (The mock task is single-threaded via select! so
            // this delay isn't great; we register the handler
            // and instead never respond by leaving the in_rx
            // empty; concretely cancel by JSON-RPC id 1 since
            // that's the one we send.)
            MockResult::Ok(json!(null))
        })
        .await;

    // The first request we send after handshake gets id 2 (id 1
    // is initialize). Issue it and immediately cancel.
    let pending =
        server
            .handle
            .request::<_, Value>("textDocument/hover", json!({"textDocument": {"uri": "file:///x"}, "position": {"line":0,"character":0}}));
    server.handle.cancel(2).expect("cancel send");
    let r = pending.await;
    assert!(matches!(r, Err(LspError::Cancelled)), "got {r:?}");

    // Mock saw a cancelRequest notification.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let notes = server.mock.notifications().await;
    assert!(
        notes.iter().any(|n| n.method == "$/cancelRequest"),
        "expected $/cancelRequest"
    );
}

/// If the mock server closes its pipe mid-request, all pending
/// requests resolve with ActorGone.
#[tokio::test]
async fn server_pipe_close_resolves_pending_with_actor_gone() {
    use lattice_lsp::{LspReader, LspWriter, Message, ServerConfig};
    use tokio::io::duplex;

    let (a, b) = duplex(8 * 1024);
    let (a_read, a_write) = tokio::io::split(a);
    let (b_read, b_write) = tokio::io::split(b);

    // Mock task: respond to initialize, then drop the writer.
    tokio::spawn(async move {
        let mut r = LspReader::new(b_read);
        let mut w = LspWriter::new(b_write);
        if let Ok(Some(Message::Request(req))) = r.read_message().await {
            let caps = ServerCapabilities::default();
            let result = lsp_types::InitializeResult {
                capabilities: caps,
                server_info: None,
            };
            let resp = lattice_lsp::Response::ok(
                req.id,
                serde_json::to_value(result).unwrap(),
            );
            let _ = w.write_message(&Message::Response(resp)).await;
        }
        // Drop both halves -> actor sees pipe close.
    });

    let cfg = ServerConfig::new("mock-crash", "x", "rust");
    let handle = lattice_lsp::spawn_with_io(
        cfg,
        std::env::current_dir().unwrap(),
        LspReader::new(a_read),
        LspWriter::new(a_write),
        None,
        None,
        lattice_lsp::LspLogger::with_defaults(),
        None,
        None,
    )
    .await
    .unwrap();

    // Wait briefly for the read_loop to observe the EOF.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let r = handle.request::<_, Value>("textDocument/hover", json!({})).await;
    assert!(matches!(r, Err(LspError::ActorGone) | Err(LspError::ResponseDropped)));
}

/// Server returns an error response → caller sees LspError::Server
/// with the structured payload (code + message).
#[tokio::test]
async fn server_error_response_surfaces_with_code_and_message() {
    let server = MockServer::start().await;
    server
        .mock
        .on("textDocument/hover", |_| {
            MockResult::Err(lattice_lsp::ResponseError {
                code: error_codes::REQUEST_FAILED,
                message: "mock: hover failed".into(),
                data: None,
            })
        })
        .await;
    let r = server
        .handle
        .request::<_, Value>("textDocument/hover", json!({}))
        .await;
    match r {
        Err(LspError::Server(e)) => {
            assert_eq!(e.code, error_codes::REQUEST_FAILED);
            assert_eq!(e.message, "mock: hover failed");
        }
        other => panic!("expected LspError::Server, got {other:?}"),
    }
}

/// Response that doesn't match the requested type -> LspError::ResponseDecode.
#[tokio::test]
async fn response_with_wrong_shape_is_decode_error() {
    let server = MockServer::start().await;
    // Handler returns a string where the caller expects a struct.
    server
        .mock
        .on("textDocument/hover", |_| MockResult::Ok(json!("not-a-hover-shape")))
        .await;

    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct Hover {
        contents: Value,
    }

    let r = server.handle.request::<_, Hover>("textDocument/hover", json!({})).await;
    assert!(matches!(r, Err(LspError::ResponseDecode(_))));
}
