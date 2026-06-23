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
use std::sync::Arc;

use arc_swap::ArcSwap;
use futures::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::dispatch::{self, Outgoing};
use crate::error::Result;
use crate::lockfile::{Lockfile, LockfileContents};
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
}

/// Spawn the supervisor task on `rt`. Returns immediately; the task stays
/// idle until `start()`.
pub fn spawn(config: ServerConfig, rt: &tokio::runtime::Handle) -> ClaudeCodeServerHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let state = Arc::new(ArcSwap::from_pointee(ServerState::default()));
    rt.spawn(supervisor_main(config, cmd_rx, state.clone()));
    ClaudeCodeServerHandle { cmd_tx, state }
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
) {
    let mut running: Option<RunningServer> = None;
    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            ServerCmd::Start => {
                if running.is_some() {
                    continue; // idempotent restart
                }
                match start_listener(&config).await {
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

async fn start_listener(config: &ServerConfig) -> Result<(RunningServer, u16)> {
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
    let accept_task = tokio::spawn(accept_loop(listener, token));
    Ok((
        RunningServer {
            _lockfile: lockfile,
            accept_task,
        },
        port,
    ))
}

async fn accept_loop(listener: TcpListener, token: String) {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let token = token.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_connection(stream, token).await {
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

async fn serve_connection(stream: TcpStream, token: String) -> Result<()> {
    let ws = transport::accept(stream, &token).await?;
    let (mut write, mut read) = ws.split();
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
        for outgoing in dispatch::dispatch_frame(text.as_bytes()) {
            let payload = match &outgoing {
                Outgoing::Response(r) => serde_json::to_string(r)?,
                Outgoing::Notification(n) => serde_json::to_string(n)?,
            };
            write.send(WsMessage::Text(payload.into())).await?;
        }
    }
    Ok(())
}
