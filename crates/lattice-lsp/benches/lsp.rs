#![allow(clippy::unwrap_used, clippy::panic)]
//! Criterion benchmarks for the LSP wire layer.
//!
//! Per DESIGN.md §5.2.5 LSP requests are *Background*-class
//! (no sync-prelude budget), but the *plumbing* underneath them
//! sits on the keystroke path:
//!
//! - `framing::parse_header_block` runs once per inbound message;
//!   at high message rates (semantic-tokens delta during a fast
//!   scroll) this is hot.
//! - `Message::from_json` decodes every inbound payload --
//!   diagnostics fan-out is the canonical worst case.
//! - `Message::to_json` encodes every outbound -- `didChange`
//!   per keystroke is the canonical worst case.
//!
//! Targets are below the snapshot/publish budgets in §8.2; the
//! goal is "LSP plumbing never shows up in a flame graph next to
//! the editor's own work."

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use lsp_types::PositionEncodingKind;
use serde_json::json;

use std::path::PathBuf;
use std::sync::Arc;

use lattice_lsp::capabilities::{Capabilities, client_capabilities};
use lattice_lsp::framing::parse_header_block;
use lattice_lsp::jsonrpc::{Message, Notification, Request, RequestId};
use lattice_lsp::position::{
    byte_to_lsp_character, utf8_byte_to_utf16_column, utf16_column_to_utf8_byte,
};
use lattice_lsp::sync::DocSync;
use lattice_lsp::{LogLevel, LogSource, LspLogger};
use lattice_protocol::edit::{Edit, EditKind};
use lattice_protocol::event::{AppliedEdit as ProtocolAppliedEdit, Event};
use lattice_protocol::ids::DocumentId;
use lattice_protocol::position::{Position, Range};
use lattice_runtime::{EventBus, EventFilter, SubscriptionTarget};
use lattice_protocol::EventKind;

/// Header parse: ASCII-only, ≤200 byte block. Should be deep
/// in nanoseconds.
fn framing_parse_header(c: &mut Criterion) {
    let block = b"Content-Length: 1234\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8";
    c.bench_function("lsp::framing::parse_header_block", |b| {
        b.iter(|| {
            let h = parse_header_block(black_box(block.as_slice())).unwrap();
            black_box(h.content_length);
        });
    });
}

/// Encode a representative `textDocument/didChange` notification
/// (the highest-frequency outbound message at a steady-state
/// debounced rate of one per ~50ms). `params` is one
/// TextDocumentContentChangeEvent with a small replacement.
fn encode_did_change(c: &mut Criterion) {
    let n = Message::Notification(Notification::new(
        "textDocument/didChange",
        Some(json!({
            "textDocument": {"uri": "file:///workspace/lib.rs", "version": 17},
            "contentChanges": [{
                "range": {
                    "start": {"line": 12, "character": 4},
                    "end":   {"line": 12, "character": 4}
                },
                "rangeLength": 0,
                "text": "x"
            }]
        })),
    ));
    c.bench_function("lsp::encode::did_change", |b| {
        b.iter(|| {
            let bytes = black_box(&n).to_json().unwrap();
            black_box(bytes);
        });
    });
}

/// Decode a representative `publishDiagnostics` body. Diagnostics
/// arrive on every save / on idle from servers that compile in
/// the background; one diagnostic with a code + range + message.
fn decode_publish_diagnostics(c: &mut Criterion) {
    let body = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": {
            "uri": "file:///workspace/lib.rs",
            "version": 17,
            "diagnostics": [{
                "range": {
                    "start": {"line": 12, "character": 4},
                    "end":   {"line": 12, "character": 9}
                },
                "severity": 1,
                "code": "E0308",
                "source": "rustc",
                "message": "expected `String`, found `&str`"
            }]
        }
    }))
    .unwrap();
    c.bench_function("lsp::decode::publish_diagnostics", |b| {
        b.iter(|| {
            let m = Message::from_json(black_box(&body)).unwrap();
            black_box(m);
        });
    });
}

