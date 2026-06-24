//! Per-server actor (DESIGN.md §5.4 + §5.7). One tokio task owns
//! the wire-side state for one (workspace, server-id) pair:
//!
//! - The pending-request table (RequestId → oneshot to the
//!   editor-side caller).
//! - The negotiated [`Capabilities`].
//! - A monotonic JSON-RPC request-id counter.
//!
//! Two helper tasks fan I/O in and out:
//!
//! - **read_loop** -- reads [`Message`]s from `LspReader` and
//!   pushes them into an inbound channel.
//! - **write_loop** -- receives [`Message`]s on an outbound
//!   channel and writes them through `LspWriter`.
//!
//! ## Why three tasks
//!
//! A single-task design works for a typewriter-pace editor but
//! collapses under burst loads (a server emitting hundreds of
//! `$/progress` notifications during indexing while the editor
//! is also sending `didChange` per keystroke). Splitting reads
//! and writes onto separate tasks lets the OS schedule them
//! across cores; the actor task itself stays cheap (no I/O).
//!
//! ## Lifecycle
//!
//! [`spawn`] runs the initialize handshake before returning a
//! [`ServerHandle`]. Failure during handshake yields
//! [`LspError::HandshakeFailed`] and tears the child process
//! down via `kill_on_drop`.
//!
//! [`ServerHandle::shutdown`] runs the LSP shutdown protocol:
//! `shutdown` request → `exit` notification → wait for child
//! exit. The actor task exits cleanly and all pending
//! requests resolve with [`LspError::Cancelled`].

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use arc_swap::ArcSwap;
use lsp_types::{
    ClientInfo, InitializeParams, InitializeResult, InitializedParams, Uri, WorkspaceFolder,
};
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncWrite};
use tokio::process::{Child, ChildStderr};
use tokio::select;
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::capabilities::{self, Capabilities};
use crate::codec::{LspReader, LspWriter};
use crate::config::ServerConfig;
use crate::diagnostics::{DiagnosticEvent, DiagnosticsBus};
use crate::error::{LspError, LspResult};
use crate::jsonrpc::{Message, Notification, Request, RequestId, Response};
use crate::logging::{LogLevel, LogSource, LspLogger};
use crate::pending::{InvocationId, Pending};
use crate::transport::ChildTransport;

/// Editor-facing handle to one running language-server actor.
///
/// Cheap to clone (`Arc` internally); the editor passes one
/// around per buffer / pane that talks to this server. Dropping
/// the last clone closes the mailbox -- the actor sees the
/// channel close and runs the LSP shutdown sequence on its way
/// out, so no leak.
#[derive(Clone)]
pub struct ServerHandle {
    inner: Arc<HandleInner>,
}

struct HandleInner {
    cmd_tx: mpsc::UnboundedSender<ActorCmd>,
    /// Negotiated capabilities, published as an
    /// `ArcSwap<Capabilities>` (4.4.n). The handshake snapshot
    /// goes in at handshake-time; subsequent
    /// `client/registerCapability` and
    /// `client/unregisterCapability` notifications publish new
    /// snapshots in place. Readers
    /// ([`ServerHandle::capabilities`]) load a fresh `Arc` per
    /// call -- lock-free and as cheap as an atomic load.
    capabilities: Arc<ArcSwap<Capabilities>>,
    /// Server id, for logs / telemetry.
    server_id: String,
    /// Workspace root this actor was spawned against (B'.2).
    /// Pairs with `server_id` as the canonical
    /// `(server_id, workspace)` instance key so multi-instance
    /// setups stay distinct in the per-instance log rings.
    workspace_root: Arc<std::path::Path>,
    /// Diagnostics broadcast bus -- subscribers (App, plugins,
    /// future picker) receive every `publishDiagnostics` from
    /// this server.
    diagnostics: DiagnosticsBus,
    /// LSP-subsystem logger. Cloned from the App's shared
    /// logger; per-instance records land in the
    /// `*lsp:<server>:<workspace>*` ring.
    logger: LspLogger,
}

impl std::fmt::Debug for ServerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerHandle")
            .field("server_id", &self.inner.server_id)
            .finish_non_exhaustive()
    }
}

impl ServerHandle {
    /// Snapshot of the current negotiated capabilities. Each
    /// call returns a fresh `Arc` that includes any dynamic
    /// registrations the server has issued since handshake
    /// (4.4.n). Callers that need the capability set to be
    /// stable for a multi-step decision should bind the
    /// snapshot to a local variable; subsequent
    /// `capabilities()` calls see whatever the actor has
    /// published in the interim.
    pub fn capabilities(&self) -> Arc<Capabilities> {
        self.inner.capabilities.load_full()
    }

    /// Server's stable id (e.g. `"rust"`). Useful for logs and
    /// for the supervisor to look up `ServerConfig`.
    pub fn server_id(&self) -> &str {
        &self.inner.server_id
    }

    /// Workspace root this actor was spawned against. Pairs with
    /// `server_id()` as the canonical instance identity (B'.2);
    /// cheap clone (Arc bump).
    pub fn workspace_root(&self) -> Arc<std::path::Path> {
        Arc::clone(&self.inner.workspace_root)
    }

    /// Build the `(server_id, workspace)` instance key for use
    /// with [`LspLogger::log`] and the per-instance log ring.
    pub fn instance(&self) -> crate::logging::InstanceKey {
        crate::logging::InstanceKey::new(
            Arc::<str>::from(self.inner.server_id.as_str()),
            Arc::clone(&self.inner.workspace_root),
        )
    }

    /// Subscribe to this server's diagnostics broadcast. Each
    /// subscriber receives every `publishDiagnostics` event
    /// after the call -- prior events are not replayed
    /// (a freshly opened pane re-issues the URIs it cares about
    /// to the server, which republishes diagnostics for those).
    ///
    /// The returned `Receiver` is the standard
    /// `tokio::sync::broadcast::Receiver`. A lagging consumer
    /// drops oldest first; reconcile by tracking the latest
    /// `version` per URI and ignoring events older than the
    /// editor's view of the doc.
    pub fn subscribe_diagnostics(&self) -> tokio::sync::broadcast::Receiver<DiagnosticEvent> {
        self.inner.diagnostics.subscribe()
    }

    /// True iff at least one subscriber is currently listening.
    /// Used by tests; production code doesn't need this.
    pub fn diagnostics_subscriber_count(&self) -> usize {
        self.inner.diagnostics.receiver_count()
    }

    /// Borrow the logger this actor emits through. The App
    /// holds the same `LspLogger` (cloned) and uses it for
    /// supervisor-side records.
    pub fn logger(&self) -> &LspLogger {
        &self.inner.logger
    }

    /// Send a typed JSON-RPC request and return a [`Pending`]
    /// resolving to the deserialized response.
    ///
    /// `R` must match the server's response shape for `method`;
    /// a mismatch surfaces as [`LspError::ResponseDecode`].
    pub fn request<P, R>(&self, method: &str, params: P) -> Pending<R>
    where
        P: serde::Serialize,
        R: serde::de::DeserializeOwned + Send + 'static,
    {
        let params_json = match serde_json::to_value(params) {
            Ok(v) => Some(v),
            Err(e) => return Pending::ready_err(LspError::ResponseDecode(e)),
        };
        let (reply_tx, reply_rx) = oneshot::channel::<LspResult<Value>>();
        let cmd = ActorCmd::Request {
            method: method.to_string(),
            params: params_json,
            reply: reply_tx,
        };
        if self.inner.cmd_tx.send(cmd).is_err() {
            return Pending::ready_err(LspError::ActorGone);
        }
        // Adapt Value → R inside a small relay so the public
        // API returns Pending<R> rather than Pending<Value>.
        let id = InvocationId::next();
        let (tx, rx) = oneshot::channel::<LspResult<R>>();
        tokio::spawn(async move {
            let result = match reply_rx.await {
                Ok(Ok(v)) => match serde_json::from_value::<R>(v) {
                    Ok(r) => Ok(r),
                    Err(e) => Err(LspError::ResponseDecode(e)),
                },
                Ok(Err(e)) => Err(e),
                Err(_) => Err(LspError::ResponseDropped),
            };
            let _ = tx.send(result);
        });
        Pending::new(id, rx)
    }

    /// Same as [`Self::request`] but with cooperative cancellation
    /// driven by a [`lattice_protocol::CancellationToken`]. While
    /// the relay task is awaiting the response from the actor, it
    /// also polls the token; if the token flips before the response
    /// arrives, the relay resolves with [`LspError::Cancelled`].
    ///
    /// **Local-only cancellation today.** The server may keep
    /// computing -- we just drop its result if it arrives stale.
    /// `$/cancelRequest` over the wire is a Phase 4.2 polish item
    /// (requires plumbing the JSON-RPC id back from the actor for
    /// server-side cancel; not on a hot path).
    ///
    /// Used by every Phase 4.2 navigation feature
    /// ([`Self::hover`] / [`Self::goto_definition`] / ...) so
    /// stale popups don't appear after the user moves on.
    pub fn request_with_cancel<P, R>(
        &self,
        method: &str,
        params: P,
        token: lattice_protocol::CancellationToken,
    ) -> Pending<R>
    where
        P: serde::Serialize,
        R: serde::de::DeserializeOwned + Send + 'static,
    {
        let params_json = match serde_json::to_value(params) {
            Ok(v) => Some(v),
            Err(e) => return Pending::ready_err(LspError::ResponseDecode(e)),
        };
        let (reply_tx, mut reply_rx) = oneshot::channel::<LspResult<Value>>();
        let cmd = ActorCmd::Request {
            method: method.to_string(),
            params: params_json,
            reply: reply_tx,
        };
        if self.inner.cmd_tx.send(cmd).is_err() {
            return Pending::ready_err(LspError::ActorGone);
        }
        let id = InvocationId::next();
        let (tx, rx) = oneshot::channel::<LspResult<R>>();
        tokio::spawn(async move {
            // 10ms poll cadence balances responsiveness (a typical
            // human-perceptible delay is >50ms) against wakeup
            // overhead. The token is an `Arc<AtomicBool>`; the poll
            // is one Acquire load.
            let result = loop {
                tokio::select! {
                    biased;
                    v = &mut reply_rx => {
                        break match v {
                            Ok(Ok(v)) => match serde_json::from_value::<R>(v) {
                                Ok(r) => Ok(r),
                                Err(e) => Err(LspError::ResponseDecode(e)),
                            },
                            Ok(Err(e)) => Err(e),
                            Err(_) => Err(LspError::ResponseDropped),
                        };
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                        if token.is_cancelled() {
                            break Err(LspError::Cancelled);
                        }
                    }
                }
            };
            let _ = tx.send(result);
        });
        Pending::new(id, rx)
    }

