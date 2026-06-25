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
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use lattice_diff::ProgrammaticDiffBus;
use lattice_mode::inbound::InboundBus;
use lattice_runtime::EventBus;

use crate::dispatch::{self, DispatchContext, Outgoing};
use crate::inbound::ClaudeCodeInboundRequest;
use crate::error::Result;
use crate::lockfile::{Lockfile, LockfileContents};
use crate::reads::ReadContext;
use crate::snapshot::ReadStateHandle;
use crate::{auth, transport};

/// IDE name reported in the discovery lockfile.
const IDE_NAME: &str = "Lattice";

/// I6: capacity of the server-initiated notification broadcast. A connection
/// that falls this far behind a burst skips the dropped frames (acceptable —
/// selection notifications coalesce to "latest wins").
const NOTIFY_CAPACITY: usize = 64;

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
    /// I5.1: the pre-bound listener (bound synchronously in [`start`] so the
    /// caller learns the port immediately) + the auth token. The supervisor
    /// writes the discovery lockfile, wraps the listener for tokio, and runs
    /// the accept loop.
    ///
    /// [`start`]: ClaudeCodeServerHandle::start
    Start {
        listener: std::net::TcpListener,
        token: String,
    },
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
    /// I6: broadcast channel for server-initiated notification frames. The
    /// notification task (`notifications.rs`) + `:claude-send` publish frames
    /// here; each connection subscribes a receiver and forwards frames to its
    /// WS writer. A dropped/lagged receiver is pruned by the channel itself —
    /// no manual connection registry.
    notify_tx: broadcast::Sender<String>,
}

