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
    /// Negotiated capabilities -- captured at handshake. Stable
    /// for the actor's lifetime; per-feature dispatch reads from
    /// here before issuing requests.
    capabilities: Arc<Capabilities>,
    /// Server id, for logs / telemetry.
    server_id: String,
    /// Diagnostics broadcast bus -- subscribers (App, plugins,
    /// future picker) receive every `publishDiagnostics` from
    /// this server.
    diagnostics: DiagnosticsBus,
    /// LSP-subsystem logger. Cloned from the App's shared
    /// logger; per-server records carry this server's id and
    /// land in the `*lsp:<server>*` ring.
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
    /// Negotiated capabilities. Stable for the actor's lifetime.
    pub fn capabilities(&self) -> Arc<Capabilities> {
        Arc::clone(&self.inner.capabilities)
    }

    /// Server's stable id (e.g. `"rust"`). Useful for logs and
    /// for the supervisor to look up `ServerConfig`.
    pub fn server_id(&self) -> &str {
        &self.inner.server_id
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
        self.inner
            .cmd_tx
            .send(cmd)
            .map_err(|_| LspError::ActorGone)
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
        self.inner
            .cmd_tx
            .send(cmd)
            .map_err(|_| LspError::ActorGone)
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
) -> LspResult<ServerHandle>
where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<ActorCmd>();
    let (handshake_tx, handshake_rx) = oneshot::channel::<LspResult<Arc<Capabilities>>>();

    let server_id = config.id.clone();
    let init_options = config.initialization_options.clone();
    let workspace_folder_uri = uri_from_path(&workspace_root);
    let workspace_name = workspace_root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "workspace".to_string());
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
        workspace_folder_uri,
        workspace_name,
        init_options,
        diagnostics.clone(),
        logger.clone(),
        apply_edit_bus,
    ));

    let capabilities = handshake_rx
        .await
        .map_err(|_| LspError::HandshakeFailed("actor died before handshake".into()))??;

    let server_id_arc: Arc<str> = Arc::from(server_id.as_str());
    logger.log(
        Some(&server_id_arc),
        LogLevel::Info,
        LogSource::Client,
        "handshake complete; server attached",
    );

    Ok(ServerHandle {
        inner: Arc::new(HandleInner {
            cmd_tx,
            capabilities,
            server_id,
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
    handshake_tx: oneshot::Sender<LspResult<Arc<Capabilities>>>,
    server_id: String,
    workspace_folder_uri: Uri,
    workspace_name: String,
    init_options: Option<Value>,
    diagnostics: DiagnosticsBus,
    logger: LspLogger,
    apply_edit_bus: Option<crate::apply_edit::ApplyEditBus>,
) where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let server_id_arc: Arc<str> = Arc::from(server_id.as_str());
    // Drain stderr through the logger -- each line lands in the
    // `*lsp:<server>*` ring at Warn (server stderr is the
    // canonical "something's up" signal).
    if let Some(stderr) = stderr {
        tokio::spawn(stderr_drain(stderr, Arc::clone(&server_id_arc), logger.clone()));
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
        Arc::clone(&server_id_arc),
        logger.clone(),
    ));

    // Spawn read_loop.
    let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Message>();
    tokio::spawn(read_loop(
        reader,
        in_tx,
        Arc::clone(&server_id_arc),
        logger.clone(),
    ));

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
                handle_pre_handshake_message(&server_id_arc, other, &diagnostics, &logger);
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

    // Send `initialized` notification per LSP base spec -- the
    // server is required to wait for this before processing
    // other requests.
    let initialized_value = serde_json::to_value(InitializedParams {}).unwrap_or(Value::Null);
    let _ = out_tx.send(Message::Notification(Notification::new(
        "initialized",
        Some(initialized_value),
    )));

    // Hand the handle back to the caller.
    if handshake_tx.send(Ok(Arc::clone(&caps))).is_err() {
        // The caller dropped the spawn future -- nothing else to
        // do. Run shutdown locally.
        perform_shutdown(&out_tx, &mut in_rx, &mut next_id, child.as_mut()).await;
        return;
    }

    // Main loop.
    let mut pending: HashMap<RequestId, oneshot::Sender<LspResult<Value>>> = HashMap::new();
    let mut shutting_down: Option<oneshot::Sender<LspResult<()>>> = None;

    'main: loop {
        select! {
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
                        handle_server_notification(&server_id_arc, &n, &diagnostics, &logger);
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
                            let server_id_clone = Arc::clone(&server_id_arc);
                            let logger_clone = logger.clone();
                            let out_tx_clone = out_tx.clone();
                            let req_id = req.id.clone();
                            let params = req.params.clone();
                            tokio::spawn(async move {
                                let resp = handle_apply_edit_request(
                                    server_id_clone,
                                    req_id,
                                    params,
                                    &bus,
                                    &logger_clone,
                                )
                                .await;
                                let _ = out_tx_clone.send(Message::Response(resp));
                            });
                        } else {
                            let resp = handle_server_request(&server_id_arc, &req, &logger);
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
    if let Some(reply) = shutting_down {
        let _ = reply.send(Ok(()));
    }
}

/// Handle a server-initiated notification. `publishDiagnostics`
/// fans out to the [`DiagnosticsBus`]; log / show / progress
/// notifications land in the per-server log ring.
fn handle_server_notification(
    server_id: &Arc<str>,
    n: &Notification,
    diagnostics: &DiagnosticsBus,
    logger: &LspLogger,
) {
    match n.method.as_str() {
        "window/logMessage" => {
            // LSP severity: 1=Error, 2=Warning, 3=Info, 4=Log/Debug.
            let (level, msg) = parse_window_message(&n.params);
            logger.log(Some(server_id), level, LogSource::LspMessage, msg);
        }
        "window/showMessage" => {
            let (level, msg) = parse_window_message(&n.params);
            logger.log(Some(server_id), level, LogSource::LspShowMessage, msg);
        }
        "$/progress" => {
            // Progress events are debug-level chatter until the
            // 4.4 progress slot lands.
            logger.log(
                Some(server_id),
                LogLevel::Debug,
                LogSource::Client,
                format!("$/progress: {}", compact_params(&n.params)),
            );
        }
        "telemetry/event" => {
            logger.log(
                Some(server_id),
                LogLevel::Debug,
                LogSource::Client,
                format!("telemetry/event: {}", compact_params(&n.params)),
            );
        }
        "textDocument/publishDiagnostics" => {
            let params = match n.params.clone() {
                Some(v) => v,
                None => {
                    logger.log(
                        Some(server_id),
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
                        Some(server_id),
                        LogLevel::Debug,
                        LogSource::Client,
                        format!("publishDiagnostics: {n_diags} diag(s) for {uri}"),
                    );
                }
                Err(e) => {
                    logger.log(
                        Some(server_id),
                        LogLevel::Warn,
                        LogSource::Client,
                        format!("publishDiagnostics deserialise failed: {e}"),
                    );
                }
            }
        }
        other => {
            logger.log(
                Some(server_id),
                LogLevel::Debug,
                LogSource::Client,
                format!("unhandled server notification: {other}"),
            );
        }
    }
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
    server_id: Arc<str>,
    req_id: RequestId,
    params: Option<Value>,
    bus: &crate::apply_edit::ApplyEditBus,
    logger: &LspLogger,
) -> Response {
    let parsed: lsp_types::ApplyWorkspaceEditParams = match params {
        Some(v) => match serde_json::from_value(v) {
            Ok(p) => p,
            Err(e) => {
                logger.log(
                    Some(&server_id),
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
        label: parsed.label,
        edit: parsed.edit,
        response: response_tx,
    };
    if bus.dispatch(inbound).is_err() {
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

/// Handle a server-initiated request. Default behaviour is "we
/// don't implement that yet" (METHOD_NOT_FOUND); per-method
/// handlers replace this as features land.
fn handle_server_request(server_id: &Arc<str>, req: &Request, logger: &LspLogger) -> Response {
    match req.method.as_str() {
        // Accept dynamic registration so servers don't fail at
        // startup. We don't actually act on the registration --
        // 4.4 wires the registry properly.
        "client/registerCapability" | "client/unregisterCapability" => {
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
                Some(server_id),
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
    server_id: &Arc<str>,
    msg: Message,
    diagnostics: &DiagnosticsBus,
    logger: &LspLogger,
) {
    match msg {
        Message::Notification(n) => {
            handle_server_notification(server_id, &n, diagnostics, logger)
        }
        Message::Request(r) => {
            logger.log(
                Some(server_id),
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
                Some(server_id),
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
    server_id: Arc<str>,
    logger: LspLogger,
) where
    W: AsyncWrite + Unpin + Send,
{
    while let Some(msg) = out_rx.recv().await {
        // Trace interceptor: emit a Trace record before the
        // wire write iff trace mode is enabled for this
        // server. `is_tracing` is a single HashSet lookup --
        // off path costs almost nothing.
        if logger.is_tracing(&server_id) {
            logger.log(
                Some(&server_id),
                LogLevel::Trace,
                LogSource::Trace,
                format!("→ {}", trace_render(&msg)),
            );
        }
        let mut w = writer.lock().await;
        if let Err(e) = w.write_message(&msg).await {
            logger.log(
                Some(&server_id),
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
    server_id: Arc<str>,
    logger: LspLogger,
) where
    R: AsyncBufRead + Unpin + Send,
{
    loop {
        match reader.read_message().await {
            Ok(Some(msg)) => {
                if logger.is_tracing(&server_id) {
                    logger.log(
                        Some(&server_id),
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
                    Some(&server_id),
                    LogLevel::Info,
                    LogSource::Client,
                    "server closed stdout cleanly",
                );
                return;
            }
            Err(e) => {
                logger.log(
                    Some(&server_id),
                    LogLevel::Error,
                    LogSource::Client,
                    format!("read_loop terminating: {e}"),
                );
                return;
            }
        }
    }
}

async fn stderr_drain(stderr: ChildStderr, server_id: Arc<str>, logger: LspLogger) {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        logger.log(Some(&server_id), LogLevel::Warn, LogSource::Stderr, line);
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