    /// Fire a JSON-RPC notification (no response expected).
    pub fn notify<P: serde::Serialize>(&self, method: &str, params: P) -> LspResult<()> {
        let params_json = serde_json::to_value(params).map_err(LspError::ResponseDecode)?;
        let cmd = ActorCmd::Notify {
            method: method.to_string(),
            params: Some(params_json),
        };
        self.inner.cmd_tx.send(cmd).map_err(|_| LspError::ActorGone)
    }

    /// 4.4.b: send `$/setTrace { value }` to the server (LSP
    /// §3.18). `Off` silences trace records; `Messages` ships
    /// the wire shapes; `Verbose` ships shapes + parameter
    /// contents. The server replies with `$/logTrace`
    /// notifications which the host routes into the
    /// `*lsp:<server>:trace*` ring.
    pub fn set_trace(&self, value: lsp_types::TraceValue) -> LspResult<()> {
        self.notify("$/setTrace", lsp_types::SetTraceParams { value })
    }

    /// Cancel an in-flight server-side `$/progress` operation
    /// (LSP §3.16 `window/workDoneProgress/cancel`). The server
    /// is asked to wind down the work tied to `token`; whether
    /// it complies is server-specific. The host treats the
    /// cancel as best-effort — the modeline keeps the entry
    /// until an `end` progress notification arrives.
    pub fn cancel_progress(&self, token: &str) -> LspResult<()> {
        // Wire shape: { "token": <number | string> }. We always
        // serialise as a string here; that's the canonical
        // representation the host uses to key its accumulator,
        // and servers accept either form.
        self.notify(
            "window/workDoneProgress/cancel",
            serde_json::json!({ "token": token }),
        )
    }

    /// Cancel a pending request by JSON-RPC numeric id (LSP
    /// `$/cancelRequest`). The actor sends the cancel
    /// notification and resolves the matching pending oneshot
    /// with [`LspError::Cancelled`].
    ///
    /// Note: the JSON-RPC id is server-internal -- callers
    /// usually don't have it. The higher-level cancellation
    /// path uses [`lattice_runtime::CancellationToken`] which
    /// the editor binds to a request via `request_with_cancel`
    /// (added in 4.2 alongside the navigation features).
    pub fn cancel(&self, jsonrpc_id: i64) -> LspResult<()> {
        let cmd = ActorCmd::Cancel { id: jsonrpc_id };
        self.inner.cmd_tx.send(cmd).map_err(|_| LspError::ActorGone)
    }

    /// Open a document in the actor's own DocSync mirror and emit
    /// `textDocument/didOpen`. Single-writer through the actor:
    /// the supervisor mutex is no longer in this path, so the UI
    /// thread cannot stall behind a flush. FIFO ordering with
    /// subsequent `record_edit` calls is preserved by the cmd
    /// channel.
    pub fn open_doc(
        &self,
        uri: lsp_types::Uri,
        language_id: impl Into<String>,
        text: impl Into<String>,
    ) -> LspResult<()> {
        let cmd = ActorCmd::OpenDoc {
            uri,
            language_id: language_id.into(),
            text: text.into(),
        };
        self.inner.cmd_tx.send(cmd).map_err(|_| LspError::ActorGone)
    }

    /// Record an edit against the actor's mirror. Coalesced with
    /// other edits and flushed after the actor's debounce window.
    /// Drop-free: the cmd channel is unbounded, so the publisher
    /// (typically the per-server fan-in task) cannot lose work.
    pub fn record_edit(
        &self,
        uri: lsp_types::Uri,
        edit: lattice_protocol::edit::Edit,
    ) -> LspResult<()> {
        let cmd = ActorCmd::RecordEdit { uri, edit };
        self.inner.cmd_tx.send(cmd).map_err(|_| LspError::ActorGone)
    }

    /// Force a flush of the pending change queue for one URI.
    /// Useful before a synchronous request that depends on the
    /// server having seen the latest text (hover/definition right
    /// after typing).
    pub fn flush(&self, uri: lsp_types::Uri) -> LspResult<()> {
        let cmd = ActorCmd::Flush { uri };
        self.inner.cmd_tx.send(cmd).map_err(|_| LspError::ActorGone)
    }

    /// Force a flush of every URI tracked by this actor.
    pub fn flush_all(&self) -> LspResult<()> {
        let cmd = ActorCmd::FlushAll;
        self.inner.cmd_tx.send(cmd).map_err(|_| LspError::ActorGone)
    }

    /// Close a document: emit any final `textDocument/didChange`
    /// then `textDocument/didClose`, all from inside the actor.
    pub fn close_doc(&self, uri: lsp_types::Uri) -> LspResult<()> {
        let cmd = ActorCmd::CloseDoc { uri };
        self.inner.cmd_tx.send(cmd).map_err(|_| LspError::ActorGone)
    }

    /// Run the LSP shutdown sequence: `shutdown` request → `exit`
    /// notification → wait for child exit. After this resolves
    /// the actor task is gone; subsequent requests yield
    /// [`LspError::ActorGone`].
    pub fn shutdown(&self) -> Pending<()> {
        let id = InvocationId::next();
        let (tx, rx) = oneshot::channel::<LspResult<()>>();
        let cmd = ActorCmd::Shutdown { reply: tx };
        if self.inner.cmd_tx.send(cmd).is_err() {
            return Pending::ready_err(LspError::ActorGone);
        }
        Pending::new(id, rx)
    }
}

/// Internal actor commands -- not part of the public API.
enum ActorCmd {
    Request {
        method: String,
        params: Option<Value>,
        reply: oneshot::Sender<LspResult<Value>>,
    },
    Notify {
        method: String,
        params: Option<Value>,
    },
    Cancel {
        id: i64,
    },
    Shutdown {
        reply: oneshot::Sender<LspResult<()>>,
    },
    /// Bring a buffer under the actor's DocSync management. The
    /// actor builds the `didOpen` payload via `DocSync::open` +
    /// ships it. v1 is fire-and-forget -- if the channel push
    /// fails the caller already lost the connection. Phase 4.x
    /// per-actor edit-path refactor.
    OpenDoc {
        uri: lsp_types::Uri,
        language_id: String,
        text: String,
    },
    /// Apply one committed edit to the actor's DocSync mirror
    /// + queue the `didChange` event. The per-actor debounce
    /// timer drives the eventual flush; rapid edits coalesce.
    /// Phase 4.x.
    RecordEdit {
        uri: lsp_types::Uri,
        edit: lattice_protocol::edit::Edit,
    },
    /// Eagerly drain queued change events for `uri` and ship a
    /// `didChange`. Used by will-save hooks etc. that need a
    /// coherent server-side view RIGHT NOW. Phase 4.x.
    Flush {
        uri: lsp_types::Uri,
    },
    /// Same as `Flush` but for every URI the actor tracks.
    /// Used at editor shutdown. Phase 4.x.
    FlushAll,
    /// Drop a buffer's mirror; ship the optional final
    /// `didChange` followed by `didClose`. Phase 4.x.
    CloseDoc {
        uri: lsp_types::Uri,
    },
}

/// Spawn a language server from a [`ServerConfig`].
///
/// Performs the initialize handshake before returning. Failures
/// at any handshake step (spawn, framing, decode, server error
/// response, missing required capability) surface as
/// [`LspError`]. `logger` is the shared subsystem logger -- in
/// production every server uses the App's clone; tests pass a
/// fresh `LspLogger::with_defaults()`.
pub async fn spawn(
    config: ServerConfig,
    workspace_root: std::path::PathBuf,
    logger: LspLogger,
    apply_edit_bus: Option<crate::apply_edit::ApplyEditBus>,
    configuration_bus: Option<crate::configuration::ConfigurationBus>,
    show_document_bus: Option<crate::show_document::ShowDocumentBus>,
    show_message_request_bus: Option<crate::show_message_request::ShowMessageRequestBus>,
    event_bus: Option<Arc<lattice_runtime::EventBus>>,
) -> LspResult<ServerHandle> {
    let transport = ChildTransport::spawn(&config.binary, &config.args, Some(&workspace_root))
        .await
        .map_err(LspError::Transport)?;
    let (reader, writer, stderr, child) = transport.split();
    spawn_with_io(
        config,
        workspace_root,
        reader,
        writer,
        stderr,
        Some(child),
        logger,
        apply_edit_bus,
        configuration_bus,
        show_document_bus,
        show_message_request_bus,
        event_bus,
    )
    .await
}

