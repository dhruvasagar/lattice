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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lattice_protocol::event::AppliedEdit;
use lattice_protocol::ids::DocumentId;

/// Fired after `lsp-mode` activates on a buffer and the
/// supervisor has queued the `textDocument/didOpen`. The
/// semantic counterpart to LSP's didOpen on the editor's bus:
/// the buffer is now LSP-tracked. Subscribers (statusline,
/// future plugin hooks, telemetry) react without polling
/// `App::active_modes`.
///
/// Carries only the `DocumentId` so the event can be published
/// from `LspMode::on_activate` without the mode needing a
/// host-side path lookup. Subscribers that need the buffer's
/// path resolve it themselves via their App handle.
#[derive(Debug, Clone)]
pub struct LspBufferAttached {
    pub id: DocumentId,
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
    /// `None` for subsystem-wide records; `Some(id)` per-instance.
    /// Pairs with `workspace` -- both are `Some` for per-instance
    /// records (post-B'.2), both `None` for subsystem-wide.
    pub server_id: Option<Arc<str>>,
    /// `None` for subsystem-wide records; `Some(path)` per-instance.
    /// The workspace root the `(server_id, workspace)` actor was
    /// spawned against. Two `rust-analyzer` instances on different
    /// workspaces stay distinct via this field.
    pub workspace: Option<Arc<Path>>,
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

/// Fired when an attached server sends a `$/progress`
/// notification (LSP §3.16 work-done progress). The host
/// accumulates progress entries by (server_id, token) and
/// surfaces the most recent active one in the modeline.
///
/// `kind` carries the progress lifecycle phase:
/// - `LspProgressKind::Begin` -- new operation started.
/// - `LspProgressKind::Report` -- ongoing update.
/// - `LspProgressKind::End` -- operation finished; host
///   removes the entry.
#[derive(Debug, Clone)]
pub struct LspProgressUpdate {
    pub server_id: Arc<str>,
    pub token: String,
    pub kind: LspProgressKind,
    pub title: Option<String>,
    pub message: Option<String>,
    /// 0..=100 if the server reports it; `None` for
    /// indeterminate progress.
    pub percentage: Option<u32>,
    pub cancellable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LspProgressKind {
    Begin,
    Report,
    End,
}

lattice_protocol::register_event!(
    LspProgressUpdate,
    "lsp.progress-update",
    "Fired when a server sends $/progress. Host accumulates by \
     (server_id, token) and surfaces in the modeline.",
    "lattice-lsp",
);

/// Fired when an actor task exits (4.4.d). The supervisor
/// subscribes to this event to drive crash-detection
/// auto-restart: `Clean` exits (after a `Shutdown` command)
/// are ignored; `Unexpected` exits trigger the
/// supervisor's restart-with-backoff path. The supervisor
/// is the single subscriber today; plugin telemetry can
/// subscribe later without further plumbing.
#[derive(Debug, Clone)]
pub struct LspActorExited {
    pub server_id: Arc<str>,
    pub reason: LspActorExitReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LspActorExitReason {
    /// Actor exited because the supervisor (or last
    /// `ServerHandle`) requested shutdown -- no restart wanted.
    Clean,
    /// Actor exited because read_loop returned None (pipe
    /// closed, server crashed) or another unexpected condition.
    /// The supervisor restarts via the same path `:lsp-restart`
    /// uses, gated on the restart-history backoff window.
    Unexpected,
}

lattice_protocol::register_event!(
    LspActorExited,
    "lsp.actor-exited",
    "Fired when an actor task exits. The supervisor uses this for \
     crash-detection auto-restart; subscribers can also tap for \
     telemetry.",
    "lattice-lsp",
);

/// 4.4.g: fired when an attached server requests
/// `workspace/inlayHint/refresh`. The host invalidates every
/// cached inlay-hint entry for buffers attached to this
/// server; the next render tick re-issues `inlayHint` and
/// repopulates.
///
/// The actor replies `null` to the server inline (the request
/// is fire-and-forget from the LSP spec's perspective); the
/// cache invalidation flows over the typed event bus so the
/// App-side drain can mutate `lsp_inlay_hints_cache` without
/// the actor needing buffer-state access.
#[derive(Debug, Clone)]
pub struct LspInlayHintRefresh {
    pub server_id: Arc<str>,
}

lattice_protocol::register_event!(
    LspInlayHintRefresh,
    "lsp.inlay-hint-refresh",
    "Fired when a server requests workspace/inlayHint/refresh; the host \
     invalidates cached hints for buffers attached to that server.",
    "lattice-lsp",
);

/// 4.4.i: fired when an attached server requests
/// `workspace/semanticTokens/refresh`. The host invalidates
/// every cached semantic-tokens entry for buffers attached to
/// this server; the next render tick re-issues
/// `semanticTokens/full` (forcing a fresh baseline rather than
/// a delta against a now-stale `result_id`) and repopulates.
///
/// Same shape as `LspInlayHintRefresh`: the actor replies
/// `null` to the server inline, and cache invalidation flows
/// over the typed event bus so the App-side drain can mutate
/// `lsp_semantic_tokens_cache` without the actor reaching into
/// buffer state.
#[derive(Debug, Clone)]
pub struct LspSemanticTokensRefresh {
    pub server_id: Arc<str>,
}

lattice_protocol::register_event!(
    LspSemanticTokensRefresh,
    "lsp.semantic-tokens-refresh",
    "Fired when a server requests workspace/semanticTokens/refresh; the \
     host invalidates cached semantic tokens for buffers attached to \
     that server.",
    "lattice-lsp",
);

/// 4.4.j: fired when an attached server requests
/// `workspace/diagnostic/refresh`. The host invalidates the
/// pull-diagnostics `result_id` cache for every buffer attached
/// to this server; the next render tick re-issues
/// `textDocument/diagnostic` (without a `previous_result_id`,
/// so the server must answer with a `Full` report) and the
/// `DiagnosticsLayer` re-applies.
///
/// Same shape as the inlay-hint / semantic-tokens refreshes:
/// actor replies `null` inline and publishes this event so
/// the App-side drain can mutate `lsp_pull_diagnostics_cache`
/// without the actor reaching into buffer state.
#[derive(Debug, Clone)]
pub struct LspDiagnosticRefresh {
    pub server_id: Arc<str>,
}

lattice_protocol::register_event!(
    LspDiagnosticRefresh,
    "lsp.diagnostic-refresh",
    "Fired when a server requests workspace/diagnostic/refresh; the host \
     invalidates the pull-diagnostics result_id cache for buffers attached \
     to that server.",
    "lattice-lsp",
);

/// 4.5.d: server-issued `workspace/codeLens/refresh` request
/// surfaces as this typed bus event. Same shape as the other
/// refresh events; the App-side drain evicts cached code
/// lenses for every buffer attached to the requesting server.
#[derive(Debug, Clone)]
pub struct LspCodeLensRefresh {
    pub server_id: Arc<str>,
}

lattice_protocol::register_event!(
    LspCodeLensRefresh,
    "lsp.code-lens-refresh",
    "Fired when a server requests workspace/codeLens/refresh; the host \
     evicts cached code lenses for buffers attached to that server so the \
     next render tick re-issues textDocument/codeLens.",
    "lattice-lsp",
);

/// Fired when a document buffer changes *and* `lsp-mode` is
/// active for that buffer (M.5.5). The per-actor fan-in
/// (`crate::fan_in`) subscribes via
/// `EventBus::subscribe_typed::<LspDocumentChanged>` and
/// forwards each `AppliedEdit` as a `RecordEdit` actor command.
///
/// `path` is `None` for path-less buffers (no URI to map);
/// fan_in skips those. The event isn't published at all when
/// `lsp-mode` is inactive, so fan_in never sees edits the user
/// has gated off.
#[derive(Debug, Clone)]
pub struct LspDocumentChanged {
    pub id: DocumentId,
    pub path: Option<PathBuf>,
    pub version: u64,
    pub edits: Vec<AppliedEdit>,
}

lattice_protocol::register_event!(
    LspDocumentChanged,
    "lsp.document-changed",
    "Fired on every buffer edit when lsp-mode is active. The \
     per-actor fan-in turns each AppliedEdit into a didChange \
     payload bound for attached LSP servers.",
    "lattice-lsp",
);