/// Decode a small request -- the per-id correlation hot path
/// (initialize response, hover response, etc).
fn decode_small_response(c: &mut Criterion) {
    let body = br#"{"jsonrpc":"2.0","id":1,"result":{"capabilities":{"hoverProvider":true}}}"#;
    c.bench_function("lsp::decode::small_response", |b| {
        b.iter(|| {
            let m = Message::from_json(black_box(body)).unwrap();
            black_box(m);
        });
    });
}

/// Encode + decode round-trip for one `Request`. Models the
/// outgoing-side cost of every LSP request the actor issues.
fn encode_decode_request_round_trip(c: &mut Criterion) {
    let req = Message::Request(Request::new(
        RequestId::from_u64(1),
        "textDocument/hover",
        Some(json!({
            "textDocument": {"uri": "file:///workspace/lib.rs"},
            "position": {"line": 0, "character": 0}
        })),
    ));
    c.bench_function("lsp::encode_decode::hover_request", |b| {
        b.iter(|| {
            let bytes = black_box(&req).to_json().unwrap();
            let parsed = Message::from_json(&bytes).unwrap();
            black_box(parsed);
        });
    });
}

/// Position-encoding conversion: ASCII line. utf-8 mode is a
/// branch-and-return; this measures the floor.
fn position_utf8_passthrough(c: &mut Criterion) {
    let line = "fn handler(req: &Request<Output, Error>) -> Result<()>";
    c.bench_function("lsp::position::utf8_passthrough", |b| {
        b.iter(|| {
            let col = byte_to_lsp_character(
                black_box(line),
                black_box(50),
                &PositionEncodingKind::UTF8,
            );
            black_box(col);
        });
    });
}

/// Worst case: 64-character line of CJK glyphs (3 utf-8 bytes / 1
/// utf-16 unit each). Walks the whole prefix counting utf-16
/// units. Backs the §8.2 commitment that utf-16 column conversion
/// stays sub-microsecond on any realistic line.
fn position_utf16_cjk_line(c: &mut Criterion) {
    let line: String = "中文".repeat(32);
    let byte = (line.len() / 2) as u32;
    c.bench_function("lsp::position::utf16_cjk_line", |b| {
        b.iter(|| {
            let col = utf8_byte_to_utf16_column(black_box(&line), black_box(byte));
            black_box(col);
        });
    });
}

/// Reverse direction: utf-16 character → utf-8 byte. Used on
/// every range coming FROM the server (definitions, diagnostics).
fn position_utf16_to_byte_cjk(c: &mut Criterion) {
    let line: String = "中文".repeat(32);
    c.bench_function("lsp::position::utf16_to_byte_cjk", |b| {
        b.iter(|| {
            let byte = utf16_column_to_utf8_byte(black_box(&line), black_box(32));
            black_box(byte);
        });
    });
}

/// Logger throughput in the production path: one Info record
/// per call (Trace toggle off; level passes default Info
/// filter). Models per-event cost at the actor boundary.
fn logging_log_info(c: &mut Criterion) {
    let logger = LspLogger::with_defaults();
    let server_id: Arc<str> = Arc::from("rust");
    c.bench_function("lsp::logging::log_info", |b| {
        b.iter(|| {
            logger.log(
                Some(black_box(&server_id)),
                LogLevel::Info,
                LogSource::Client,
                black_box("server attached"),
            );
        });
    });
}

/// Same shape but Trace-level with the toggle OFF -- the
/// short-circuit path. Should be a HashSet lookup + return.
fn logging_log_trace_off(c: &mut Criterion) {
    let logger = LspLogger::with_defaults();
    let server_id: Arc<str> = Arc::from("rust");
    c.bench_function("lsp::logging::log_trace_off", |b| {
        b.iter(|| {
            logger.log(
                Some(black_box(&server_id)),
                LogLevel::Trace,
                LogSource::Trace,
                black_box("trace text"),
            );
        });
    });
}