/// Spawn the actor against pre-existing `LspReader` / `LspWriter`
/// halves. Used by tests (mock server over a duplex pipe) and
/// by future embedded transports (TCP, named pipe).
///
/// `child` is `None` for in-process tests (the duplex partner is
/// a tokio task) and `Some(_)` for the real child-process path.
/// When None, the shutdown sequence skips the child-exit wait.
#[allow(clippy::too_many_arguments)]
pub async fn spawn_with_io<R, W>(
    config: ServerConfig,
    workspace_root: std::path::PathBuf,
    reader: LspReader<R>,
    writer: LspWriter<W>,
    stderr: Option<ChildStderr>,
    child: Option<Child>,
    logger: LspLogger,
    apply_edit_bus: Option<crate::apply_edit::ApplyEditBus>,
    configuration_bus: Option<crate::configuration::ConfigurationBus>,
    show_document_bus: Option<crate::show_document::ShowDocumentBus>,
    show_message_request_bus: Option<crate::show_message_request::ShowMessageRequestBus>,
    event_bus: Option<Arc<lattice_runtime::EventBus>>,
) -> LspResult<ServerHandle>
where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<ActorCmd>();
    // 4.4.n: handshake hands back the published `ArcSwap` cell
    // (not a plain `Arc<Capabilities>`) so the host's handle and
    // the actor task share one publication point. After
    // handshake the actor swaps in fresh snapshots whenever
    // `client/(un)registerCapability` modifies the dynamic
    // registry; readers (`ServerHandle::capabilities()`) load
    // through the same cell.
    let (handshake_tx, handshake_rx) = oneshot::channel::<LspResult<Arc<ArcSwap<Capabilities>>>>();

    let server_id = config.id.clone();
    let init_options = config.initialization_options.clone();
    let workspace_folder_uri = uri_from_path(&workspace_root);
    let workspace_name = workspace_root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "workspace".to_string());
    // B'.2: the actor's logger calls carry the canonical
    // `(server_id, workspace)` pair so multi-instance setups
    // (two `rust-analyzer`s on different workspaces) stay
    // distinct in the per-instance log rings.
    let instance = crate::logging::InstanceKey::new(
        Arc::<str>::from(server_id.as_str()),
        Arc::<std::path::Path>::from(workspace_root.as_path()),
    );
    // One bus per actor, shared with the read_loop's notification
    // dispatcher.
    let diagnostics = DiagnosticsBus::new();

    // Subsystem-wide event: server spawn / handshake start.
    logger.log(
        None,
        LogLevel::Info,
        LogSource::Client,
        format!(
            "spawning LSP actor for server {:?} workspace {}",
            server_id,
            workspace_root.display()
        ),
    );

    tokio::spawn(actor_main(
        reader,
        writer,
        child,
        stderr,
        cmd_rx,
        handshake_tx,
        server_id.clone(),
        instance.clone(),
        workspace_folder_uri,
        workspace_name,
        init_options,
        diagnostics.clone(),
        logger.clone(),
        apply_edit_bus,
        configuration_bus,
        show_document_bus,
        show_message_request_bus,
        event_bus,
    ));

    let capabilities = handshake_rx
        .await
        .map_err(|_| LspError::HandshakeFailed("actor died before handshake".into()))??;

    logger.log(
        Some(&instance),
        LogLevel::Info,
        LogSource::Client,
        "handshake complete; server attached",
    );
    let _server_id_arc: Arc<str> = Arc::clone(&instance.server_id);
    let workspace_root_arc: Arc<std::path::Path> = Arc::clone(&instance.workspace);

    Ok(ServerHandle {
        inner: Arc::new(HandleInner {
            cmd_tx,
            // 4.4.n: ArcSwap shared with the actor task so
            // register / unregister updates are observable
            // from readers without restarting the actor.
            capabilities,
            server_id,
            workspace_root: workspace_root_arc,
            diagnostics,
            logger,
        }),
    })
}

/// Convert a filesystem path to a `file://` URI in the form LSP
/// expects. lsp-types 0.97 dropped the `url` crate; we
/// percent-encode manually for the small set of bytes that
/// matter in a path (space → `%20`, etc.). Servers in practice
/// tolerate plain `file:///<path>` without aggressive encoding.
/// Inverse of [`uri_from_path`]. Strips the `file://` scheme +
/// percent-decodes the small set of bytes the encoder rewrites.
/// Returns the path string (caller decides whether to coerce to
/// `PathBuf`); `None` for non-`file://` URIs.
pub fn uri_to_path(uri: &Uri) -> Option<std::path::PathBuf> {
    let s = uri.as_str();
    let stripped = s.strip_prefix("file://")?;
    // Percent-decode the small set the encoder writes. More
    // exotic encodings (CJK paths etc) round-trip via the
    // identity branch in uri_from_path.
    let mut out = String::with_capacity(stripped.len());
    let mut chars = stripped.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            let h1 = chars.next();
            let h2 = chars.next();
            if let (Some(h1), Some(h2)) = (h1, h2)
                && let (Some(h1), Some(h2)) = (h1.to_digit(16), h2.to_digit(16))
            {
                let byte = (h1 * 16 + h2) as u8;
                out.push(byte as char);
                continue;
            }
            // Malformed; keep the literal `%` and any consumed chars.
            out.push('%');
            if let Some(h1) = h1 {
                out.push(h1);
            }
            if let Some(h2) = h2 {
                out.push(h2);
            }
        } else {
            out.push(c);
        }
    }
    Some(std::path::PathBuf::from(out))
}

