//! AT.14: the loopback listener the `lattice --mcp` bridge connects to, and
//! the lock file it is found by.
//!
//! **Opt-in.** Nothing listens until `llm.serve_tools` is on; turning it off
//! closes the listener, every connection, and the lock file.
//!
//! **Security boundary: loopback plus a token.** The listener binds
//! `127.0.0.1` on a port the system picks, and writes the port and a fresh
//! token to `<config-home>/lattice/cache/mcp/<pid>-<port>.json`, readable by
//! the owner only, in a directory only the owner can list. A connection
//! sends the token as its first line; anything else is closed before a byte
//! of MCP is read. This is the IDE server's boundary (`auth.rs`), with the
//! token on a line instead of in a WebSocket header, since the bridge relays
//! bytes and speaks no HTTP.
//!
//! **After the token, the stream is MCP's stdio framing**: one JSON-RPC
//! message per line. The bridge therefore copies its stdin and stdout to the
//! socket unchanged, and an agent's stdio MCP client talks to this server as
//! if it were the bridge's own.
//!
//! **A connection that ends closes its reviews.** An agent that quits while a
//! proposal waits for review must not leave the diff on screen with no one to
//! receive its verdict. The end is seen as the socket reads EOF, before rmcp
//! drains the in-flight call that is waiting on that very review.

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, BufReader, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::mcp::auth;
use crate::mcp::tools_server::{PluginToolsServer, next_origin};

/// A running listener's lock file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolsLock {
    /// The editor's process.
    pub pid: u32,
    /// The loopback port.
    pub port: u16,
    /// What a connection's first line must be.
    pub token: String,
    /// The project the editor serves: a bridge started inside it picks this
    /// editor over another.
    pub root: String,
    /// The editor's version, for a bridge's error messages.
    pub version: String,
}

/// Where lock files live: under the config home with every lattice path, in
/// the cache directory that is always safe to delete (each lock is written
/// again when its listener starts). `None` when no config home resolves.
pub fn lock_dir() -> Option<PathBuf> {
    lattice_config::cache_home().map(|c| c.join("mcp"))
}

/// How long a connection has to send its token.
const HELLO_WAIT: Duration = Duration::from_secs(5);
/// The longest first line read: a token is 32 characters.
const HELLO_MAX: u64 = 128;

/// Closes the reviews tagged with an origin.
pub type CloseFn = Arc<dyn Fn(u64) + Send + Sync>;

/// A listening server. Dropped, it stops accepting, ends every connection
/// and removes its lock file.
pub struct ToolsListener {
    port: u16,
    lock: PathBuf,
    cancel: CancellationToken,
}

impl ToolsListener {
    /// Bind, write the lock under `dir`, and accept until dropped. `changed`
    /// fires when the plugins' tools change, and each connection tells its
    /// agent to list them again.
    pub async fn start(
        server: PluginToolsServer,
        root: &Path,
        dir: &Path,
        close: CloseFn,
        changed: broadcast::Sender<()>,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let token: Arc<str> = auth::generate_token()
            .map_err(|e| std::io::Error::other(e.to_string()))?
            .into();
        let lock = ToolsLock {
            pid: std::process::id(),
            port,
            token: token.to_string(),
            root: root.display().to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        };
        let path = write_lock(dir, &lock)?;
        let cancel = CancellationToken::new();
        tokio::spawn(accept(
            listener,
            token,
            server,
            close,
            changed,
            cancel.clone(),
        ));
        Ok(Self {
            port,
            lock: path,
            cancel,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The lock file a bridge is pointed at to reach this editor.
    pub fn lock_path(&self) -> &Path {
        &self.lock
    }
}

impl Drop for ToolsListener {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Err(e) = std::fs::remove_file(&self.lock) {
            tracing::debug!(target: "lattice::llm", path = %self.lock.display(), "llm: tools lock not removed: {e}");
        }
    }
}

/// Write `lock` as `<pid>-<port>.json` under `dir`, both private.
fn write_lock(dir: &Path, lock: &ToolsLock) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let path = dir.join(format!("{}-{}.json", lock.pid, lock.port));
    let json = serde_json::to_vec_pretty(lock).map_err(std::io::Error::other)?;
    crate::mcp::lockfile::write_private(&path, &json)?;
    Ok(path)
}

async fn accept(
    listener: TcpListener,
    token: Arc<str>,
    server: PluginToolsServer,
    close: CloseFn,
    changed: broadcast::Sender<()>,
    cancel: CancellationToken,
) {
    loop {
        let stream = tokio::select! {
            () = cancel.cancelled() => return,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => stream,
                Err(e) => {
                    tracing::debug!(target: "lattice::llm", "llm: tools listener accept failed: {e}");
                    continue;
                }
            },
        };
        tokio::spawn(serve(
            stream,
            token.clone(),
            server.clone(),
            close.clone(),
            changed.subscribe(),
            cancel.child_token(),
        ));
    }
}

