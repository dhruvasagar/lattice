//! The IDE server supervisor: an idle tokio task that starts/stops the
//! loopback WebSocket listener on command.
//!
//! Mirrors `lattice_lsp`'s supervisor shape: a single task owns the
//! listener lifecycle; a clone-able [`ClaudeCodeServerHandle`] drives it
//! via a non-blocking `cmd_tx` send. The ex-command `apply` closures
//! (`:claude-code-start` / `:claude-code-stop`) and `claude-code-mode`'s
//! `on_activate` (I5) hold the handle and call `start` / `stop`.
//!
//! All protocol I/O happens off the editor thread: the supervisor task
//! and one task per connection run on the IDE runtime. The editor thread
//! only ever calls `start` / `stop` (a channel send) and reads the
//! wait-free [`ServerState`] snapshot.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use lattice_runtime::EventBus;

use crate::dispatch::{self, DispatchContext, Outgoing};
use crate::inbound::{self, ClaudeCodeInboundBus};
use crate::error::Result;
use crate::lockfile::{Lockfile, LockfileContents};
use crate::reads::ReadContext;
use crate::snapshot::ReadStateHandle;
use crate::{auth, transport};

/// IDE name reported in the discovery lockfile.
const IDE_NAME: &str = "Lattice";

/// Static config the server binds with.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Absolute paths advertised as workspace folders.
    pub workspace_folders: Vec<String>,
    /// Directory the discovery lockfile is written into (`~/.claude/ide`
    /// in production; a temp dir in tests).
    pub lock_dir: PathBuf,
}

/// Wait-free snapshot of server state for status reads (headerline, I7).
#[derive(Debug, Clone, Default)]
pub struct ServerState {
    /// Whether the listener is currently bound + accepting.
    pub running: bool,
    /// The bound loopback port, when running.
    pub port: Option<u16>,
}

/// Command to the supervisor task.
enum ServerCmd {
    Start,
    Stop,
}

/// Handle to the supervisor. Clone-able + `Send + Sync`: the ex-command
/// `apply` closures and `claude-code-mode`'s `on_activate` hold one and
/// drive `start` / `stop`. Mirrors `lattice_lsp::LspSupervisorHandle`.
#[derive(Clone)]
pub struct ClaudeCodeServerHandle {
    cmd_tx: mpsc::UnboundedSender<ServerCmd>,
    state: Arc<ArcSwap<ServerState>>,
    /// The crate-owned read cache (subscription set up at spawn). Shared
    /// into the [`DispatchContext`] built by `install_read_services`.
    cache: ReadStateHandle,
    /// Workspace folders from the config (for `getWorkspaceFolders`).
    workspace_folders: Vec<String>,
    /// The live dispatch context connections read. Starts with no generic
    /// services (cache + config only); `install_services` upgrades it once
    /// boot has wired the buffer-store / diagnostics handles + the write bus.
    dispatch_ctx: Arc<ArcSwap<DispatchContext>>,
    /// I3.2: holds the write-drain's tick-callback registration alive for the
    /// server's lifetime (dropping it would unregister the drain). Behind a
    /// shared cell so the handle stays `Clone`.
    write_reg: Arc<Mutex<Option<lattice_mode::TickCallbackRegistration>>>,
}

impl ClaudeCodeServerHandle {
    /// Request the server start (bind + lockfile + accept). Idempotent and
    /// non-blocking: one `cmd_tx` send applied on the supervisor task. A
    /// dropped supervisor makes this a no-op.
    pub fn start(&self) {
        let _ = self.cmd_tx.send(ServerCmd::Start);
    }

    /// Request the server stop (unbind + unlink lockfile + drop conns).
    /// Idempotent, non-blocking.
    pub fn stop(&self) {
        let _ = self.cmd_tx.send(ServerCmd::Stop);
    }

    /// Current state snapshot (wait-free `Arc` load).
    pub fn snapshot(&self) -> Arc<ServerState> {
        self.state.load_full()
    }

    /// I2.2 + I3.2: install the generic read + write services once boot has
    /// created them (they're built later than the server handle — see the
    /// boot-ordering note + boot-composition.md). Rebuilds the dispatch context
    /// with the cache + config + read handles + the write bus, and registers
    /// the write drain with the tick-callback registry. Connections read the
    /// upgraded context wait-free.
    pub fn install_services(
        &self,
        buffer_store: Option<lattice_mode::BufferStoreHandle>,
        diagnostics: Option<lattice_lsp::modes::DiagnosticsQueryHandle>,
        tick_callbacks: lattice_mode::TickCallbackRegistryHandle,
        async_landed: Arc<Notify>,
    ) {
        // I3.2: create the inbound write bus (its `send` wakes `async_landed`,
        // so writes apply off-keystroke) + register its per-tick drain with the
        // tick-callback registry. The registration is held in `write_reg` for
        // the server's lifetime.
        let (bus, rx) = ClaudeCodeInboundBus::new(async_landed);
        let drain = inbound::make_drain(rx, self.cache.clone());
        let reg = tick_callbacks.register(Box::new(drain));
        *self.write_reg.lock().unwrap_or_else(|e| e.into_inner()) = Some(reg);

        self.dispatch_ctx.store(Arc::new(DispatchContext {
            reads: ReadContext {
                cache: self.cache.clone(),
                buffer_store,
                diagnostics,
                workspace_folders: self.workspace_folders.clone(),
            },
            writes: Some(bus),
        }));
    }
}