pub fn uri_from_path(p: &std::path::Path) -> Uri {
    // Promote relative paths to absolute *before* building the URI.
    // `file:///crates/lattice-core/src/buffer.rs` is interpreted by
    // every LSP server as `/crates/lattice-core/...` (root-rooted),
    // not as "the file the editor opened from a relative arg" --
    // rust-analyzer then can't find the file inside its workspace,
    // returns null hovers / definitions, and emits notify-watcher
    // warnings about paths that don't exist on disk. The fix is to
    // canonicalise to an absolute path here so every URI we send is
    // wire-correct regardless of how the user invoked the editor
    // (relative path on the cli, etc.).
    //
    // `std::path::absolute` does NOT do I/O and does NOT resolve
    // symlinks -- both are deliberate. We just want
    // `cwd().join(p)`-shaped output. If absolute() fails (very rare
    // -- malformed path on Windows mostly), fall back to the
    // original; the URI may then still be wrong but the server's
    // existing failure mode (null reply + warning) is no worse than
    // before this fix.
    let absolute = std::path::absolute(p)
        .ok()
        .unwrap_or_else(|| p.to_path_buf());
    let display = absolute.to_string_lossy();
    // Normalise Windows backslashes to forward slashes so the
    // URI is well-formed across platforms. Drive letters
    // (`C:\`) remain in `<C:/path/...>` form, which is what LSP
    // servers expect.
    let normalised = display.replace('\\', "/");
    let raw = if normalised.starts_with('/') {
        format!("file://{normalised}")
    } else {
        format!("file:///{normalised}")
    };
    // Percent-encode the small set of byte classes that fluent-uri
    // rejects (spaces, control chars). Anything else passes through.
    let mut encoded = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            ' ' => encoded.push_str("%20"),
            '"' => encoded.push_str("%22"),
            '<' => encoded.push_str("%3C"),
            '>' => encoded.push_str("%3E"),
            '|' => encoded.push_str("%7C"),
            other => encoded.push(other),
        }
    }
    Uri::from_str(&encoded).unwrap_or_else(|_| {
        // If even that fails, fall back to a synthetic
        // host-only URI so the actor doesn't panic.
        Uri::from_str("file:///").expect("file:/// is a valid URI")
    })
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
async fn actor_main<R, W>(
    reader: LspReader<R>,
    writer: LspWriter<W>,
    mut child: Option<Child>,
    stderr: Option<ChildStderr>,
    mut cmd_rx: mpsc::UnboundedReceiver<ActorCmd>,
    handshake_tx: oneshot::Sender<LspResult<Arc<ArcSwap<Capabilities>>>>,
    server_id: String,
    instance: crate::logging::InstanceKey,
    workspace_folder_uri: Uri,
    workspace_name: String,
    init_options: Option<Value>,
    diagnostics: DiagnosticsBus,
    logger: LspLogger,
    apply_edit_bus: Option<crate::apply_edit::ApplyEditBus>,
    configuration_bus: Option<crate::configuration::ConfigurationBus>,
    show_document_bus: Option<crate::show_document::ShowDocumentBus>,
    show_message_request_bus: Option<crate::show_message_request::ShowMessageRequestBus>,
    event_bus: Option<Arc<lattice_runtime::EventBus>>,
) where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let server_id_arc: Arc<str> = Arc::clone(&instance.server_id);
    // Drain stderr through the logger -- each line lands in the
    // per-instance `*lsp:<server>:<workspace>*` ring at Warn
    // (server stderr is the canonical "something's up" signal).
    if let Some(stderr) = stderr {
        tokio::spawn(stderr_drain(stderr, instance.clone(), logger.clone()));
    }

    // Spawn write_loop. Mutex around the writer because
    // `write_loop` also writes the initialize request (via the
    // outbound channel) before the loop starts processing
    // mailbox commands. After spawn, the actor only writes
    // through the channel.
    let writer = Arc::new(Mutex::new(writer));
    let (out_tx, out_rx) = mpsc::unbounded_channel::<Message>();
    tokio::spawn(write_loop(
        Arc::clone(&writer),
        out_rx,
        instance.clone(),
        logger.clone(),
    ));

    // Spawn read_loop.
    let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Message>();
    tokio::spawn(read_loop(reader, in_tx, instance.clone(), logger.clone()));

    // Handshake.
    let mut next_id: u64 = 1;
    let init_id = RequestId::from_u64(next_id);
    next_id += 1;
    let init_params = build_initialize_params(workspace_folder_uri, workspace_name, init_options);
    let init_value = match serde_json::to_value(&init_params) {
        Ok(v) => v,
        Err(e) => {
            let _ = handshake_tx.send(Err(LspError::HandshakeFailed(format!(
                "could not serialize initialize params: {e}"
            ))));
            return;
        }
    };
    let req = Request::new(init_id.clone(), "initialize", Some(init_value));
    if out_tx.send(Message::Request(req)).is_err() {
        let _ = handshake_tx.send(Err(LspError::HandshakeFailed(
            "write_loop closed before initialize".into(),
        )));
        return;
    }

    // Wait for the matching initialize response. While waiting,
    // the server may send window/logMessage or $/progress -- log
    // them and keep waiting.
    let init_result = loop {
        match in_rx.recv().await {
            Some(Message::Response(r)) if r.id == init_id => break r,
            Some(other) => {
                handle_pre_handshake_message(
                    &instance,
                    other,
                    &diagnostics,
                    &logger,
                    event_bus.as_ref(),
                );
            }
            None => {
                let _ = handshake_tx.send(Err(LspError::HandshakeFailed(
                    "server stream closed before initialize response".into(),
                )));
                return;
            }
        }
    };

    let init_value = match init_result.error {
        Some(err) => {
            let _ = handshake_tx.send(Err(LspError::HandshakeFailed(format!(
                "server rejected initialize: {} ({})",
                err.message, err.code
            ))));
            return;
        }
        None => init_result.result.unwrap_or(Value::Null),
    };

    let server_caps = match serde_json::from_value::<InitializeResult>(init_value) {
        Ok(r) => r.capabilities,
        Err(e) => {
            let _ = handshake_tx.send(Err(LspError::HandshakeFailed(format!(
                "could not deserialize InitializeResult: {e}"
            ))));
            return;
        }
    };

    let caps = Capabilities::from_initialize(capabilities::client_capabilities(), server_caps);
    // 4.4.n: publish the handshake snapshot into the shared
    // ArcSwap cell. `caps_cell` is the publication point both
    // the host's `ServerHandle::capabilities()` and the
    // actor's own dynamic-registration code read / write.
    let caps_cell: Arc<ArcSwap<Capabilities>> = Arc::new(ArcSwap::new(Arc::clone(&caps)));
    // Local snapshot used by the actor task between mutations.
    // After `client/(un)registerCapability` we rebuild + store
    // a new `Arc<Capabilities>` and refresh this binding so
    // subsequent reads in this task see the latest state
    // without a load through the cell.
    let mut caps: Arc<Capabilities> = caps;

    // Send `initialized` notification per LSP base spec -- the
    // server is required to wait for this before processing
    // other requests.
    let initialized_value = serde_json::to_value(InitializedParams {}).unwrap_or(Value::Null);
    let _ = out_tx.send(Message::Notification(Notification::new(
        "initialized",
        Some(initialized_value),
    )));

    // Hand the handle back to the caller.
    if handshake_tx.send(Ok(Arc::clone(&caps_cell))).is_err() {
        // The caller dropped the spawn future -- nothing else to
        // do. Run shutdown locally.
        perform_shutdown(&out_tx, &mut in_rx, &mut next_id, child.as_mut()).await;
        return;
    }

    // Main loop.
    let mut pending: HashMap<RequestId, oneshot::Sender<LspResult<Value>>> = HashMap::new();
    let mut shutting_down: Option<oneshot::Sender<LspResult<()>>> = None;

    // Per-actor DocSync state (Phase 4.x edit-path refactor).
    // Owned by the actor so the select! loop is the single
    // writer to the mirror -- the incremental-sync invariant
    // is structurally guaranteed, no shared lock to misuse.
    let mut docsync = crate::sync::DocSync::new();
    // Per-actor debounce for `textDocument/didChange`. After
    // every RecordEdit we set `flush_deadline` to ~50ms in the
    // future; the select! arm guarded by `flush_pending` polls
    // the sleep, fires when the deadline hits, then clears the
    // flag. Rapid edits coalesce because each new RecordEdit
    // resets the deadline.
    const FLUSH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(50);
    let mut flush_pending = false;
    let flush_sleep = tokio::time::sleep(std::time::Duration::from_secs(60 * 60));
    tokio::pin!(flush_sleep);

    'main: loop {
        select! {
            // Debounced flush. Only polled when `flush_pending`
            // is true (a RecordEdit set it); fires once after
            // FLUSH_DEBOUNCE of idleness past the last edit.
            _ = &mut flush_sleep, if flush_pending => {
                flush_pending = false;
                for (uri, params) in docsync.take_flush_all_payloads(&caps) {
                    let n = Notification::new(
                        "textDocument/didChange",
                        Some(serde_json::to_value(params).unwrap_or(Value::Null)),
                    );
                    if out_tx.send(Message::Notification(n)).is_err() {
                        // write_loop dead -- bail.
                        break 'main;
                    }
                    let _ = uri;
                }
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(ActorCmd::Request { method, params, reply }) => {
                        let id = RequestId::from_u64(next_id); next_id += 1;
                        pending.insert(id.clone(), reply);
                        let req = Request::new(id, method, params);
                        if out_tx.send(Message::Request(req)).is_err() {
                            // write_loop dead -- everyone's pending
                            // request resolves with ActorGone.
                            break 'main;
                        }
                    }
                    Some(ActorCmd::Notify { method, params }) => {
                        let n = Notification::new(method, params);
                        let _ = out_tx.send(Message::Notification(n));
                    }
                    Some(ActorCmd::Cancel { id }) => {
                        let cancel = Notification::new(
                            "$/cancelRequest",
                            Some(serde_json::json!({"id": id})),
                        );
                        let _ = out_tx.send(Message::Notification(cancel));
                        // Resolve the matching pending entry locally
                        // so the caller doesn't wait for the server's
                        // ack.
                        let key = RequestId::Number(id);
                        if let Some(reply) = pending.remove(&key) {
                            let _ = reply.send(Err(LspError::Cancelled));
                        }
                    }
                    Some(ActorCmd::OpenDoc { uri, language_id, text }) => {
                        let params = docsync.open(uri, language_id, text);
                        let n = Notification::new(
                            "textDocument/didOpen",
                            Some(serde_json::to_value(params).unwrap_or(Value::Null)),
                        );
                        if out_tx.send(Message::Notification(n)).is_err() {
                            break 'main;
                        }
                    }
                    Some(ActorCmd::RecordEdit { uri, edit }) => {
                        // Single-writer to the mirror -- no locks,
                        // no drops. Invariant: every committed edit
                        // either applies cleanly here or logs a
                        // warning (mirror corruption is impossible
                        // because the actor owns the only mutator).
                        if let Err(e) = docsync.record_edit(&caps, &uri, &edit) {
                            logger.log(
                                Some(&instance),
                                LogLevel::Warn,
                                LogSource::Client,
                                format!(
                                    "actor.record_edit on {}: {e}",
                                    uri.as_str(),
                                ),
                            );
                        }
                        // Reset the debounce: rapid edits coalesce
                        // into one didChange after the idle window.
                        flush_sleep.as_mut().reset(
                            tokio::time::Instant::now() + FLUSH_DEBOUNCE,
                        );
                        flush_pending = true;
                    }
                    Some(ActorCmd::Flush { uri }) => {
                        if let Some(params) =
                            docsync.take_flush_payload(&caps, &uri)
                        {
                            let n = Notification::new(
                                "textDocument/didChange",
                                Some(serde_json::to_value(params).unwrap_or(Value::Null)),
                            );
                            if out_tx.send(Message::Notification(n)).is_err() {
                                break 'main;
                            }
                        }
                    }
                    Some(ActorCmd::FlushAll) => {
                        for (_uri, params) in
                            docsync.take_flush_all_payloads(&caps)
                        {
                            let n = Notification::new(
                                "textDocument/didChange",
                                Some(serde_json::to_value(params).unwrap_or(Value::Null)),
                            );
                            if out_tx.send(Message::Notification(n)).is_err() {
                                break 'main;
                            }
                        }
                        flush_pending = false;
                    }
                    Some(ActorCmd::CloseDoc { uri }) => {
                        if let Some(payloads) = docsync.close(&caps, &uri) {
                            if let Some(final_changes) = payloads.final_changes {
                                let n = Notification::new(
                                    "textDocument/didChange",
                                    Some(serde_json::to_value(final_changes)
                                        .unwrap_or(Value::Null)),
                                );
                                if out_tx.send(Message::Notification(n)).is_err() {
                                    break 'main;
                                }
                            }
                            let n = Notification::new(
                                "textDocument/didClose",
                                Some(serde_json::to_value(payloads.close)
                                    .unwrap_or(Value::Null)),
                            );
                            if out_tx.send(Message::Notification(n)).is_err() {
                                break 'main;
                            }
                        }
                    }
                    Some(ActorCmd::Shutdown { reply }) => {
                        shutting_down = Some(reply);
                        // Send the shutdown request; the response
                        // arrives back through in_rx and we then
                        // emit `exit`. We don't break the loop yet
                        // -- need to await the shutdown response.
                        // No `next_id += 1` here: we break out of
                        // the loop immediately after, so the bump
                        // would be dead.
                        let id = RequestId::from_u64(next_id);
                        let (sd_tx, sd_rx) = oneshot::channel::<LspResult<Value>>();
                        pending.insert(id.clone(), sd_tx);
                        let _ = out_tx.send(Message::Request(Request::new(
                            id,
                            "shutdown",
                            None,
                        )));
                        // Wait briefly for shutdown response, then
                        // emit exit. Bound at 5s to avoid hanging
                        // on a misbehaving server.
                        let timeout = tokio::time::sleep(std::time::Duration::from_secs(5));
                        tokio::pin!(timeout);
                        select! {
                            _ = sd_rx => {},
                            _ = &mut timeout => {
                                tracing::warn!(server_id, "shutdown response timed out; sending exit");
                            }
                        }
                        let _ = out_tx.send(Message::Notification(Notification::new(
                            "exit", None,
                        )));
                        if let Some(c) = child.as_mut() {
                            // Best-effort wait on child exit.
                            let _ = tokio::time::timeout(
                                std::time::Duration::from_secs(2),
                                c.wait(),
                            )
                            .await;
                        }
                        break 'main;
                    }
                    None => {
                        // Mailbox closed (last ServerHandle dropped).
                        // Run a graceful shutdown anyway.
                        perform_shutdown(&out_tx, &mut in_rx, &mut next_id, child.as_mut()).await;
                        break 'main;
                    }
                }
            },
            msg = in_rx.recv() => {
                match msg {
                    Some(Message::Response(r)) => {
                        if let Some(reply) = pending.remove(&r.id) {
                            let result = match r.error {
                                Some(e) => Err(LspError::Server(e)),
                                None => Ok(r.result.unwrap_or(Value::Null)),
                            };
                            let _ = reply.send(result);
                        } else {
                            tracing::warn!(server_id, ?r.id, "response with unknown id");
                        }
                    }
                    Some(Message::Notification(n)) => {
                        handle_server_notification(
                            &instance,
                            &n,
                            &diagnostics,
                            &logger,
                            event_bus.as_ref(),
                        );
                    }
                    Some(Message::Request(req)) => {
                        // `workspace/applyEdit` (Phase 4.3) is
                        // async: the App must apply the edit on
                        // the UI thread, so we forward the
                        // request through the apply-edit bus and
                        // a spawned task awaits the response
                        // before writing it back to the wire.
                        // All other server-initiated requests
                        // resolve synchronously inline.
                        if req.method == "workspace/applyEdit"
                            && let Some(bus) = apply_edit_bus.as_ref()
                        {
                            let bus = bus.clone();
                            let instance_clone = instance.clone();
                            let logger_clone = logger.clone();
                            let out_tx_clone = out_tx.clone();
                            let req_id = req.id.clone();
                            let params = req.params.clone();
                            tokio::spawn(async move {
                                let resp = handle_apply_edit_request(
                                    instance_clone,
                                    req_id,
                                    params,
                                    &bus,
                                    &logger_clone,
                                )
                                .await;
                                let _ = out_tx_clone.send(Message::Response(resp));
                            });
                        } else if req.method == "workspace/configuration"
                            && let Some(bus) = configuration_bus.as_ref()
                        {
                            // Phase 4.1 follow-up: real values
                            // (not just `null` per item) require
                            // the App to walk its cached TOML
                            // tree. Same async pattern as
                            // applyEdit -- dispatch via the bus,
                            // await response, ferry back.
                            let bus = bus.clone();
                            let instance_clone = instance.clone();
                            let logger_clone = logger.clone();
                            let out_tx_clone = out_tx.clone();
                            let req_id = req.id.clone();
                            let params = req.params.clone();
                            tokio::spawn(async move {
                                let resp = handle_configuration_request(
                                    instance_clone,
                                    req_id,
                                    params,
                                    &bus,
                                    &logger_clone,
                                )
                                .await;
                                let _ = out_tx_clone.send(Message::Response(resp));
                            });
                        } else if req.method == "window/showDocument"
                            && let Some(bus) = show_document_bus.as_ref()
                        {
                            // 4.4.b: server wants the host to open
                            // a URI (file -> buffer; external ->
                            // OS handler). The App's drain
                            // performs the open + writes back via
                            // the embedded oneshot.
                            let bus = bus.clone();
                            let instance_clone = instance.clone();
                            let logger_clone = logger.clone();
                            let out_tx_clone = out_tx.clone();
                            let req_id = req.id.clone();
                            let params = req.params.clone();
                            tokio::spawn(async move {
                                let resp = handle_show_document_request(
                                    instance_clone,
                                    req_id,
                                    params,
                                    &bus,
                                    &logger_clone,
                                )
                                .await;
                                let _ = out_tx_clone.send(Message::Response(resp));
                            });
                        } else if req.method == "window/showMessageRequest"
                            && let Some(bus) = show_message_request_bus.as_ref()
                        {
                            // 4.4.b: server-emitted modal action
                            // request. The App opens an action
                            // picker; the user's selection
                            // (or `null` on dismiss) ferries back.
                            let bus = bus.clone();
                            let instance_clone = instance.clone();
                            let logger_clone = logger.clone();
                            let out_tx_clone = out_tx.clone();
                            let req_id = req.id.clone();
                            let params = req.params.clone();
                            tokio::spawn(async move {
                                let resp = handle_show_message_request(
                                    instance_clone,
                                    req_id,
                                    params,
                                    &bus,
                                    &logger_clone,
                                )
                                .await;
                                let _ = out_tx_clone.send(Message::Response(resp));
                            });
                        } else if req.method == "workspace/inlayHint/refresh" {
                            // 4.4.g: server-initiated inlay
                            // hint cache invalidation. Reply
                            // `null` synchronously per spec;
                            // publish the typed
                            // `LspInlayHintRefresh` event so
                            // the App's drain clears cached
                            // hints for attached buffers and
                            // the next render tick re-issues
                            // `inlayHint`.
                            let _ = out_tx.send(Message::Response(Response::ok(
                                req.id.clone(),
                                Value::Null,
                            )));
                            if let Some(bus) = event_bus.as_ref() {
                                bus.publish_typed(crate::events::LspInlayHintRefresh {
                                    server_id: Arc::clone(&server_id_arc),
                                });
                            }
                        } else if req.method == "workspace/semanticTokens/refresh" {
                            // 4.4.i: server-initiated semantic
                            // tokens cache invalidation. Same
                            // shape as the inlay-hint refresh:
                            // reply `null` synchronously, publish
                            // `LspSemanticTokensRefresh` so the
                            // App's drain drops cached tokens
                            // (and any stale `result_id`) for
                            // attached buffers; the next render
                            // tick re-issues a `full` request to
                            // rebuild the baseline.
                            let _ = out_tx.send(Message::Response(Response::ok(
                                req.id.clone(),
                                Value::Null,
                            )));
                            if let Some(bus) = event_bus.as_ref() {
                                bus.publish_typed(crate::events::LspSemanticTokensRefresh {
                                    server_id: Arc::clone(&server_id_arc),
                                });
                            }
                        } else if req.method == "workspace/inlineValue/refresh" {
                            // 4.5.h: server-initiated inline-
                            // value cache invalidation. The
                            // renderer trigger is itself
                            // deferred (no debug-adapter
                            // integration yet); we still reply
                            // `null` per spec so the server's
                            // request resolves, and log so a
                            // future renderer wire-up can grep
                            // for the breadcrumb.
                            let _ = out_tx.send(Message::Response(Response::ok(
                                req.id.clone(),
                                Value::Null,
                            )));
                            logger.log(
                                Some(&instance),
                                LogLevel::Info,
                                LogSource::Client,
                                "workspace/inlineValue/refresh accepted (renderer trigger deferred)",
                            );
                        } else if req.method == "workspace/codeLens/refresh" {
                            // 4.5.d: server-initiated code-lens
                            // cache invalidation. Same shape as
                            // inlay-hint / semantic-tokens
                            // refreshes: reply `null` inline +
                            // publish so the App's drain evicts
                            // cached lenses for attached buffers
                            // and the next tick's pump re-issues
                            // `textDocument/codeLens`.
                            let _ = out_tx.send(Message::Response(Response::ok(
                                req.id.clone(),
                                Value::Null,
                            )));
                            if let Some(bus) = event_bus.as_ref() {
                                bus.publish_typed(crate::events::LspCodeLensRefresh {
                                    server_id: Arc::clone(&server_id_arc),
                                });
                            }
                        } else if req.method == "workspace/diagnostic/refresh" {
                            // 4.4.j: server-initiated pull-
                            // diagnostic invalidation. Reply
                            // `null` inline + publish so the
                            // App's drain evicts the per-buffer
                            // `result_id` cache. The next render
                            // tick re-pulls
                            // `textDocument/diagnostic` without
                            // a `previous_result_id`, forcing a
                            // `Full` report regardless of what
                            // the server had cached.
                            let _ = out_tx.send(Message::Response(Response::ok(
                                req.id.clone(),
                                Value::Null,
                            )));
                            if let Some(bus) = event_bus.as_ref() {
                                bus.publish_typed(crate::events::LspDiagnosticRefresh {
                                    server_id: Arc::clone(&server_id_arc),
                                });
                            }
                        } else if req.method == "client/registerCapability" {
                            // 4.4.n: parse the registration batch,
                            // fold every entry into the dynamic
                            // registry, publish a new caps snapshot,
                            // reply `null`. Parse errors degrade to
                            // a logged warning + still-reply-null --
                            // throwing the registration away is
                            // better than failing the request,
                            // which most servers treat as a fatal
                            // protocol error.
                            let parsed = req
                                .params
                                .as_ref()
                                .map(|p| {
                                    serde_json::from_value::<lsp_types::RegistrationParams>(
                                        p.clone(),
                                    )
                                });
                            match parsed {
                                Some(Ok(params)) => {
                                    caps = caps.with_dynamic_mut(|reg| {
                                        for r in params.registrations {
                                            reg.register(
                                                crate::DynamicRegistration {
                                                    id: r.id,
                                                    method: r.method,
                                                    register_options: r
                                                        .register_options,
                                                },
                                            );
                                        }
                                    });
                                    caps_cell.store(Arc::clone(&caps));
                                }
                                Some(Err(e)) => {
                                    logger.log(
                                        Some(&instance),
                                        LogLevel::Warn,
                                        LogSource::Client,
                                        format!(
                                            "client/registerCapability: malformed params, dropping ({e})"
                                        ),
                                    );
                                }
                                None => {
                                    logger.log(
                                        Some(&instance),
                                        LogLevel::Warn,
                                        LogSource::Client,
                                        "client/registerCapability: missing params",
                                    );
                                }
                            }
                            let _ = out_tx.send(Message::Response(Response::ok(
                                req.id.clone(),
                                Value::Null,
                            )));
                        } else if req.method == "client/unregisterCapability" {
                            // 4.4.n: evict each entry by id; publish.
                            // Unknown ids are silently dropped
                            // (see `DynamicRegistry::unregister`).
                            let parsed = req
                                .params
                                .as_ref()
                                .map(|p| {
                                    serde_json::from_value::<lsp_types::UnregistrationParams>(
                                        p.clone(),
                                    )
                                });
                            match parsed {
                                Some(Ok(params)) => {
                                    caps = caps.with_dynamic_mut(|reg| {
                                        for u in &params.unregisterations {
                                            reg.unregister(&u.id);
                                        }
                                    });
                                    caps_cell.store(Arc::clone(&caps));
                                }
                                Some(Err(e)) => {
                                    logger.log(
                                        Some(&instance),
                                        LogLevel::Warn,
                                        LogSource::Client,
                                        format!(
                                            "client/unregisterCapability: malformed params, dropping ({e})"
                                        ),
                                    );
                                }
                                None => {
                                    logger.log(
                                        Some(&instance),
                                        LogLevel::Warn,
                                        LogSource::Client,
                                        "client/unregisterCapability: missing params",
                                    );
                                }
                            }
                            let _ = out_tx.send(Message::Response(Response::ok(
                                req.id.clone(),
                                Value::Null,
                            )));
                        } else {
                            let resp = handle_server_request(&instance, &req, &logger);
                            let _ = out_tx.send(Message::Response(resp));
                        }
                    }
                    None => {
                        // read_loop ended -- server exited or pipe
                        // broke. Resolve all pending requests.
                        break 'main;
                    }
                }
            }
        }
    }

    // Drain pending: every outstanding caller resolves with the
    // appropriate failure.
    for (_id, reply) in pending.drain() {
        let _ = reply.send(Err(LspError::ActorGone));
    }
    let was_clean_shutdown = shutting_down.is_some();
    if let Some(reply) = shutting_down {
        let _ = reply.send(Ok(()));
    }
    // 4.4.d: publish actor-exited to the event bus so the
    // supervisor can drive auto-restart on unexpected exits.
    // Clean exits (the user / shutdown path requested it) are
    // tagged distinctly so the supervisor doesn't restart what
    // it just asked to die.
    if let Some(bus) = event_bus {
        let reason = if was_clean_shutdown {
            crate::events::LspActorExitReason::Clean
        } else {
            crate::events::LspActorExitReason::Unexpected
        };
        bus.publish_typed(crate::events::LspActorExited {
            server_id: server_id_arc.clone(),
            reason,
        });
    }
}