/// Same shape with the toggle ON -- includes the ring push.
fn logging_log_trace_on(c: &mut Criterion) {
    let logger = LspLogger::new(LogLevel::Trace, 100_000);
    let server_id: Arc<str> = Arc::from("rust");
    logger.enable_trace(Arc::clone(&server_id));
    c.bench_function("lsp::logging::log_trace_on", |b| {
        b.iter(|| {
            logger.log(
                Some(black_box(&server_id)),
                LogLevel::Trace,
                LogSource::Trace,
                black_box("trace text"),
            );
        });
    });
}

// ---------------------------------------------------------------
// Edit-path benches (added with the per-actor fan-in refactor).
//
// These three benches monitor the cost the new architecture
// (DESIGN.md §5.2.5 + docs/dev/architecture/lsp-architecture.md §11) puts on the
// keystroke path:
//
//   (a) `lsp_edit_publish_*`       -- UI-thread overhead per edit
//   (b) `lsp_edit_propagation_*`   -- end-to-end publish -> actor
//   (c) `lsp_didchange_flush_*`    -- queued change -> wire payload
//
// (a) is the only one that runs on the UI thread; (b) measures
// the latency budget the user sees from "key down" to "server
// has it" before any debounce; (c) measures the per-flush cost
// the actor pays asynchronously.
// ---------------------------------------------------------------

fn make_document_changed_event(n_edits: usize) -> Event {
    let edits: Vec<ProtocolAppliedEdit> = (0..n_edits)
        .map(|i| ProtocolAppliedEdit {
            original_range: Range::empty(Position::new(0, i as u32)),
            inserted_range: Range::new(
                Position::new(0, i as u32),
                Position::new(0, i as u32 + 1),
            ),
            replaced_text: String::new(),
            inserted_text: "x".to_string(),
        })
        .collect();
    Event::DocumentChanged {
        id: DocumentId::new(1),
        path: Some(PathBuf::from("/workspace/lib.rs")),
        version: 17,
        edits,
    }
}

/// (a) UI-thread overhead per edit. Measures `EventBus::publish`
/// of one `Event::DocumentChanged` carrying a single applied
/// edit, against a bus with three `DocumentChanged` subscribers
/// (typical: rust-analyzer + clippy bridge + a future plugin).
/// This is the *only* part of the LSP edit path that runs on
/// the keystroke thread; the §8 budget says it must stay tens
/// of microseconds even when many actors are attached.
fn lsp_edit_publish_three_subs(c: &mut Criterion) {
    let bus = EventBus::new();
    // Drain receivers in background-equivalent storage; we just
    // need real `mpsc::UnboundedSender`s on the bus.
    let _keepalive: Vec<_> = (0..3)
        .map(|_| {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            bus.subscribe(
                EventFilter::kinds(vec![EventKind::DocumentChanged]),
                SubscriptionTarget::Channel(tx),
            );
            rx
        })
        .collect();
    let event = make_document_changed_event(1);
    c.bench_function("lsp_edit_publish_three_subs", |b| {
        b.iter(|| {
            bus.publish(black_box(event.clone()));
        });
    });
}

/// (b) End-to-end edit-propagation latency. Publishes one
/// `DocumentChanged` event and awaits its arrival on a single
/// subscriber's mpsc -- mirrors the bus -> fan-in hop. Excludes
/// the actor's own `record_edit` (benched in (c)) so this
/// number is the bus + mpsc cost in isolation.
fn lsp_edit_propagation_publish_to_recv(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let bus = EventBus::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    bus.subscribe(
        EventFilter::kinds(vec![EventKind::DocumentChanged]),
        SubscriptionTarget::Channel(tx),
    );
    let event = make_document_changed_event(1);
    c.bench_function("lsp_edit_propagation_publish_to_recv", |b| {
        b.iter(|| {
            bus.publish(black_box(event.clone()));
            runtime.block_on(async {
                rx.recv().await.unwrap();
            });
        });
    });
}

