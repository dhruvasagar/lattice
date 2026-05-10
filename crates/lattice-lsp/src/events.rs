// `linkme`'s distributed-slice expansion uses `#[link_section]`
// declarations, which the workspace's `unsafe_code = "deny"`
// lint flags. Same shape `lattice-config::core_options` and
// `lattice-protocol::event_registry` use.
#![allow(unsafe_code)]

//! LSP-owned editor-bus events (M.5.3.b).
//!
//! Three concrete event types replacing the LSP variants that
//! used to live in the central `lattice_protocol::Event` enum.
//! Each registers via [`lattice_protocol::register_event!`] so
//! `:describe-events` (M.5.3.c) and any future event-introspection
//! tooling sees them. The editor's
//! [`lattice_runtime::EventBus::publish_typed`] /
//! [`lattice_runtime::EventBus::subscribe_typed`] surface dispatches
//! and downcasts.
//!
//! Why these live here: the central `Event` enum was a single
//! sealed type that every feature crate had to edit to publish.
//! Moving LSP events to `lattice-lsp` puts them under their
//! owner's authority -- same model `lattice-mode` uses for the
//! `Mode` trait.

use std::path::PathBuf;
use std::sync::Arc;

use lattice_protocol::ids::DocumentId;

/// Fired after `lsp-mode` activates on a buffer and the
/// supervisor has queued the `textDocument/didOpen`. The
/// semantic counterpart to LSP's didOpen on the editor's bus:
/// the buffer is now LSP-tracked. Subscribers (statusline,
/// future plugin hooks, telemetry) react without polling
/// `App::active_modes`.
///
/// `path` is `None` for standalone-server / scratch-buffer
/// activations (auto-activation only fires for path-bearing
/// buffers today; a future revision may add a `server_ids`
/// slice when standalone-server semantics land).
#[derive(Debug, Clone)]
pub struct LspBufferAttached {
    pub id: DocumentId,
    pub path: Option<PathBuf>,
}

lattice_protocol::register_event!(
    LspBufferAttached,
    "lsp.buffer-attached",
    "Fired after lsp-mode activates on a buffer and the \
     supervisor's didOpen request has been queued.",
    "lattice-lsp",
);

/// Fired when `lsp-mode` deactivates on a buffer and the
/// supervisor has sent `textDocument/didClose` to attached
/// servers. The buffer remains open in the editor; only LSP
/// tracking ends. Server connection persists if other buffers
/// are still attached.
#[derive(Debug, Clone)]
pub struct LspBufferDetached {
    pub id: DocumentId,
    pub path: Option<PathBuf>,
}

lattice_protocol::register_event!(
    LspBufferDetached,
    "lsp.buffer-detached",
    "Fired after lsp-mode deactivates and the supervisor \
     has sent didClose to attached servers.",
    "lattice-lsp",
);

/// Fired when [`crate::LspLogger::log`] appends a record to a
/// log ring (subsystem-wide when `server_id` is `None`,
/// per-server otherwise). Subscribers (the App's drain hook)
/// refresh open `*lsp*` / `*lsp:<server>*` /
/// `*lsp:<server>:trace*` help buffers from the logger
/// snapshot so log views update live as records arrive.
///
/// Carries primitive `String` fields rather than typed
/// `LogLevel` / `LogSource` enums because the legacy bus
/// shape predated the typed-event surface. Future revisions
/// may switch to typed enums now that the boundary moved
/// inside `lattice-lsp`; subscribers can re-snapshot through
/// the logger when they need the typed value.
#[derive(Debug, Clone)]
pub struct LspLogPushed {
    /// `None` for subsystem-wide records; `Some(id)` per-server.
    pub server_id: Option<Arc<str>>,
    /// Severity tag (`"trace"`, `"debug"`, `"info"`, `"warn"`,
    /// `"error"`).
    pub level: String,
    /// Source tag (`"client"`, `"stderr"`, `"log"`, `"show"`,
    /// `"trace"`).
    pub source: String,
    /// The record's message text.
    pub message: String,
}

lattice_protocol::register_event!(
    LspLogPushed,
    "lsp.log-pushed",
    "Fired when LspLogger::log appends a record to a log ring.",
    "lattice-lsp",
);