/// Handle a server-initiated notification. `publishDiagnostics`
/// fans out to the [`DiagnosticsBus`]; log / show / progress
/// notifications land in the per-server log ring.
fn handle_server_notification(
    instance: &crate::logging::InstanceKey,
    n: &Notification,
    diagnostics: &DiagnosticsBus,
    logger: &LspLogger,
    event_bus: Option<&Arc<lattice_runtime::EventBus>>,
) {
    let server_id = &instance.server_id;
    match n.method.as_str() {
        "window/logMessage" => {
            // LSP severity: 1=Error, 2=Warning, 3=Info, 4=Log/Debug.
            let (level, msg) = parse_window_message(&n.params);
            logger.log(Some(instance), level, LogSource::LspMessage, msg);
        }
        "window/showMessage" => {
            let (level, msg) = parse_window_message(&n.params);
            logger.log(Some(instance), level, LogSource::LspShowMessage, msg);
        }
        "$/progress" => {
            // Server-side work-done progress (LSP §3.16). Parse
            // the {token, value} envelope and publish a typed
            // `LspProgressUpdate` on the editor bus. The modeline
            // (and any plugin subscriber) accumulates by
            // (server_id, token).
            //
            // Logger still gets a Debug breadcrumb so the
            // `*lsp:<server>:trace*` ring keeps the raw record.
            logger.log(
                Some(instance),
                LogLevel::Debug,
                LogSource::Client,
                format!("$/progress: {}", compact_params(&n.params)),
            );
            if let (Some(bus), Some(update)) =
                (event_bus, parse_progress(server_id, n.params.as_ref()))
            {
                bus.publish_typed(update);
            }
        }
        "experimental/serverStatus" => {
            // L2: rust-analyzer readiness notification. `quiescent:
            // true` means the server finished indexing and features
            // (hover/diagnostics/completion) are reliable; `health`
            // reports ok/warning/error. Publish a typed event the
            // modeline turns into the ✓/⟳/✗ readiness glyph.
            logger.log(
                Some(instance),
                LogLevel::Debug,
                LogSource::Client,
                format!(
                    "experimental/serverStatus: {}",
                    compact_params(&n.params)
                ),
            );
            if let (Some(bus), Some(update)) =
                (event_bus, parse_server_status(server_id, n.params.as_ref()))
            {
                bus.publish_typed(update);
            }
        }
        "telemetry/event" => {
            // 4.4.a: distinct LogSource so plugin subscribers
            // on the typed event bus can filter
            // `source == "telemetry"` instead of parsing
            // free-form log text. Payload rides as the
            // compacted-JSON message tail; subscribers that
            // need structured access parse the suffix.
            logger.log(
                Some(instance),
                LogLevel::Debug,
                LogSource::Telemetry,
                compact_params(&n.params),
            );
        }
        "$/logTrace" => {
            // 4.4.b: server-emitted trace record. Shape:
            // `{ message: String, verbose: Option<String> }`.
            // Append both lines to the trace ring so the
            // `*lsp:<server>:trace*` buffer surfaces them; the
            // ring drops records by capacity, not by level, so
            // a verbose-mode session can produce a lot of data
            // -- that's intentional, the user opted in by
            // running `:lsp-trace`.
            let (message, verbose) = parse_log_trace(n.params.as_ref());
            logger.log(Some(instance), LogLevel::Trace, LogSource::Trace, message);
            if let Some(verbose) = verbose {
                logger.log(
                    Some(instance),
                    LogLevel::Trace,
                    LogSource::Trace,
                    format!("    {verbose}"),
                );
            }
        }
        "textDocument/publishDiagnostics" => {
            let params = match n.params.clone() {
                Some(v) => v,
                None => {
                    logger.log(
                        Some(instance),
                        LogLevel::Warn,
                        LogSource::Client,
                        "publishDiagnostics with empty params",
                    );
                    return;
                }
            };
            match serde_json::from_value::<lsp_types::PublishDiagnosticsParams>(params) {
                Ok(p) => {
                    let n_diags = p.diagnostics.len();
                    let uri = p.uri.as_str().to_string();
                    let event = DiagnosticEvent::from_lsp(Arc::clone(server_id), p);
                    diagnostics.publish(event);
                    logger.log(
                        Some(instance),
                        LogLevel::Debug,
                        LogSource::Client,
                        format!("publishDiagnostics: {n_diags} diag(s) for {uri}"),
                    );
                }
                Err(e) => {
                    logger.log(
                        Some(instance),
                        LogLevel::Warn,
                        LogSource::Client,
                        format!("publishDiagnostics deserialise failed: {e}"),
                    );
                }
            }
        }
        other => {
            logger.log(
                Some(instance),
                LogLevel::Debug,
                LogSource::Client,
                format!("unhandled server notification: {other}"),
            );
        }
    }
}