/// Spawn the supervisor task on `rt`. Returns immediately; the task stays
/// idle until `start()`.
pub fn spawn(
    config: ServerConfig,
    event_bus: Arc<EventBus>,
    rt: &tokio::runtime::Handle,
) -> ClaudeCodeServerHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let state = Arc::new(ArcSwap::from_pointee(ServerState::default()));
    // I2.1: the read cache subscribes to the generic event bus here.
    let cache = crate::snapshot::spawn_read_cache(&event_bus, rt);
    // I2.2: start with a deps-less dispatch context (cache + config only);
    // `install_read_services` upgrades it once boot wires the generic
    // buffer-store / diagnostics handles.
    let dispatch_ctx = Arc::new(ArcSwap::from_pointee(DispatchContext {
        reads: ReadContext {
            cache: cache.clone(),
            buffer_store: None,
            diagnostics: None,
            workspace_folders: config.workspace_folders.clone(),
        },
        // I3.2 wires the real inbound bus here; until then write tools report
        // a graceful "not initialized".
        writes: None,
    }));
    rt.spawn(supervisor_main(
        config.clone(),
        cmd_rx,
        state.clone(),
        dispatch_ctx.clone(),
    ));
    ClaudeCodeServerHandle {
        cmd_tx,
        state,
        cache,
        workspace_folders: config.workspace_folders,
        dispatch_ctx,
        write_reg: Arc::new(Mutex::new(None)),
    }
}

/// A bound listener + its lockfile. Dropping aborts the accept loop and
/// (via the lockfile's `Drop`) unlinks the discovery file.
struct RunningServer {
    _lockfile: Lockfile,
    accept_task: JoinHandle<()>,
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

async fn supervisor_main(
    config: ServerConfig,
    mut cmd_rx: mpsc::UnboundedReceiver<ServerCmd>,
    state: Arc<ArcSwap<ServerState>>,
    dispatch_ctx: Arc<ArcSwap<DispatchContext>>,
) {
    let mut running: Option<RunningServer> = None;
    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            ServerCmd::Start => {
                if running.is_some() {
                    continue; // idempotent restart
                }
                match start_listener(&config, dispatch_ctx.clone()).await {
                    Ok((server, port)) => {
                        state.store(Arc::new(ServerState {
                            running: true,
                            port: Some(port),
                        }));
                        running = Some(server);
                        tracing::info!(port, "claude-code IDE server started");
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "claude-code IDE server failed to start");
                    }
                }
            }
            ServerCmd::Stop => {
                if running.take().is_some() {
                    // RunningServer::drop aborts accept + unlinks lockfile.
                    state.store(Arc::new(ServerState::default()));
                    tracing::info!("claude-code IDE server stopped");
                }
            }
        }
    }
}

async fn start_listener(
    config: &ServerConfig,
    dispatch_ctx: Arc<ArcSwap<DispatchContext>>,
) -> Result<(RunningServer, u16)> {
    // Bind an ephemeral loopback port. Linux ephemeral ports fall inside
    // the dynamic 10000-65535 range the contract specifies; explicit
    // range selection is a refinement, not needed for I1.
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let token = auth::generate_token()?;
    let lockfile = Lockfile::write(
        &config.lock_dir,
        port,
        &LockfileContents {
            pid: std::process::id(),
            workspace_folders: config.workspace_folders.clone(),
            ide_name: IDE_NAME.to_string(),
            transport: "ws".to_string(),
            auth_token: token.clone(),
            running_in_windows: false,
        },
    )?;
    let accept_task = tokio::spawn(accept_loop(listener, token, dispatch_ctx));
    Ok((
        RunningServer {
            _lockfile: lockfile,
            accept_task,
        },
        port,
    ))
}

async fn accept_loop(
    listener: TcpListener,
    token: String,
    dispatch_ctx: Arc<ArcSwap<DispatchContext>>,
) {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let token = token.clone();
                let ctx = dispatch_ctx.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_connection(stream, token, ctx).await {
                        tracing::debug!(error = %e, "claude-code connection ended with error");
                    }
                });
            }
            Err(e) => {
                tracing::debug!(error = %e, "claude-code accept error");
                // Avoid a busy-spin if accept keeps failing.
                tokio::task::yield_now().await;
            }
        }
    }
}

async fn serve_connection(
    stream: TcpStream,
    token: String,
    dispatch_ctx: Arc<ArcSwap<DispatchContext>>,
) -> Result<()> {
    let ws = transport::accept(stream, &token).await?;
    let (mut write, mut read) = ws.split();
    // Load the current dispatch context once per connection (installed at
    // boot before any start, so it carries the generic read services).
    let ctx = dispatch_ctx.load_full();
    while let Some(frame) = read.next().await {
        let frame = frame?;
        if frame.is_close() {
            break;
        }
        // MCP frames are JSON text; ignore binary / ping / pong (pings are
        // auto-ponged by the stream's read machinery).
        let Ok(text) = frame.to_text() else {
            continue;
        };
        for outgoing in dispatch::dispatch_frame(text.as_bytes(), &ctx).await {
            let payload = match &outgoing {
                Outgoing::Response(r) => serde_json::to_string(r)?,
                Outgoing::Notification(n) => serde_json::to_string(n)?,
            };
            write.send(WsMessage::Text(payload)).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[tokio::test]
    async fn install_services_registers_write_drain_and_installs_bus() {
        let config = ServerConfig {
            workspace_folders: vec![],
            lock_dir: std::env::temp_dir(),
        };
        let handle = spawn(
            config,
            Arc::new(EventBus::new()),
            &tokio::runtime::Handle::current(),
        );
        let tick: lattice_mode::TickCallbackRegistryHandle =
            Arc::new(lattice_mode::TickCallbackRegistry::new());
        handle.install_services(None, None, tick.clone(), Arc::new(Notify::new()));
        assert_eq!(tick.registered_count(), 1, "the write drain is registered");
        assert!(
            handle.dispatch_ctx.load().writes.is_some(),
            "the write bus is installed"
        );
    }
}
