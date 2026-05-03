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
use serde_json::json;

use lattice_lsp::framing::parse_header_block;
use lattice_lsp::jsonrpc::{Message, Notification, Request, RequestId};

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

criterion_group!(
    benches,
    framing_parse_header,
    encode_did_change,
    decode_publish_diagnostics,
    decode_small_response,
    encode_decode_request_round_trip,
);
criterion_main!(benches);
