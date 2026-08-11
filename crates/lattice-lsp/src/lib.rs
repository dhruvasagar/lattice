//! `lattice-lsp` -- the LSP client (DESIGN.md §5.4, Phase 4).
//!
//! ## Why hand-rolled
//!
//! `tower-lsp` is server-side. `async-lsp` brings tower middleware
//! that doesn't fit our actor model (every server is one tokio task
//! with a mailbox + oneshot replies, identical to
//! `lattice-runtime::RopeDocumentHandle`). The wire protocol is a few
//! hundred lines of framing + JSON-RPC; reusing our existing
//! cancellation primitives (`CancellationToken`) is cleaner than
//! adapting middleware.
//!
//! ## Layering
//!
//! - [`framing`] -- LSP's `Content-Length` header parser. Pure;
//!   stream-agnostic; tested against partial reads, malformed
//!   headers, and oversized bodies.
//! - [`jsonrpc`] -- typed JSON-RPC 2.0 messages: requests with id
//!   correlation, responses, notifications, and `ResponseError`.
//!   No transport assumptions.
//! - [`codec`] -- glues framing + jsonrpc onto
//!   `tokio::io::AsyncBufRead` / `AsyncWrite`. Yields one
//!   [`jsonrpc::Message`] per `read_message` call; encodes one per
//!   `write_message`.
//! - [`transport`] -- spawns a child process and exposes its
//!   stdio as a [`codec`] reader / writer pair. Per
//!   DESIGN.md §5.4 each (workspace, server-id) gets one transport.
//! - **Future modules** (folded in across 4.1.b–4.4):
//!   - `actor` -- the per-server tokio task (mailbox + dispatch).
//!   - `client` -- the editor-facing `LspHandle` analog of
//!     `RopeDocumentHandle`.
//!   - `sync` -- `AppliedEdit` ↔ `TextDocumentContentChangeEvent`.
//!   - `position` -- utf-8 ↔ utf-16 column conversion.
//!   - `capabilities` -- client capability advertisement +
//!     server capability gating.
//!
//! ## Performance discipline
//!
//! All public methods that talk to a server return `Pending<T>`
//! (matching §5.2.1's dispatch envelope); nothing blocks the UI.
//! LSP requests are §5.2.5 *Background*-class -- they have no
//! sync-prelude budget and may be cancelled / superseded freely.
//! Per-call performance characteristics live in
//! `benches/lsp.rs` and are mirrored in `docs/dev/operations/benchmarks.md`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::panic))]

pub mod actor;
pub mod apply_edit;
// M-async.5: `attach_driver` retired. `LspMode::on_activate`
// now drives the supervisor's `open_buffer` directly; the
// bus's `DocumentOpened` event still fires for other
// subscribers but LSP no longer keys off it.
pub mod buffer_names;
pub mod cache;
pub mod capabilities;
pub mod codec;
pub mod completion;
pub mod config;
pub mod configuration;
pub mod diagnostics;
pub mod diagnostics_layer;
pub mod dynamic_registration;
pub mod error;
// EP.3: the language server as a producer of the core error list.
pub mod error_list_feed;
pub mod events;
pub mod fan_in;
pub mod features;
pub mod file_watcher;
pub mod folding_sync;
pub mod framing;
pub mod help_views;
pub mod install;
/// JSON-RPC 2.0 message types now live in `lattice-protocol` (IDE-protocol
/// Risk 3 lift) so `lattice-claude-code` can share them. Re-exported here
/// as `lattice_lsp::jsonrpc` so existing `crate::jsonrpc::*` paths,
/// downstream callers, tests, and benches keep resolving unchanged.
pub use lattice_protocol::jsonrpc;
pub mod logging;
pub mod modeline;
pub mod modes;
pub mod pending;
pub mod position;
pub mod providers;
pub mod show_document;
pub mod show_message_request;
pub mod supervisor;
pub mod sync;
pub mod transport;