async fn serve(
    stream: TcpStream,
    token: Arc<str>,
    server: PluginToolsServer,
    close: CloseFn,
    mut changed: broadcast::Receiver<()>,
    cancel: CancellationToken,
) {
    let (read, write) = stream.into_split();
    let mut read = BufReader::new(read);
    let mut hello = String::new();
    let said = tokio::time::timeout(
        HELLO_WAIT,
        (&mut read).take(HELLO_MAX).read_line(&mut hello),
    )
    .await;
    if !matches!(said, Ok(Ok(_))) || !auth::header_matches(&token, hello.trim_end()) {
        tracing::debug!(target: "lattice::llm", "llm: a tools connection did not send the token");
        return;
    }
    let origin = next_origin();
    let ended = {
        let close = close.clone();
        move || close(origin)
    };
    let read = OnEnd {
        inner: read,
        end: Some(Box::new(ended)),
    };
    match rmcp::service::serve_server(server.for_connection(origin), (read, write)).await {
        Ok(running) => {
            let peer = running.peer().clone();
            let notifier = tokio::spawn(async move {
                loop {
                    match changed.recv().await {
                        Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {
                            if peer.notify_tool_list_changed().await.is_err() {
                                return;
                            }
                        }
                        Err(broadcast::error::RecvError::Closed) => return,
                    }
                }
            });
            let stop = running.cancellation_token();
            tokio::select! {
                _ = running.waiting() => {}
                () = cancel.cancelled() => stop.cancel(),
            }
            notifier.abort();
        }
        Err(e) => {
            tracing::debug!(target: "lattice::llm", "llm: a tools connection did not initialize: {e}");
        }
    }
    // Ended, stopped or never initialized: nothing of this connection's may
    // stay on screen. A no-op when it opened no review.
    close(origin);
}

/// A reader that runs `end` once, at EOF or a read error.
struct OnEnd<R> {
    inner: R,
    end: Option<Box<dyn FnOnce() + Send>>,
}

impl<R: AsyncRead + Unpin> AsyncRead for OnEnd<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let room = buf.remaining() > 0;
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(read) = &polled
            && room
            && (read.is_err() || buf.filled().len() == before)
            && let Some(end) = self.end.take()
        {
            end();
        }
        polled
    }
}

/// The listener while `llm.serve_tools` is on, and what starting it takes.
pub struct ToolsService {
    server: PluginToolsServer,
    /// The project root, read when the listener starts.
    root: Arc<dyn Fn() -> PathBuf + Send + Sync>,
    /// Whether `llm.serve_tools` is on now.
    wanted: Arc<dyn Fn() -> bool + Send + Sync>,
    dir: Option<PathBuf>,
    close: CloseFn,
    changed: broadcast::Sender<()>,
    running: tokio::sync::Mutex<Option<ToolsListener>>,
    /// The running listener's lock, readable without the async lock: an ACP
    /// session being started points its bridge at it.
    lock: std::sync::Mutex<Option<PathBuf>>,
}

/// Registered by [`install`]; looked up as this type.
pub type ToolsServiceHandle = Arc<ToolsService>;