impl ClaudeCodeServerHandle {
    /// Start the server and return the bound loopback **port**, or `None` on
    /// failure. Idempotent: a second call while already running returns the
    /// existing port without re-binding.
    ///
    /// I5.1: the listener is pre-bound *synchronously here* (not async on the
    /// supervisor) so `:claude` learns the port immediately and can inject
    /// `CLAUDE_CODE_SSE_PORT` into the agent's environment before spawning it.
    /// The bind + the supervisor's subsequent lockfile write are one-shot (a
    /// user command, never the render loop), so the brief sync work is fine.
    pub fn start(&self) -> Option<u16> {
        let current = self.state.load();
        if current.running {
            return current.port; // idempotent — already bound
        }
        let listener = match std::net::TcpListener::bind(("127.0.0.1", 0)) {
            Ok(l) => l,
            Err(e) => {
                tracing::debug!(error = %e, "claude-code: pre-bind failed");
                return None;
            }
        };
        let port = listener.local_addr().ok()?.port();
        if let Err(e) = listener.set_nonblocking(true) {
            tracing::debug!(error = %e, "claude-code: set_nonblocking failed");
            return None;
        }
        let token = match auth::generate_token() {
            Ok(t) => t,
            Err(e) => {
                tracing::debug!(error = %e, "claude-code: token generation failed");
                return None;
            }
        };
        // Optimistic running state so a re-entrant `start()` doesn't double-bind;
        // the supervisor rolls it back if it can't take the listener over.
        self.state.store(Arc::new(ServerState {
            running: true,
            port: Some(port),
        }));
        if self
            .cmd_tx
            .send(ServerCmd::Start { listener, token })
            .is_err()
        {
            // Supervisor gone — roll the optimistic state back.
            self.state.store(Arc::new(ServerState::default()));
            return None;
        }
        Some(port)
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

    /// I6: broadcast a server-initiated notification frame to every connected
    /// agent. A no-op (the frame is dropped) when no connections are
    /// subscribed. Used by `:claude-send`; the notification task uses its own
    /// [`Self::notify_sender`] clone.
    pub fn notify(&self, frame: String) {
        let _ = self.notify_tx.send(frame);
    }

    /// I6.1: a clone of the broadcast sender for the notification task to
    /// publish `selection_changed` / `didChangeActiveEditor` frames through.
    pub fn notify_sender(&self) -> broadcast::Sender<String> {
        self.notify_tx.clone()
    }

    /// I7: number of currently-connected agents (subscribed broadcast
    /// receivers). Surfaced in the `claude-code-mode` status.
    pub fn connection_count(&self) -> usize {
        self.notify_tx.receiver_count()
    }

    /// BC.3b: the crate-owned read cache. `install()` clones it to build the
    /// inbound handler ([`crate::inbound::make_handler`]) — the per-item closure
    /// the generic `boot.inbound` bus drains, which maps write requests against
    /// the same cache the read tools snapshot.
    pub fn read_cache(&self) -> ReadStateHandle {
        self.cache.clone()
    }

    /// I2.2 + I3.2 / BC.3b: seat the generic read handles + the write bus into
    /// the live dispatch context (connections read it wait-free). `writes` is
    /// the generic [`InboundBus`] built by `install()` via `boot.inbound` (which
    /// owns the channel, the per-tick drain, and the off-keystroke wake — the
    /// drain's registration token rides `boot.into_registrations()` into the
    /// Editor). The server handle is spawned before boot wires these handles, so
    /// this upgrade runs once, from the subsystem's `install(boot)`.
    pub fn install_services(
        &self,
        buffer_store: Option<lattice_mode::BufferStoreHandle>,
        diagnostics: Option<lattice_lsp::modes::DiagnosticsQueryHandle>,
        writes: InboundBus<ClaudeCodeInboundRequest>,
        diff: Option<ProgrammaticDiffBus>,
    ) {
        self.dispatch_ctx.store(Arc::new(DispatchContext {
            reads: ReadContext {
                cache: self.cache.clone(),
                buffer_store,
                diagnostics,
                workspace_folders: self.workspace_folders.clone(),
            },
            writes: Some(writes),
            diff,
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
        // I4: the openDiff bus is wired by `install_services`; until then
        // `openDiff` reports a graceful "not initialized".
        diff: None,
    }));
    // I6: the server-initiated notification broadcast. Bounded — a lagged
    // connection skips dropped frames (coalescing is fine for selection
    // notifications, where only the latest matters).
    let (notify_tx, _) = broadcast::channel::<String>(NOTIFY_CAPACITY);
    // I6.1: the notification task — coalesces SelectionsChanged + broadcasts
    // selection_changed / didChangeActiveEditor frames. Crate-owned (reads the
    // same generic event bus + read cache the read tools use).
    crate::notifications::spawn_notifier(&event_bus, notify_tx.clone(), cache.clone(), rt);
    rt.spawn(supervisor_main(
        config.clone(),
        cmd_rx,
        state.clone(),
        dispatch_ctx.clone(),
        notify_tx.clone(),
    ));
    ClaudeCodeServerHandle {
        cmd_tx,
        state,
        cache,
        workspace_folders: config.workspace_folders,
        dispatch_ctx,
        notify_tx,
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
    notify_tx: broadcast::Sender<String>,
) {
    let mut running: Option<RunningServer> = None;
    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            ServerCmd::Start { listener, token } => {
                if running.is_some() {
                    continue; // idempotent (start() also guards via state)
                }
                match start_accepting(
                    &config,
                    listener,
                    token,
                    dispatch_ctx.clone(),
                    notify_tx.clone(),
                ) {
                    Ok(server) => {
                        // `start()` already published running+port optimistically.
                        running = Some(server);
                        tracing::info!("claude-code IDE server accepting");
                    }
                    Err(e) => {
                        // Roll back the optimistic running state set by `start()`.
                        state.store(Arc::new(ServerState::default()));
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

/// I5.1: take over the pre-bound listener — write the discovery lockfile, wrap
/// the listener for tokio, and spawn the accept loop. Runs on the supervisor
/// task (inside the IDE runtime), so `from_std` + `tokio::spawn` have a runtime
/// context. The std listener was already set non-blocking in [`start`].
///
/// [`start`]: ClaudeCodeServerHandle::start
fn start_accepting(
    config: &ServerConfig,
    std_listener: std::net::TcpListener,
    token: String,
    dispatch_ctx: Arc<ArcSwap<DispatchContext>>,
    notify_tx: broadcast::Sender<String>,
) -> Result<RunningServer> {
    let port = std_listener.local_addr()?.port();
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
    let listener = TcpListener::from_std(std_listener)?;
    let accept_task = tokio::spawn(accept_loop(listener, token, dispatch_ctx, notify_tx));
    Ok(RunningServer {
        _lockfile: lockfile,
        accept_task,
    })
}

async fn accept_loop(
    listener: TcpListener,
    token: String,
    dispatch_ctx: Arc<ArcSwap<DispatchContext>>,
    notify_tx: broadcast::Sender<String>,
) {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let token = token.clone();
                let ctx = dispatch_ctx.clone();
                // I6: each connection subscribes its own broadcast receiver;
                // `receiver_count()` on the sender is the live connection count.
                let notify_rx = notify_tx.subscribe();
                tokio::spawn(async move {
                    if let Err(e) = serve_connection(stream, token, ctx, notify_rx).await {
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
    mut notify_rx: broadcast::Receiver<String>,
) -> Result<()> {
    let ws = transport::accept(stream, &token).await?;
    let (mut write, mut read) = ws.split();
    // Load the current dispatch context once per connection (installed at
    // boot before any start, so it carries the generic read services).
    let ctx = dispatch_ctx.load_full();

    // I6: one WS sink can't be written from two tasks, so a single outbound
    // channel funnels BOTH request responses (from the read loop) and pushed
    // server-initiated notifications (from the broadcast) to one writer task.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();

    // Writer: drain the outbound channel → WS. Ends when every `out_tx` clone
    // drops (read loop done + forwarder gone) or the peer write fails.
    let writer = tokio::spawn(async move {
        while let Some(payload) = out_rx.recv().await {
            if write.send(WsMessage::Text(payload)).await.is_err() {
                break; // peer gone
            }
        }
    });

    // Forwarder: broadcast → outbound. A `Lagged` receiver (fell behind a
    // burst) skips the dropped frames — coalescing is the intended behaviour
    // for selection notifications (latest wins).
    let notif_out = out_tx.clone();
    let forwarder = tokio::spawn(async move {
        loop {
            match notify_rx.recv().await {
                Ok(frame) => {
                    if notif_out.send(frame).is_err() {
                        break; // writer gone
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Read loop: handle incoming MCP frames; responses ride the same writer.
    // Capture the result so teardown runs on both the ok and error paths.
    let result: Result<()> = async {
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
                if out_tx.send(payload).is_err() {
                    return Ok(()); // writer gone — connection is finished
                }
            }
        }
        Ok(())
    }
    .await;

    // Teardown: dropping the read loop's `out_tx` + the forwarder's clone ends
    // the writer; abort the forwarder so its `notify_rx` is released promptly.
    drop(out_tx);
    forwarder.abort();
    let _ = writer.await;
    result
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[tokio::test]
    async fn install_services_seats_read_handles_and_bus() {
        use crate::inbound::make_handler;
        use lattice_mode::inbound::make_inbound;
        use tokio::sync::Notify;

        let config = ServerConfig {
            workspace_folders: vec![],
            lock_dir: std::env::temp_dir(),
        };
        let handle = spawn(
            config,
            Arc::new(EventBus::new()),
            &tokio::runtime::Handle::current(),
        );
        // The generic inbound bus, as `install()` builds it via `boot.inbound`.
        let (bus, _drain) = make_inbound::<ClaudeCodeInboundRequest, _>(
            Arc::new(Notify::new()),
            make_handler(handle.read_cache()),
        );
        handle.install_services(None, None, bus, None);
        assert!(
            handle.dispatch_ctx.load().writes.is_some(),
            "the write bus is installed"
        );
    }

    /// I5.1a: `start()` pre-binds synchronously and returns the bound port; a
    /// second call while running is idempotent (same port, no re-bind).
    #[tokio::test]
    async fn start_returns_port_and_is_idempotent() {
        let handle = spawn(
            ServerConfig {
                workspace_folders: vec![],
                lock_dir: std::env::temp_dir(),
            },
            Arc::new(EventBus::new()),
            &tokio::runtime::Handle::current(),
        );
        let p1 = handle.start().expect("start binds + returns a port");
        let p2 = handle.start().expect("idempotent re-start returns a port");
        assert_eq!(p1, p2, "second start returns the same port (no re-bind)");
        let snap = handle.snapshot();
        assert_eq!(snap.port, Some(p1), "snapshot reflects the bound port");
        assert!(snap.running, "server is running after start");
        handle.stop();
    }
}
