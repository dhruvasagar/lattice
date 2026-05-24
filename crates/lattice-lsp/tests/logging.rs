#![allow(clippy::unwrap_used, clippy::panic)]
//! Integration tests for the LSP logging subsystem
//! (Phase 4.1.f).
//!
//! Coverage:
//!
//! - Handshake emits a Client/Info record to the global +
//!   per-server rings.
//! - `window/logMessage` lands in the per-server ring at the
//!   level the server requested (Error / Warn / Info / Log).
//! - `window/showMessage` lands in the per-server ring as
//!   `LspShowMessage`.
//! - `publishDiagnostics` emits a Debug-level summary alongside
//!   the bus broadcast.
//! - Trace toggle: off = no `LogSource::Trace` records; on =
//!   inbound + outbound traced for every message.
//! - Server-initiated unhandled-method requests log a record.
//! - The handle's `.logger()` accessor is the same ring as the
//!   one we passed in.
//!
//! All tests run against the in-process MockServer; no real
//! LSP server is involved.

mod common;

use std::time::Duration;

use serde_json::{Value, json};

use lattice_lsp::{LogLevel, LogSource, LspLogger};

use common::{MockResult, MockServer};

/// Helper: build a fresh logger + MockServer pair so the test
/// can inspect per-server log rings.
async fn mock_with_logger() -> (MockServer, LspLogger) {
    let logger = LspLogger::with_defaults();
    // Override min level so Debug records (publishDiagnostics
    // summary, etc.) show up in tests.
    logger.set_default_level(LogLevel::Debug);
    let server = MockServer::start_with_logger(logger.clone()).await;
    (server, logger)
}

#[tokio::test]
async fn handshake_emits_global_and_per_server_records() {
    let (_server, logger) = mock_with_logger().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let g = logger.snapshot_global();
    assert!(
        g.iter().any(|r| r.message.contains("spawning LSP actor")),
        "expected global spawn record; got {:?}",
        g.iter().map(|r| &r.message).collect::<Vec<_>>()
    );
    let instance = _server.handle.instance();
    let s = logger.snapshot_instance(&instance);
    assert!(
        s.iter().any(|r| r.message.contains("handshake complete")),
        "expected per-server handshake-complete record"
    );
}

#[tokio::test]
async fn window_log_message_routes_to_per_server_ring_at_requested_level() {
    let (server, logger) = mock_with_logger().await;
    let instance = server.handle.instance();
    server.mock.push_notification(
        "window/logMessage",
        // MessageType::ERROR == 1 in the LSP wire encoding.
        json!({
            "type": 1,
            "message": "rust-analyzer panicked"
        }),
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    let recs = logger.snapshot_instance(&instance);
    let log_msg = recs
        .iter()
        .find(|r| r.source == LogSource::LspMessage)
        .expect("expected LspMessage record");
    assert_eq!(log_msg.level, LogLevel::Error);
    assert_eq!(log_msg.message, "rust-analyzer panicked");
}

#[tokio::test]
async fn window_show_message_uses_distinct_log_source() {
    let (server, logger) = mock_with_logger().await;
    let instance = server.handle.instance();
    server.mock.push_notification(
        "window/showMessage",
        // MessageType::WARNING == 2.
        json!({
            "type": 2,
            "message": "config out of date"
        }),
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    let recs = logger.snapshot_instance(&instance);
    let show = recs
        .iter()
        .find(|r| r.source == LogSource::LspShowMessage)
        .expect("expected LspShowMessage record");
    assert_eq!(show.level, LogLevel::Warn);
    assert_eq!(show.message, "config out of date");
}

#[tokio::test]
async fn publish_diagnostics_emits_debug_summary_alongside_bus() {
    let (server, logger) = mock_with_logger().await;
    let instance = server.handle.instance();
    server.mock.push_notification(
        "textDocument/publishDiagnostics",
        json!({
            "uri": "file:///x.rs",
            "version": 3,
            "diagnostics": [{
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
                "severity": 1,
                "message": "boom"
            }]
        }),
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    let recs = logger.snapshot_instance(&instance);
    assert!(
        recs.iter().any(|r| r.message.contains("publishDiagnostics") && r.message.contains("file:///x.rs")),
        "expected publishDiagnostics summary; got {:?}",
        recs.iter().map(|r| &r.message).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn trace_off_means_no_trace_records() {
    let (server, logger) = mock_with_logger().await;
    let instance = server.handle.instance();
    server
        .mock
        .on("textDocument/hover", |_p| {
            MockResult::Ok(json!({"contents": "hi"}))
        })
        .await;
    let _r = server
        .handle
        .request::<_, Value>("textDocument/hover", json!({"x": 1}))
        .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let recs = logger.snapshot_instance(&instance);
    let trace_count = recs.iter().filter(|r| r.source == LogSource::Trace).count();
    assert_eq!(trace_count, 0, "trace off, no trace records");
}

#[tokio::test]
async fn trace_on_records_inbound_and_outbound_messages() {
    let (server, logger) = mock_with_logger().await;
    let instance = server.handle.instance();
    logger.enable_trace(instance.clone());
    server
        .mock
        .on("textDocument/hover", |_p| {
            MockResult::Ok(json!({"contents": "hi"}))
        })
        .await;
    let _r = server
        .handle
        .request::<_, Value>("textDocument/hover", json!({"x": 1}))
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let recs = logger.snapshot_instance(&instance);
    let traces: Vec<_> = recs
        .iter()
        .filter(|r| r.source == LogSource::Trace)
        .collect();
    assert!(
        traces.iter().any(|r| r.message.starts_with("→")),
        "expected outbound trace marker (→); got {:?}",
        traces.iter().map(|r| &r.message).collect::<Vec<_>>()
    );
    assert!(
        traces.iter().any(|r| r.message.starts_with("←")),
        "expected inbound trace marker (←)"
    );
}

#[tokio::test]
async fn server_initiated_unhandled_request_routes_through_logger() {
    let (server, logger) = mock_with_logger().await;
    let instance = server.handle.instance();
    server
        .mock
        .push_request(9999, "client/totallyMadeUp", json!({}));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let recs = logger.snapshot_instance(&instance);
    assert!(
        recs.iter().any(|r| r.message.contains("totallyMadeUp")),
        "expected unhandled-method log record; got {:?}",
        recs.iter().map(|r| &r.message).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn shared_logger_observes_records_from_actor_handle() {
    let (server, logger) = mock_with_logger().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let from_handle = server.handle.logger().snapshot_global();
    let from_test = logger.snapshot_global();
    assert_eq!(from_handle.len(), from_test.len());
}

#[tokio::test]
async fn known_servers_lists_the_attached_mock() {
    let (_server, logger) = mock_with_logger().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let servers: Vec<String> = logger
        .known_instances()
        .into_iter()
        .map(|k| k.server_id.to_string())
        .collect();
    assert!(servers.contains(&"mock".to_string()));
}