/// Parse a `$/progress` payload (LSP §3.16) into a typed
/// `LspProgressUpdate`. Returns `None` if the envelope is
/// missing fields or has the wrong shape — we'd rather drop
/// the update than publish a half-filled event.
///
/// Token can be number or string per spec; we serialise both
/// to `String` so the (server_id, token) accumulator key is
/// uniform.
fn parse_progress(
    server_id: &Arc<str>,
    params: Option<&Value>,
) -> Option<crate::events::LspProgressUpdate> {
    use crate::events::{LspProgressKind, LspProgressUpdate};
    let p = params?;
    let token = match p.get("token")? {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let value = p.get("value")?;
    let kind_str = value.get("kind")?.as_str()?;
    let kind = match kind_str {
        "begin" => LspProgressKind::Begin,
        "report" => LspProgressKind::Report,
        "end" => LspProgressKind::End,
        _ => return None,
    };
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let percentage = value
        .get("percentage")
        .and_then(Value::as_u64)
        .map(|n| n.min(100) as u32);
    let cancellable = value
        .get("cancellable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Some(LspProgressUpdate {
        server_id: Arc::clone(server_id),
        token,
        kind,
        title,
        message,
        percentage,
        cancellable,
    })
}

/// L2: parse an `experimental/serverStatus` params object
/// (rust-analyzer). Shape: `{ health: "ok"|"warning"|"error",
/// quiescent: bool, message?: string }`. Unknown / missing health
/// maps to `Ok` (treat as healthy); missing quiescent defaults to
/// `true` (assume ready rather than spin forever on a malformed
/// payload). Returns `None` only when params are absent.
fn parse_server_status(
    server_id: &Arc<str>,
    params: Option<&Value>,
) -> Option<crate::events::LspServerStatusChanged> {
    use crate::events::{LspServerHealth, LspServerStatusChanged};
    let p = params?;
    let health = match p.get("health").and_then(Value::as_str) {
        Some("error") => LspServerHealth::Error,
        Some("warning") => LspServerHealth::Warning,
        _ => LspServerHealth::Ok,
    };
    let quiescent = p
        .get("quiescent")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let message = p
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some(LspServerStatusChanged {
        server_id: Arc::clone(server_id),
        quiescent,
        health,
        message,
    })
}

/// 4.4.b: parse a `$/logTrace` params object. Returns
/// `(message, verbose_opt)`. Fallback for malformed shape:
/// the compacted-JSON tail as the message and no verbose.
fn parse_log_trace(params: Option<&Value>) -> (String, Option<String>) {
    let Some(p) = params else {
        return ("<empty $/logTrace params>".to_string(), None);
    };
    let message = p
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| compact_params(&Some(p.clone())));
    let verbose = p.get("verbose").and_then(Value::as_str).map(str::to_owned);
    (message, verbose)
}

/// Pull severity + message out of a `window/logMessage` /
/// `window/showMessage` params object. Defaults to Info /
/// "<unparseable>" on shape mismatch -- we never drop user
/// information silently.
fn parse_window_message(params: &Option<Value>) -> (LogLevel, String) {
    let Some(v) = params.as_ref() else {
        return (LogLevel::Info, "<empty params>".into());
    };
    let level = match v.get("type").and_then(|t| t.as_i64()) {
        Some(1) => LogLevel::Error,
        Some(2) => LogLevel::Warn,
        Some(3) => LogLevel::Info,
        Some(4) => LogLevel::Debug, // Log-class
        Some(5) => LogLevel::Debug, // LSP 3.18 Debug
        _ => LogLevel::Info,
    };
    let msg = v
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("<no message>")
        .to_string();
    (level, msg)
}

/// Render a JSON value to a single-line compact string. Used
/// for the log records so multi-line server payloads don't
/// blow the buffer view's per-row layout.
fn compact_params(params: &Option<Value>) -> String {
    match params {
        Some(v) => serde_json::to_string(v).unwrap_or_else(|_| "<unprintable>".into()),
        None => String::new(),
    }
}

/// Handle a `workspace/applyEdit` request asynchronously
/// (Phase 4.3). Parses the LSP params, dispatches them through
/// the apply-edit bus to the App's drain, awaits the App's
/// outcome via the embedded oneshot, and converts that into the
/// LSP `Response` body. Spec response shape:
/// `ApplyWorkspaceEditResponse { applied, failure_reason,
/// failed_change }`. We don't track `failed_change` today (the
/// per-file apply path is non-atomic), so it stays `None`.
///
/// Failure modes that surface as `applied: false`:
/// - The request params don't deserialize into
///   `ApplyWorkspaceEditParams`.
/// - The receiver dropped before the App could process the
///   edit (App is shutting down).
/// - The App reports `applied: false` with its own
///   `failure_reason`.
async fn handle_apply_edit_request(
    instance: crate::logging::InstanceKey,
    req_id: RequestId,
    params: Option<Value>,
    bus: &crate::apply_edit::ApplyEditBus,
    logger: &LspLogger,
) -> Response {
    let server_id = Arc::clone(&instance.server_id);
    let parsed: lsp_types::ApplyWorkspaceEditParams = match params {
        Some(v) => match serde_json::from_value(v) {
            Ok(p) => p,
            Err(e) => {
                logger.log(
                    Some(&instance),
                    LogLevel::Warn,
                    LogSource::Client,
                    format!("workspace/applyEdit: malformed params: {e}"),
                );
                return apply_edit_response(req_id, false, Some(format!("malformed params: {e}")));
            }
        },
        None => {
            return apply_edit_response(
                req_id,
                false,
                Some("workspace/applyEdit: missing params".into()),
            );
        }
    };
    let (response_tx, response_rx) = oneshot::channel();
    let inbound = crate::apply_edit::InboundApplyEdit {
        server_id: Arc::clone(&server_id),
        workspace: Arc::clone(&instance.workspace),
        label: parsed.label,
        edit: parsed.edit,
        response: response_tx,
    };
    // BC.8d: the apply-edit bus is now the generic `InboundBus` (host-drained
    // variant); `send` wakes the editor so the edit is applied off-keystroke —
    // same `Result<(), payload>` shape as the retired `ApplyEditBus::dispatch`.
    if bus.send(inbound).is_err() {
        return apply_edit_response(
            req_id,
            false,
            Some("client cannot apply edits (no receiver)".into()),
        );
    }
    match response_rx.await {
        Ok(outcome) => apply_edit_response(req_id, outcome.applied, outcome.failure_reason),
        Err(_) => apply_edit_response(
            req_id,
            false,
            Some("client did not respond before drop".into()),
        ),
    }
}