pub use actor::{ServerHandle, spawn, spawn_with_io};
pub use apply_edit::{ApplyEditBus, ApplyEditOutcome, InboundApplyEdit};
pub use buffer_names::{
    LSP_SUBSYSTEM_LOG_NAME, lsp_server_log_name, lsp_server_trace_log_name,
    parse_lsp_server_log_name, parse_lsp_trace_log_name,
};
pub use capabilities::{Capabilities, FileOpKind, client_capabilities};
pub use codec::{LspReader, LspWriter};
pub use config::{ServerConfig, builtin_servers, resolve_workspace_root};
pub use configuration::{ConfigurationBus, InboundConfigurationRequest};
pub use diagnostics::{DIAGNOSTICS_CHANNEL_CAPACITY, DiagnosticEvent, DiagnosticsBus};
pub use diagnostics_layer::{
    DiagnosticsLayer, InlineDiagnosticSummary, SeverityCounts, pump_diagnostics,
};
pub use dynamic_registration::{DynamicRegistration, DynamicRegistry};
pub use error::{LspError, LspResult};
pub use events::{
    LspActorExitReason, LspActorExited, LspBufferAttached, LspBufferDetached, LspCodeLensRefresh,
    LspDiagnosticRefresh, LspDocumentChanged, LspInlayHintRefresh, LspLogPushed, LspProgressKind,
    LspProgressUpdate, LspSemanticTokensRefresh, LspServerHealth, LspServerStatusChanged,
};
pub use file_watcher::{WatcherSubscriptions, compile_with_workspace_root};
pub use framing::{FrameError, FrameHeader};
pub use install::install;
pub use lattice_protocol::jsonrpc::{
    Message, Notification, Request, RequestId, Response, ResponseError,
};
pub use logging::{
    InstanceKey, LogLevel, LogRecord, LogRing, LogSource, LspLogger, format_log_event_line,
    level_tag as log_level_tag,
};
pub use pending::{InvocationId, Pending};
pub use show_document::{InboundShowDocument, ShowDocumentBus, ShowDocumentOutcome};
pub use show_message_request::{
    InboundShowMessageRequest, ShowMessageRequestBus, ShowMessageRequestOutcome,
};
pub use supervisor::{
    ActorKey, LspSupervisor, LspSupervisorHandle, RestartReport, SupervisorSnapshot,
};
pub use sync::{DocSync, uri_from_str};

// Re-export commonly-used LSP types so consumers don't need
// a direct `lsp-types` dep just to spell them out in
// signatures. lsp-types changes shape across major versions;
// the re-exports give us a stable seam if we need to swap.
// 5.8.J / refactor #70: lattice-lsp is the canonical owner of the
// `lsp_types` substrate; downstream crates reach types through this
// crate rather than depending on `lsp-types` directly. The module
// re-export gives them the full namespace (~108 distinct types are
// used workspace-wide; selective re-exports were unwieldy).
pub use lsp_types;

// Convenience re-exports for the most-used items so callers can
// write `lattice_lsp::Diagnostic` instead of
// `lattice_lsp::lsp_types::Diagnostic` for the common cases.
pub use lsp_types::{
    Diagnostic, DiagnosticSeverity, InlayHint, InlayHintLabel, InlayHintLabelPart,
    Position as LspPosition, Range as LspRange, Uri,
};
pub use transport::{ChildTransport, TransportError};

/// Flatten an LSP [`InlayHintLabel`] into a single plain `String`.
/// `String` variants return as-is; `LabelParts(Vec<LabelPart>)`
/// concatenates each part's `value`. Tooltip / command / location
/// fields on label parts are ignored — they're resolve / hover
/// affordances, not a paint concern.
///
/// Phase 5.8.N: hoisted out of both renderer peers. TUI had it as
/// `lattice-ui-tui::render::inlay_hint_label_text`; GPUI had the
/// equivalent inlined in `lattice-ui-gpui::window`. Renderer-
/// neutral since it operates purely on an LSP type; lives here
/// because lattice-lsp is the canonical owner of `lsp_types`.
pub fn inlay_hint_label_text(label: &InlayHintLabel) -> String {
    match label {
        InlayHintLabel::String(s) => s.clone(),
        InlayHintLabel::LabelParts(parts) => parts.iter().map(|p| p.value.clone()).collect(),
    }
}

#[cfg(test)]
mod inlay_hint_label_tests {
    use super::*;

    #[test]
    fn string_variant_returns_as_is() {
        let label = InlayHintLabel::String(": i32".into());
        assert_eq!(inlay_hint_label_text(&label), ": i32");
    }

    #[test]
    fn parts_variant_concatenates_values() {
        let parts = InlayHintLabel::LabelParts(vec![
            InlayHintLabelPart {
                value: "size".into(),
                tooltip: None,
                location: None,
                command: None,
            },
            InlayHintLabelPart {
                value: ": ".into(),
                tooltip: None,
                location: None,
                command: None,
            },
            InlayHintLabelPart {
                value: "usize".into(),
                tooltip: None,
                location: None,
                command: None,
            },
        ]);
        assert_eq!(inlay_hint_label_text(&parts), "size: usize");
    }

    #[test]
    fn empty_parts_returns_empty_string() {
        let parts = InlayHintLabel::LabelParts(vec![]);
        assert_eq!(inlay_hint_label_text(&parts), "");
    }
}