impl ToolsService {
    /// The running listener's lock file, `None` while it is off.
    pub fn lock_path(&self) -> Option<PathBuf> {
        self.lock.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Start or stop the listener to match `llm.serve_tools`. Reads the
    /// option when it runs, so toggling it quickly settles on the last value.
    pub async fn apply(&self) {
        let mut running = self.running.lock().await;
        let wanted = (self.wanted)();
        match (wanted, running.is_some()) {
            (true, false) => {
                let Some(dir) = &self.dir else {
                    tracing::warn!(target: "lattice::llm", "llm.serve_tools: no config home, so no lock file an agent could find");
                    return;
                };
                let root = (self.root)();
                match ToolsListener::start(
                    self.server.clone(),
                    &root,
                    dir,
                    self.close.clone(),
                    self.changed.clone(),
                )
                .await
                {
                    Ok(listener) => {
                        tracing::info!(target: "lattice::llm", "llm: serving plugin tools to outside agents; add them to Claude Code with `claude mcp add lattice -- lattice --mcp`");
                        *self.lock.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(listener.lock_path().to_path_buf());
                        *running = Some(listener);
                    }
                    Err(e) => {
                        tracing::warn!(target: "lattice::llm", "llm.serve_tools: could not listen: {e}")
                    }
                }
            }
            (false, true) => {
                *self.lock.lock().unwrap_or_else(|e| e.into_inner()) = None;
                *running = None;
                tracing::debug!(target: "lattice::llm", "llm: stopped serving plugin tools");
            }
            _ => {}
        }
    }
}

/// Closes an outside agent's reviews, on the editor thread.
struct CloseReviews(u64);

/// Wire the listener to `llm.serve_tools`. Runs after the llm layer's
/// install, whose broker, plugin registry and options it reads.
pub fn install(boot: &mut impl lattice_mode::SubsystemBoot) {
    use crate::llm::chat::LlmChatsHandle;
    use crate::llm::tools::plugin::{PluginToolsChanged, PluginToolsHandle};
    let (Some(chats), Some(plugins)) = (
        boot.service::<LlmChatsHandle>().map(|c| (*c).clone()),
        boot.service::<PluginToolsHandle>().map(|p| (*p).clone()),
    ) else {
        tracing::debug!(target: "lattice::llm", "llm: no plugin tools to serve");
        return;
    };
    let registry = boot
        .service::<Arc<lattice_config::ConfigRegistry>>()
        .map(|r| (*r).clone());
    let reviews = boot.inbound::<CloseReviews, _>(|CloseReviews(origin)| {
        vec![lattice_grammar::effect::Effect::CloseAllSessionDiffs {
            origin_session: origin,
        }]
    });
    let close: CloseFn = Arc::new(move |origin| {
        let _ = reviews.send(CloseReviews(origin));
    });
    let (changed, _) = broadcast::channel(16);
    {
        let changed = changed.clone();
        boot.event_bus()
            .subscribe_typed_fn::<PluginToolsChanged, _>(move |_| {
                // No connection listening is not an error.
                let _ = changed.send(());
                true
            });
    }
    let rules = {
        let chats = Arc::downgrade(&chats);
        Arc::new(move || chats.upgrade().map(|c| c.tool_rules()))
    };
    let root = {
        let chats = Arc::downgrade(&chats);
        Arc::new(move || chats.upgrade().map(|c| c.tools_root()).unwrap_or_default())
    };
    let server = PluginToolsServer::new(chats.tools().clone(), plugins, rules, 0);
    let service: ToolsServiceHandle = Arc::new(ToolsService {
        server,
        root,
        wanted: Arc::new(move || {
            registry
                .as_ref()
                .and_then(|r| r.get_typed::<crate::llm::options::LlmServeTools>())
                .is_some_and(|on| *on)
        }),
        dir: lock_dir(),
        close,
        changed,
        running: tokio::sync::Mutex::new(None),
        lock: std::sync::Mutex::new(None),
    });
    // `:set`, or the config file being read; and the value already set.
    {
        use lattice_protocol::event::{Event, EventKind};
        use lattice_runtime::{EventFilter, SubscriptionTarget};
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
        boot.event_bus().subscribe(
            EventFilter::kind(EventKind::OptionChanged),
            SubscriptionTarget::Channel(tx),
        );
        let service = service.clone();
        boot.runtime_handle().spawn(async move {
            service.apply().await;
            while let Some(event) = rx.recv().await {
                if matches!(&event, Event::OptionChanged { name, .. } if name == "llm.serve_tools")
                {
                    service.apply().await;
                }
            }
        });
    }
    // An ACP agent lattice starts is handed the bridge, pointed at this
    // editor, while the tools are served: it reaches them with no setup.
    #[cfg(feature = "acp")]
    if let Some(slot) = boot.service::<crate::acp::mcp_servers::SessionMcpServers>() {
        let service = Arc::downgrade(&service);
        slot.set(Arc::new(move || {
            let lock = service.upgrade().and_then(|s| s.lock_path());
            let exe = std::env::current_exe();
            match (lock, exe) {
                (Some(lock), Ok(exe)) => vec![bridge_server(exe, &lock)],
                (Some(_), Err(e)) => {
                    tracing::warn!(target: "lattice::llm", "llm: the agent is not given the plugins' tools: lattice cannot find its own executable: {e}");
                    Vec::new()
                }
                (None, _) => Vec::new(),
            }
        }));
    }
    boot.register_service::<ToolsServiceHandle>(service);
}

/// `lattice --mcp`, as an agent is told to start it: the editor's own
/// executable, pointed at this editor by its lock.
#[cfg(feature = "acp")]
fn bridge_server(exe: PathBuf, lock: &Path) -> crate::acp::mcp_servers::StdioMcpServer {
    crate::acp::mcp_servers::StdioMcpServer {
        name: "lattice".into(),
        command: exe,
        args: vec!["--mcp".into()],
        env: vec![(
            crate::mcp::tools_bridge::LOCK_ENV.into(),
            lock.display().to_string(),
        )],
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use std::sync::Mutex;

    use serde_json::{Value, json};
    use tokio::io::{AsyncWriteExt as _, BufReader};

    use super::*;
    use crate::llm::tools::plugin::{
        PluginTool, PluginToolKind, PluginToolResult, PluginTools, PluginToolsHandle, Reach,
        ToolPlugin,
    };
    use crate::llm::tools::{
        BoxFuture, CallContext, SourceTool, ToolBroker, ToolBrokerHandle, ToolError, ToolOutput,
        ToolSource,
    };

    struct Org;

    impl ToolPlugin for Org {
        fn tools(&self) -> BoxFuture<'_, Result<Vec<PluginTool>, String>> {
            Box::pin(async {
                Ok(vec![PluginTool {
                    name: "agenda".into(),
                    description: "today".into(),
                    input_schema: r#"{"type":"object"}"#.into(),
                    kind: PluginToolKind::Read,
                }])
            })
        }

        fn call(&self, _: &str, _: String) -> BoxFuture<'_, Result<PluginToolResult, String>> {
            Box::pin(async {
                Ok(PluginToolResult {
                    text: "Standup at 09:30".into(),
                    proposals: Vec::new(),
                })
            })
        }
    }