/// Build an `ApplyWorkspaceEditResponse`-shaped LSP `Response`.
/// The `failed_change` field stays `None` -- atomic-rollback +
/// per-change failure indexing land alongside future
/// `apply_workspace_edit_atomic` work.
fn apply_edit_response(
    req_id: RequestId,
    applied: bool,
    failure_reason: Option<String>,
) -> Response {
    let body = lsp_types::ApplyWorkspaceEditResponse {
        applied,
        failure_reason,
        failed_change: None,
    };
    match serde_json::to_value(body) {
        Ok(v) => Response::ok(req_id, v),
        Err(e) => Response::err(
            req_id,
            crate::jsonrpc::ResponseError {
                code: crate::jsonrpc::error_codes::INTERNAL_ERROR,
                message: format!("encode response: {e}"),
                data: None,
            },
        ),
    }
}

/// Handle a `workspace/configuration` request asynchronously
/// (Phase 4.1 follow-up). Parses the LSP `ConfigurationParams`,
/// dispatches each item's `section` through the configuration
/// bus to the App's drain, and converts the App's
/// `Vec<serde_json::Value>` reply into the spec-shaped
/// `Vec<Value>` response (one entry per requested item, in
/// input order; missing sections come back as `Value::Null`).
///
/// Failure modes that fall back to `[null, ...]` so the server
/// doesn't hang:
/// - Malformed params (logged as Warn).
/// - Receiver dropped before the App could respond.
async fn handle_configuration_request(
    instance: crate::logging::InstanceKey,
    req_id: RequestId,
    params: Option<Value>,
    bus: &crate::configuration::ConfigurationBus,
    logger: &LspLogger,
) -> Response {
    let server_id = Arc::clone(&instance.server_id);
    let parsed: lsp_types::ConfigurationParams = match params {
        Some(v) => match serde_json::from_value(v) {
            Ok(p) => p,
            Err(e) => {
                logger.log(
                    Some(&instance),
                    LogLevel::Warn,
                    LogSource::Client,
                    format!("workspace/configuration: malformed params: {e}"),
                );
                // Empty array reply -- the server's caller will
                // see "no items" rather than a hang.
                return Response::ok(req_id, Value::Array(Vec::new()));
            }
        },
        None => return Response::ok(req_id, Value::Array(Vec::new())),
    };
    let sections: Vec<String> = parsed
        .items
        .iter()
        .map(|i| i.section.clone().unwrap_or_default())
        .collect();
    let count = sections.len();
    let (response_tx, response_rx) = oneshot::channel();
    let inbound = crate::configuration::InboundConfigurationRequest {
        server_id: Arc::clone(&server_id),
        workspace: Arc::clone(&instance.workspace),
        sections,
        response: response_tx,
    };
    // BC.8b: the configuration bus is now the generic `InboundBus`; `send`
    // wakes the editor (off-keystroke reply) — same `Result<(), payload>` shape
    // as the retired `ConfigurationBus::dispatch`.
    if bus.send(inbound).is_err() {
        let arr: Vec<Value> = (0..count).map(|_| Value::Null).collect();
        return Response::ok(req_id, Value::Array(arr));
    }
    let values = match response_rx.await {
        Ok(v) => v,
        Err(_) => (0..count).map(|_| Value::Null).collect(),
    };
    Response::ok(req_id, Value::Array(values))
}

/// 4.4.b: `window/showDocument` request handler. Parses the
/// LSP shape, dispatches via the bus, awaits the App's
/// outcome, and ferries `ShowDocumentResult { success }` back.
/// Falls back to `success: false` on malformed params or
/// receiver-dropped — the spec lets clients refuse and we
/// prefer that over hanging the server.
async fn handle_show_document_request(
    instance: crate::logging::InstanceKey,
    req_id: RequestId,
    params: Option<Value>,
    bus: &crate::show_document::ShowDocumentBus,
    logger: &LspLogger,
) -> Response {
    let server_id = Arc::clone(&instance.server_id);
    let parsed: lsp_types::ShowDocumentParams = match params {
        Some(v) => match serde_json::from_value(v) {
            Ok(p) => p,
            Err(e) => {
                logger.log(
                    Some(&instance),
                    LogLevel::Warn,
                    LogSource::Client,
                    format!("window/showDocument: malformed params: {e}"),
                );
                return show_document_response(req_id, false);
            }
        },
        None => return show_document_response(req_id, false),
    };
    let (response_tx, response_rx) = oneshot::channel();
    let inbound = crate::show_document::InboundShowDocument {
        server_id: Arc::clone(&server_id),
        workspace: Arc::clone(&instance.workspace),
        uri: parsed.uri,
        external: parsed.external.unwrap_or(false),
        take_focus: parsed.take_focus.unwrap_or(false),
        selection: parsed.selection,
        response: response_tx,
    };
    // BC.8c: the show-document bus is now the generic `InboundBus`; `send`
    // wakes the editor (off-keystroke reply) — same `Result<(), payload>`
    // shape as the retired `ShowDocumentBus::dispatch`.
    if bus.send(inbound).is_err() {
        return show_document_response(req_id, false);
    }
    match response_rx.await {
        Ok(outcome) => show_document_response(req_id, outcome.success),
        Err(_) => show_document_response(req_id, false),
    }
}

fn show_document_response(req_id: RequestId, success: bool) -> Response {
    let body = lsp_types::ShowDocumentResult { success };
    match serde_json::to_value(body) {
        Ok(v) => Response::ok(req_id, v),
        Err(e) => Response::err(
            req_id,
            crate::jsonrpc::ResponseError {
                code: crate::jsonrpc::error_codes::INTERNAL_ERROR,
                message: format!("encode response: {e}"),
                data: None,
            },
        ),
    }
}

/// 4.4.b: `window/showMessageRequest` handler. Same shape as
/// applyEdit + showDocument: parse, dispatch, await, ferry.
/// The reply body is either the selected `MessageActionItem`
/// (verbatim) or JSON `null` when the user dismissed without
/// picking.
async fn handle_show_message_request(
    instance: crate::logging::InstanceKey,
    req_id: RequestId,
    params: Option<Value>,
    bus: &crate::show_message_request::ShowMessageRequestBus,
    logger: &LspLogger,
) -> Response {
    let server_id = Arc::clone(&instance.server_id);
    let parsed: lsp_types::ShowMessageRequestParams = match params {
        Some(v) => match serde_json::from_value(v) {
            Ok(p) => p,
            Err(e) => {
                logger.log(
                    Some(&instance),
                    LogLevel::Warn,
                    LogSource::Client,
                    format!("window/showMessageRequest: malformed params: {e}"),
                );
                return Response::ok(req_id, Value::Null);
            }
        },
        None => return Response::ok(req_id, Value::Null),
    };
    let (response_tx, response_rx) = oneshot::channel();
    let inbound = crate::show_message_request::InboundShowMessageRequest {
        server_id: Arc::clone(&server_id),
        workspace: Arc::clone(&instance.workspace),
        level: parsed.typ,
        message: parsed.message,
        actions: parsed.actions.unwrap_or_default(),
        response: response_tx,
    };
    // BC.8e: the show-message-request bus is now the generic `InboundBus`
    // (host-drained variant); `send` wakes the editor so the picker is raised
    // off-keystroke — same `Result<(), payload>` shape as the retired
    // `ShowMessageRequestBus::dispatch`.
    if bus.send(inbound).is_err() {
        return Response::ok(req_id, Value::Null);
    }
    let selected = match response_rx.await {
        Ok(outcome) => outcome.selected,
        Err(_) => None,
    };
    match selected {
        Some(item) => match serde_json::to_value(item) {
            Ok(v) => Response::ok(req_id, v),
            Err(e) => Response::err(
                req_id,
                crate::jsonrpc::ResponseError {
                    code: crate::jsonrpc::error_codes::INTERNAL_ERROR,
                    message: format!("encode response: {e}"),
                    data: None,
                },
            ),
        },
        None => Response::ok(req_id, Value::Null),
    }
}

/// Handle a server-initiated request. Default behaviour is "we
/// don't implement that yet" (METHOD_NOT_FOUND); per-method
/// handlers replace this as features land.
fn handle_server_request(
    instance: &crate::logging::InstanceKey,
    req: &Request,
    logger: &LspLogger,
) -> Response {
    match req.method.as_str() {
        // `client/registerCapability` / `client/unregisterCapability`
        // are handled inline in `actor_main` (4.4.n) so they can
        // mutate the published capability snapshot. Reaching this
        // arm means the inline handler is missing a branch --
        // log it so we notice in CI.
        "client/registerCapability" | "client/unregisterCapability" => {
            logger.log(
                Some(instance),
                LogLevel::Warn,
                LogSource::Client,
                format!(
                    "{} reached the inline-fallback handler -- registration dropped (bug)",
                    req.method
                ),
            );
            Response::ok(req.id.clone(), Value::Null)
        }
        // Empty configuration -- the §5.12 typed-options layer
        // wires real values in later.
        "workspace/configuration" => {
            // params: {items: [{section: "...", scopeUri: "..."}, ...]}
            // Response: array with one entry per requested item.
            let n_items = req
                .params
                .as_ref()
                .and_then(|v| v.get("items"))
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            let arr: Vec<Value> = (0..n_items).map(|_| Value::Null).collect();
            Response::ok(req.id.clone(), Value::Array(arr))
        }
        "window/workDoneProgress/create" => Response::ok(req.id.clone(), Value::Null),
        other => {
            logger.log(
                Some(instance),
                LogLevel::Warn,
                LogSource::Client,
                format!("server request unhandled: {other}"),
            );
            Response::err(
                req.id.clone(),
                crate::jsonrpc::ResponseError {
                    code: crate::jsonrpc::error_codes::METHOD_NOT_FOUND,
                    message: format!("client does not implement {other}"),
                    data: None,
                },
            )
        }
    }
}

