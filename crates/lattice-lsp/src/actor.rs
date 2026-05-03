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
use crate::error::{LspError, LspResult};
use crate::jsonrpc::{Message, Notification, Request, RequestId, Response};
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
/// [`LspError`].
pub async fn spawn(
    config: ServerConfig,
    workspace_root: std::path::PathBuf,
) -> LspResult<ServerHandle> {
    let transport = ChildTransport::spawn(&config.binary, &config.args, Some(&workspace_root))
        .await
        .map_err(LspError::Transport)?;
    let (reader, writer, stderr, child) = transport.split();
    spawn_with_io(config, workspace_root, reader, writer, stderr, Some(child)).await
}

/// Spawn the actor against pre-existing `LspReader` / `LspWriter`
/// halves. Used by tests (mock server over a duplex pipe) and
/// by future embedded transports (TCP, named pipe).
///
/// `child` is `None` for in-process tests (the duplex partner is
/// a tokio task) and `Some(_)` for the real child-process path.
/// When None, the shutdown sequence skips the child-exit wait.
pub async fn spawn_with_io<R, W>(
    config: ServerConfig,
    workspace_root: std::path::PathBuf,
    reader: LspReader<R>,
    writer: LspWriter<W>,
    stderr: Option<ChildStderr>,
    child: Option<Child>,
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
    ));

    let capabilities = handshake_rx
        .await
        .map_err(|_| LspError::HandshakeFailed("actor died before handshake".into()))??;

    Ok(ServerHandle {
        inner: Arc::new(HandleInner {
            cmd_tx,
            capabilities,
            server_id,
        }),
    })
}

/// Convert a filesystem path to a `file://` URI in the form LSP
/// expects. lsp-types 0.97 dropped the `url` crate; we
/// percent-encode manually for the small set of bytes that
/// matter in a path (space → `%20`, etc.). Servers in practice
/// tolerate plain `file:///<path>` without aggressive encoding.
pub(crate) fn uri_from_path(p: &std::path::Path) -> Uri {
    let display = p.to_string_lossy();
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
) where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    // Drain stderr in the background -- one line per tracing::warn.
    if let Some(stderr) = stderr {
        let id = server_id.clone();
        tokio::spawn(stderr_drain(stderr, id));
    }

    // Spawn write_loop. Mutex around the writer because
    // `write_loop` also writes the initialize request (via the
    // outbound channel) before the loop starts processing
    // mailbox commands. After spawn, the actor only writes
    // through the channel.
    let writer = Arc::new(Mutex::new(writer));
    let (out_tx, out_rx) = mpsc::unbounded_channel::<Message>();
    tokio::spawn(write_loop(Arc::clone(&writer), out_rx, server_id.clone()));

    // Spawn read_loop.
    let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Message>();
    tokio::spawn(read_loop(reader, in_tx, server_id.clone()));

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
                handle_pre_handshake_message(&server_id, other);
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
                        handle_server_notification(&server_id, &n);
                    }
                    Some(Message::Request(req)) => {
                        // Server-initiated request. Reply with a
                        // structured no-op for now; per-method
                        // handlers (workspace/configuration,
                        // workspace/applyEdit, ...) land in 4.1.d
                        // and 4.3.
                        let resp = handle_server_request(&server_id, &req);
                        let _ = out_tx.send(Message::Response(resp));
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

/// Handle a server-initiated notification. 4.1 logs everything;
/// 4.1.d wires `publishDiagnostics` into the decoration layer
/// and 4.4+ adds progress / log routing into a `:messages`
/// buffer.
fn handle_server_notification(server_id: &str, n: &Notification) {
    match n.method.as_str() {
        "window/logMessage" => {
            tracing::info!(server_id, params = ?n.params, "server log");
        }
        "window/showMessage" => {
            tracing::info!(server_id, params = ?n.params, "server show-message");
        }
        "$/progress" => {
            tracing::debug!(server_id, params = ?n.params, "server progress");
        }
        "telemetry/event" => {
            tracing::debug!(server_id, params = ?n.params, "server telemetry");
        }
        "textDocument/publishDiagnostics" => {
            // Routed to the editor in 4.1.d.
            tracing::debug!(server_id, "diagnostics (handler in 4.1.d)");
        }
        other => {
            tracing::debug!(server_id, method = other, "unhandled server notification");
        }
    }
}

/// Handle a server-initiated request. Default behaviour is "we
/// don't implement that yet" (METHOD_NOT_FOUND); per-method
/// handlers replace this as features land.
fn handle_server_request(server_id: &str, req: &Request) -> Response {
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
            tracing::warn!(server_id, method = other, "server request unhandled");
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
fn handle_pre_handshake_message(server_id: &str, msg: Message) {
    match msg {
        Message::Notification(n) => handle_server_notification(server_id, &n),
        Message::Request(_) => {
            tracing::warn!(
                server_id,
                "server-initiated request before handshake -- ignored"
            );
        }
        Message::Response(r) => {
            tracing::warn!(
                server_id,
                ?r.id,
                "stray response before handshake -- ignored"
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
    server_id: String,
) where
    W: AsyncWrite + Unpin + Send,
{
    while let Some(msg) = out_rx.recv().await {
        let mut w = writer.lock().await;
        if let Err(e) = w.write_message(&msg).await {
            tracing::error!(server_id, error = %e, "write_loop terminating");
            break;
        }
    }
}

async fn read_loop<R>(
    mut reader: LspReader<R>,
    in_tx: mpsc::UnboundedSender<Message>,
    server_id: String,
) where
    R: AsyncBufRead + Unpin + Send,
{
    loop {
        match reader.read_message().await {
            Ok(Some(msg)) => {
                if in_tx.send(msg).is_err() {
                    // Actor task gone.
                    return;
                }
            }
            Ok(None) => {
                tracing::info!(server_id, "server closed stdout cleanly");
                return;
            }
            Err(e) => {
                tracing::error!(server_id, error = %e, "read_loop terminating");
                return;
            }
        }
    }
}

async fn stderr_drain(stderr: ChildStderr, server_id: String) {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::warn!(server_id, msg = %line, "server stderr");
    }
}