    struct NoLattice;

    impl ToolSource for NoLattice {
        fn list(&self) -> BoxFuture<'_, Result<Vec<SourceTool>, ToolError>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn call(
            &self,
            _: &str,
            _: Value,
            _: CallContext,
        ) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
            Box::pin(async { Err(ToolError::Failed("no edits here".into())) })
        }
    }

    /// A directory of the test's own, removed when dropped.
    pub(crate) struct Scratch(PathBuf);

    impl Scratch {
        pub(crate) fn new() -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "lattice-tools-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(crate) struct Running {
        _dir: Scratch,
        listener: ToolsListener,
        pub(crate) lock: ToolsLock,
        closed: Arc<Mutex<Vec<u64>>>,
        plugins: PluginToolsHandle,
        changed: broadcast::Sender<()>,
    }

    pub(crate) async fn running() -> Running {
        let broker: ToolBrokerHandle = Arc::new(ToolBroker::new());
        let plugins: PluginToolsHandle = Arc::new(PluginTools::new(
            broker.clone(),
            Arc::new(NoLattice),
            Arc::new(Vec::new),
        ));
        plugins
            .register("org", Arc::new(Org), Reach::Contained)
            .await
            .unwrap();
        let server = PluginToolsServer::new(broker, plugins.clone(), Arc::new(|| None), 0);
        let dir = Scratch::new();
        let closed = Arc::new(Mutex::new(Vec::new()));
        let close: CloseFn = {
            let closed = closed.clone();
            Arc::new(move |o| closed.lock().unwrap().push(o))
        };
        let (changed, _) = broadcast::channel(4);
        let listener = ToolsListener::start(
            server,
            Path::new("/work/project"),
            &dir.path().join("mcp"),
            close,
            changed.clone(),
        )
        .await
        .unwrap();
        let lock: ToolsLock =
            serde_json::from_slice(&std::fs::read(listener.lock_path()).unwrap()).unwrap();
        Running {
            _dir: dir,
            listener,
            lock,
            closed,
            plugins,
            changed,
        }
    }

    /// A client of the listener: sends `hello`, then JSON-RPC lines.
    struct Client {
        read: BufReader<tokio::net::tcp::OwnedReadHalf>,
        write: tokio::net::tcp::OwnedWriteHalf,
    }

    impl Client {
        async fn connect(port: u16, hello: &str) -> Self {
            let stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            let (read, mut write) = stream.into_split();
            write
                .write_all(format!("{hello}\n").as_bytes())
                .await
                .unwrap();
            Self {
                read: BufReader::new(read),
                write,
            }
        }

        async fn send(&mut self, v: Value) {
            let line = format!("{v}\n");
            self.write.write_all(line.as_bytes()).await.unwrap();
        }

        /// The next message, or `None` at EOF.
        async fn recv(&mut self) -> Option<Value> {
            let mut line = String::new();
            let n = tokio::time::timeout(Duration::from_secs(10), self.read.read_line(&mut line))
                .await
                .expect("an answer within 10s")
                .unwrap_or(0);
            (n > 0).then(|| serde_json::from_str(&line).unwrap())
        }

        /// The reply to request `id`, skipping notifications.
        async fn reply(&mut self, id: u64) -> Value {
            loop {
                let m = self.recv().await.expect("a reply before EOF");
                if m["id"] == json!(id) {
                    return m;
                }
            }
        }

        async fn initialize(&mut self) -> Value {
            self.send(json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "test", "version": "0"}
                }
            }))
            .await;
            let r = self.reply(1).await;
            self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
                .await;
            r
        }
    }

    /// The lock says where to connect, readable by the owner alone, and goes
    /// when the listener does.
    #[tokio::test]
    async fn the_lock_names_the_port_and_goes_with_the_listener() {
        let r = running().await;
        assert_eq!(r.lock.port, r.listener.port());
        assert_eq!(r.lock.pid, std::process::id());
        assert_eq!(r.lock.root, "/work/project");
        assert_eq!(r.lock.token.len(), 32);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(r.listener.lock_path()), 0o600);
            assert_eq!(mode(r.listener.lock_path().parent().unwrap()), 0o700);
        }
        let path = r.listener.lock_path().to_path_buf();
        drop(r.listener);
        assert!(!path.exists());
    }

    /// With the token, a client initializes, lists org's tool and calls it.
    #[tokio::test]
    async fn a_client_with_the_token_lists_and_calls_the_plugins_tools() {
        let r = running().await;
        let mut c = Client::connect(r.lock.port, &r.lock.token).await;
        let init = c.initialize().await;
        assert_eq!(init["result"]["serverInfo"]["name"], "lattice");
        assert_eq!(
            init["result"]["capabilities"]["tools"]["listChanged"],
            json!(true)
        );
        c.send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
            .await;
        let listed = c.reply(2).await;
        assert_eq!(listed["result"]["tools"][0]["name"], "org__agenda");
        c.send(json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "org__agenda", "arguments": {}}
        }))
        .await;
        let called = c.reply(3).await;
        assert_eq!(
            called["result"]["content"][0]["text"], "Standup at 09:30",
            "{called}"
        );
    }

    /// Without the token the connection is closed unanswered.
    #[tokio::test]
    async fn a_wrong_token_is_closed_before_any_mcp() {
        let r = running().await;
        let mut c = Client::connect(r.lock.port, "not-the-token").await;
        c.send(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
            .await;
        assert_eq!(c.recv().await, None);
    }

    /// A plugin loading or unloading tells a connected agent to list again.
    #[tokio::test]
    async fn a_change_in_the_plugins_tells_the_agent() {
        let r = running().await;
        let mut c = Client::connect(r.lock.port, &r.lock.token).await;
        c.initialize().await;
        r.plugins.unregister("org");
        r.changed.send(()).unwrap();
        loop {
            let m = c.recv().await.expect("a notification");
            if m["method"] == "notifications/tools/list_changed" {
                break;
            }
        }
    }

    /// A client that goes away closes what it had open, tagged with its own
    /// origin, as soon as its socket ends.
    #[tokio::test]
    async fn a_connection_that_ends_closes_its_reviews() {
        let r = running().await;
        let mut c = Client::connect(r.lock.port, &r.lock.token).await;
        c.initialize().await;
        drop(c);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while r.closed.lock().unwrap().is_empty() {
            assert!(tokio::time::Instant::now() < deadline, "never closed");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let origin = r.closed.lock().unwrap()[0];
        assert!(origin > crate::mcp::tools_server::ORIGIN_BASE);
    }

    /// The service listens while the option is on and stops when it goes
    /// off, lock file and all; applying the same value twice changes nothing.
    #[tokio::test]
    async fn the_service_follows_the_option() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let broker: ToolBrokerHandle = Arc::new(ToolBroker::new());
        let plugins: PluginToolsHandle = Arc::new(PluginTools::new(
            broker.clone(),
            Arc::new(NoLattice),
            Arc::new(Vec::new),
        ));
        let dir = Scratch::new();
        let on = Arc::new(AtomicBool::new(false));
        let service = ToolsService {
            server: PluginToolsServer::new(broker, plugins, Arc::new(|| None), 0),
            root: Arc::new(|| PathBuf::from("/work")),
            wanted: {
                let on = on.clone();
                Arc::new(move || on.load(Ordering::SeqCst))
            },
            dir: Some(dir.path().join("mcp")),
            close: Arc::new(|_| {}),
            changed: broadcast::channel(4).0,
            running: tokio::sync::Mutex::new(None),
            lock: std::sync::Mutex::new(None),
        };
        service.apply().await;
        assert_eq!(service.lock_path(), None);
        on.store(true, Ordering::SeqCst);
        service.apply().await;
        let lock = service.lock_path().expect("listening");
        assert!(lock.exists());
        service.apply().await;
        assert_eq!(service.lock_path().as_ref(), Some(&lock));
        on.store(false, Ordering::SeqCst);
        service.apply().await;
        assert_eq!(service.lock_path(), None);
        assert!(!lock.exists());
    }

    /// Dropping the listener ends a connected client too.
    #[tokio::test]
    async fn stopping_the_listener_ends_its_connections() {
        let r = running().await;
        let mut c = Client::connect(r.lock.port, &r.lock.token).await;
        c.initialize().await;
        drop(r.listener);
        while c.recv().await.is_some() {}
    }

    /// An ACP agent is told to start this editor's own executable as the
    /// bridge, with the lock that names this editor.
    #[cfg(feature = "acp")]
    #[test]
    fn an_acp_agent_is_handed_the_bridge_pointed_at_this_editor() {
        let s = bridge_server(PathBuf::from("/opt/lattice"), Path::new("/c/mcp/1-2.json"));
        assert_eq!(s.name, "lattice");
        assert_eq!(s.command, PathBuf::from("/opt/lattice"));
        assert_eq!(s.args, ["--mcp"]);
        assert_eq!(
            s.env,
            [(
                "LATTICE_MCP_LOCK".to_string(),
                "/c/mcp/1-2.json".to_string()
            )]
        );
    }
}