/// Pre-handshake message handler: log, ignore. The server might
/// emit `window/logMessage` or `$/progress` before responding to
/// `initialize`; spec lets it.
fn handle_pre_handshake_message(
    instance: &crate::logging::InstanceKey,
    msg: Message,
    diagnostics: &DiagnosticsBus,
    logger: &LspLogger,
    event_bus: Option<&Arc<lattice_runtime::EventBus>>,
) {
    match msg {
        Message::Notification(n) => {
            handle_server_notification(instance, &n, diagnostics, logger, event_bus)
        }
        Message::Request(r) => {
            logger.log(
                Some(instance),
                LogLevel::Warn,
                LogSource::Client,
                format!(
                    "server-initiated {} request before handshake -- ignored",
                    r.method
                ),
            );
        }
        Message::Response(r) => {
            logger.log(
                Some(instance),
                LogLevel::Warn,
                LogSource::Client,
                format!("stray response before handshake (id {:?})", r.id),
            );
        }
    }
}

/// Build the `initialize` params with our advertised capabilities.
fn build_initialize_params(
    workspace_folder_uri: Uri,
    workspace_name: String,
    initialization_options: Option<Value>,
) -> InitializeParams {
    let folder = WorkspaceFolder {
        uri: workspace_folder_uri.clone(),
        name: workspace_name,
    };
    #[allow(deprecated)]
    InitializeParams {
        process_id: Some(std::process::id()),
        // root_path / root_uri are deprecated but some servers
        // still read them. Set the URI for backward compat
        // (lsp-types still has the field) and leave root_path as
        // None.
        root_path: None,
        root_uri: Some(workspace_folder_uri.clone()),
        initialization_options,
        capabilities: capabilities::client_capabilities(),
        trace: Some(lsp_types::TraceValue::Off),
        workspace_folders: Some(vec![folder]),
        client_info: Some(ClientInfo {
            name: "lattice".into(),
            version: Some(env!("CARGO_PKG_VERSION").into()),
        }),
        locale: None,
        ..Default::default()
    }
}

/// Run the LSP shutdown sequence end-to-end. Used when the
/// caller drops their handle without explicit `shutdown`.
/// `out_tx` is the channel to the `write_loop`; `child` is the
/// process so we can `wait` on its exit.
async fn perform_shutdown(
    out_tx: &mpsc::UnboundedSender<Message>,
    _in_rx: &mut mpsc::UnboundedReceiver<Message>,
    next_id: &mut u64,
    child: Option<&mut tokio::process::Child>,
) {
    let id = RequestId::from_u64(*next_id);
    *next_id += 1;
    let _ = out_tx.send(Message::Request(Request::new(id, "shutdown", None)));
    let _ = out_tx.send(Message::Notification(Notification::new("exit", None)));
    if let Some(c) = child {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), c.wait()).await;
    }
}

async fn write_loop<W>(
    writer: Arc<Mutex<LspWriter<W>>>,
    mut out_rx: mpsc::UnboundedReceiver<Message>,
    instance: crate::logging::InstanceKey,
    logger: LspLogger,
) where
    W: AsyncWrite + Unpin + Send,
{
    while let Some(msg) = out_rx.recv().await {
        // Trace interceptor: emit a Trace record before the
        // wire write iff trace mode is enabled for this
        // instance. `is_tracing` is a single HashSet lookup --
        // off path costs almost nothing.
        if logger.is_tracing(&instance) {
            logger.log(
                Some(&instance),
                LogLevel::Trace,
                LogSource::Trace,
                format!("→ {}", trace_render(&msg)),
            );
        }
        let mut w = writer.lock().await;
        if let Err(e) = w.write_message(&msg).await {
            logger.log(
                Some(&instance),
                LogLevel::Error,
                LogSource::Client,
                format!("write_loop terminating: {e}"),
            );
            break;
        }
    }
}

async fn read_loop<R>(
    mut reader: LspReader<R>,
    in_tx: mpsc::UnboundedSender<Message>,
    instance: crate::logging::InstanceKey,
    logger: LspLogger,
) where
    R: AsyncBufRead + Unpin + Send,
{
    loop {
        match reader.read_message().await {
            Ok(Some(msg)) => {
                if logger.is_tracing(&instance) {
                    logger.log(
                        Some(&instance),
                        LogLevel::Trace,
                        LogSource::Trace,
                        format!("← {}", trace_render(&msg)),
                    );
                }
                if in_tx.send(msg).is_err() {
                    // Actor task gone.
                    return;
                }
            }
            Ok(None) => {
                logger.log(
                    Some(&instance),
                    LogLevel::Info,
                    LogSource::Client,
                    "server closed stdout cleanly",
                );
                return;
            }
            Err(e) => {
                logger.log(
                    Some(&instance),
                    LogLevel::Error,
                    LogSource::Client,
                    format!("read_loop terminating: {e}"),
                );
                return;
            }
        }
    }
}

async fn stderr_drain(
    stderr: ChildStderr,
    instance: crate::logging::InstanceKey,
    logger: LspLogger,
) {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        logger.log(Some(&instance), LogLevel::Warn, LogSource::Stderr, line);
    }
}

/// Render a Message as a compact one-line trace string. We use
/// the JSON-RPC kind + (for requests/responses) the id +
/// method, plus a truncated body. Cheap; runs only when trace
/// is on.
fn trace_render(msg: &Message) -> String {
    const MAX: usize = 240;
    let mut s = match msg {
        Message::Request(r) => format!("Request id={:?} method={}", r.id, r.method),
        Message::Notification(n) => format!("Notification method={}", n.method),
        Message::Response(r) => {
            if let Some(err) = r.error.as_ref() {
                format!("Response id={:?} ERR {} {}", r.id, err.code, err.message)
            } else {
                format!("Response id={:?} OK", r.id)
            }
        }
    };
    if let Ok(body) = msg.to_json() {
        let body_str = String::from_utf8_lossy(&body);
        if body_str.len() <= MAX - s.len().min(MAX) {
            s.push_str(" body=");
            s.push_str(&body_str);
        } else {
            s.push_str(" body=");
            s.push_str(&body_str[..MAX.saturating_sub(s.len() + 6)]);
            s.push_str("...");
        }
    }
    s
}

#[cfg(test)]
mod uri_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn relative_path_promoted_to_absolute_uri() {
        // Bug: a relative path produced `file:///<path>` which the
        // server interprets as root-rooted, not as
        // "<cwd>/<path>". Result: rust-analyzer can't find the
        // file in its workspace and every hover / definition
        // returns null. Fix: `uri_from_path` calls
        // `std::path::absolute` first.
        let cwd = std::env::current_dir().expect("cwd");
        let rel = PathBuf::from("crates/lattice-core/src/buffer.rs");
        let uri = uri_from_path(&rel);
        let uri_str = uri.as_str();
        assert!(
            uri_str.starts_with("file://"),
            "uri must start with file://, got {uri_str:?}"
        );
        // The absolute prefix (cwd) must appear in the URI.
        let cwd_marker = cwd
            .to_string_lossy()
            .replace('\\', "/")
            .trim_start_matches('/')
            .to_string();
        assert!(
            uri_str.contains(&cwd_marker),
            "uri should contain absolute cwd; got {uri_str:?} cwd marker {cwd_marker:?}"
        );
        assert!(
            uri_str.ends_with("crates/lattice-core/src/buffer.rs"),
            "uri should end with the original relative path; got {uri_str:?}"
        );
    }

    #[test]
    fn absolute_path_unchanged_in_uri() {
        // Already-absolute paths should round-trip without extra
        // canonicalisation (no symlink resolution).
        let abs = PathBuf::from("/tmp/lattice-test/foo.rs");
        let uri = uri_from_path(&abs);
        assert_eq!(uri.as_str(), "file:///tmp/lattice-test/foo.rs");
    }
}

#[cfg(test)]
mod progress_tests {
    use super::*;
    use crate::events::LspProgressKind;
    use serde_json::json;

    fn sid() -> Arc<str> {
        Arc::from("rust")
    }

    #[test]
    fn parses_begin_with_full_payload() {
        let params = json!({
            "token": "build-1",
            "value": {
                "kind": "begin",
                "title": "Building",
                "message": "compiling",
                "percentage": 12,
                "cancellable": true,
            }
        });
        let p = parse_progress(&sid(), Some(&params)).expect("begin parses");
        assert_eq!(&*p.server_id, "rust");
        assert_eq!(p.token, "build-1");
        assert_eq!(p.kind, LspProgressKind::Begin);
        assert_eq!(p.title.as_deref(), Some("Building"));
        assert_eq!(p.message.as_deref(), Some("compiling"));
        assert_eq!(p.percentage, Some(12));
        assert!(p.cancellable);
    }

    #[test]
    fn parses_numeric_token() {
        // Per spec the token can be number or string; we serialise
        // either form to a String so the accumulator key stays
        // uniform.
        let params = json!({
            "token": 42,
            "value": { "kind": "end" }
        });
        let p = parse_progress(&sid(), Some(&params)).expect("end parses");
        assert_eq!(p.token, "42");
        assert_eq!(p.kind, LspProgressKind::End);
    }

    #[test]
    fn rejects_unknown_kind() {
        let params = json!({
            "token": "x",
            "value": { "kind": "bogus" }
        });
        assert!(parse_progress(&sid(), Some(&params)).is_none());
    }

    #[test]
    fn rejects_missing_value() {
        let params = json!({ "token": "x" });
        assert!(parse_progress(&sid(), Some(&params)).is_none());
    }

    #[test]
    fn caps_percentage_at_100() {
        // Some servers report 0..=100, some over-report briefly;
        // we clamp so the modeline can't render `120%`.
        let params = json!({
            "token": "x",
            "value": { "kind": "report", "percentage": 150 }
        });
        let p = parse_progress(&sid(), Some(&params)).expect("report parses");
        assert_eq!(p.percentage, Some(100));
    }
}

#[cfg(test)]
mod log_trace_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_message_and_optional_verbose() {
        let params = json!({
            "message": "rpc inbound",
            "verbose": "{\"jsonrpc\":\"2.0\",\"id\":1}",
        });
        let (msg, verbose) = parse_log_trace(Some(&params));
        assert_eq!(msg, "rpc inbound");
        assert_eq!(verbose.as_deref(), Some("{\"jsonrpc\":\"2.0\",\"id\":1}"));
    }

    #[test]
    fn message_required_verbose_optional() {
        let params = json!({ "message": "step" });
        let (msg, verbose) = parse_log_trace(Some(&params));
        assert_eq!(msg, "step");
        assert!(verbose.is_none());
    }

    #[test]
    fn falls_back_on_missing_message() {
        // Spec says message is required; for a malformed
        // payload we use the compacted JSON as the message so
        // the trace log still shows something useful.
        let params = json!({ "other": 1 });
        let (msg, _) = parse_log_trace(Some(&params));
        assert!(!msg.is_empty());
    }
}