/// (c) `didChange` flush throughput. Models the actor's
/// debounce-arm work: queue 16 edits via DocSync, then
/// `take_flush_payload` and serialise -- the back half of one
/// keystroke burst. UTF-8 negotiated (the common case for
/// rust-analyzer); per-actor, single-writer.
fn lsp_didchange_flush_16_edits(c: &mut Criterion) {
    let mut server_caps = lsp_types::ServerCapabilities::default();
    server_caps.position_encoding = Some(PositionEncodingKind::UTF8);
    server_caps.text_document_sync = Some(
        lsp_types::TextDocumentSyncCapability::Kind(
            lsp_types::TextDocumentSyncKind::INCREMENTAL,
        ),
    );
    let caps = Capabilities::from_initialize(client_capabilities(), server_caps);
    let uri = lsp_types::Uri::from_str("file:///workspace/lib.rs").unwrap();
    c.bench_function("lsp_didchange_flush_16_edits", |b| {
        b.iter(|| {
            let mut sync = DocSync::new();
            // Seed with a representative line of source text;
            // long enough that line splices are non-trivial.
            let text = "fn handler(req: &Request<Output, Error>) -> Result<()>\n"
                .repeat(8);
            let _open = sync.open(uri.clone(), "rust", text);
            for i in 0..16 {
                let edit = Edit {
                    range: Range::empty(Position::new(0, i)),
                    kind: EditKind::Replace { text: "x".into() },
                };
                sync.record_edit(&caps, &uri, &edit).unwrap();
            }
            let payload = sync.take_flush_payload(&caps, &uri).unwrap();
            let n = Notification::new(
                "textDocument/didChange",
                Some(serde_json::to_value(payload).unwrap()),
            );
            let bytes = Message::Notification(n).to_json().unwrap();
            black_box(bytes);
        });
    });
}

use std::str::FromStr;

/// Diagnostic-layer render-path read after the audit's C3 fix.
/// Renderer calls `line_severity` once per visible line per
/// frame (~50 lines × 60 Hz ≈ 3000/s on the render thread); the
/// pre-fix path locked an inner Mutex + cloned the entire
/// diagnostics list per call. The fix moves state into an
/// `ArcSwap<DiagnosticsSnapshot>`; this bench measures the new
/// wait-free read against a layer holding 32 diagnostics on
/// one URI.
fn diagnostics_layer_line_severity_wait_free(c: &mut Criterion) {
    let logger = LspLogger::with_defaults();
    let layer = lattice_lsp::DiagnosticsLayer::new(logger);
    let uri = lsp_types::Uri::from_str("file:///workspace/lib.rs").unwrap();
    let server_id: Arc<str> = Arc::from("rust");
    let diagnostics: Vec<lsp_types::Diagnostic> = (0..32u32)
        .map(|i| lsp_types::Diagnostic {
            range: lsp_types::Range {
                start: lsp_types::Position {
                    line: i,
                    character: 0,
                },
                end: lsp_types::Position {
                    line: i,
                    character: 4,
                },
            },
            severity: Some(lsp_types::DiagnosticSeverity::WARNING),
            ..Default::default()
        })
        .collect();
    layer.apply(lattice_lsp::diagnostics::DiagnosticEvent {
        uri: uri.clone(),
        server_id,
        version: Some(1),
        diagnostics: Arc::from(diagnostics),
    });
    c.bench_function("lsp_diagnostics_line_severity_wait_free", |b| {
        b.iter(|| {
            let s = layer.line_severity(black_box(&uri), black_box(15));
            black_box(s);
        });
    });
}

criterion_group!(
    benches,
    framing_parse_header,
    encode_did_change,
    decode_publish_diagnostics,
    decode_small_response,
    encode_decode_request_round_trip,
    position_utf8_passthrough,
    position_utf16_cjk_line,
    position_utf16_to_byte_cjk,
    logging_log_info,
    logging_log_trace_off,
    logging_log_trace_on,
    lsp_edit_publish_three_subs,
    lsp_edit_propagation_publish_to_recv,
    lsp_didchange_flush_16_edits,
    diagnostics_layer_line_severity_wait_free,
);
criterion_main!(benches);
