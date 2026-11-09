//! External MCP servers as tool sources (LL.24, design `llm-layer.md` §6).
//!
//! Each `[llm.mcp.<name>]` table is one [`McpServer`]: an rmcp client
//! connection to a stdio child or a streamable-HTTP endpoint, added to the
//! [`ToolBroker`](super::ToolBroker) under its name, so its tools are
//! `name.tool` to a person and `name__tool` on the wire.
//!
//! **A server starts when a request first needs it**, that is, when a
//! request's `llm.tools` selects one of its tools and the broker lists them.
//! Starting (spawn or connect, `initialize`, `tools/list`) is bounded by
//! [`STARTUP_TIMEOUT`]; a server that fails or times out is left out of that
//! request's offer and the request goes on without it. The failure is shown
//! in `*llm-mcp*` and is not retried until the user restarts the server
//! there: a wrong command fails the same way every time, and retrying it on
//! every request would cost each one the attempt. A server that **was**
//! running and then exited is restarted by the next request that needs it.
//!
//! **Trust is ours** (§6): every external tool asks before it runs unless
//! the user's own config allows it by name (`allow = ["get_issue"]`), and
//! `readOnlyHint` is shown, never believed. A project file cannot declare a
//! server (the settings trust table refuses `mcp`), so only the user's
//! `lattice.toml` reaches here.
//!
//! **Lifetimes.** A stdio child runs in its own process group; stopping or
//! restarting the server, a failed start, and the child's connection ending
//! all kill the whole group, so a server launched through `npx` leaves no
//! `node` behind. A tool call dropped before it returns (the request
//! stopped) sends MCP's `notifications/cancelled`; the server stays up for
//! the next call.
//!
//! **Secrets.** `env` and `headers` values may name environment variables
//! (`"$GITHUB_TOKEN"`, `"Bearer ${TOKEN}"`); they are expanded when the
//! server starts, never shown in `*llm-mcp*`, and scrubbed from the child's
//! stderr before it is logged or shown.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult, ClientConfig, Tool};
use rmcp::service::{NotificationContext, RunningService};
use rmcp::{ClientHandler, Peer, RoleClient, ServiceError};
use serde_json::{Value, json};
use tokio::sync::watch;

use super::{BoxFuture, CallContext, SourceTool, ToolError, ToolOutput, ToolSource};
use crate::conversation::ToolKind;
use crate::llm::ToolDescriptor;

/// How long a server has to start: spawn or connect, `initialize`, and list
/// its tools. A request waits at most this long for it, once.
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// A result longer than this is cut, with a note; the same cap lattice's
/// own `read_file` uses.
pub const MAX_RESULT_BYTES: usize = super::lattice::MAX_BYTES;

/// Providers accept tool names of `[A-Za-z0-9_-]`, at most this long.
const MAX_WIRE_NAME: usize = 64;

/// Lines of a stdio server's stderr kept for `*llm-mcp*` and failure text.
const STDERR_LINES: usize = 20;

/// One `[llm.mcp.<name>]` table.
#[derive(Debug, Clone, PartialEq)]
pub struct McpServerConfig {
    /// `[A-Za-z0-9-]`, not `lattice`: the namespace of its tools.
    pub name: String,
    pub transport: McpTransport,
    /// Tools that run without asking, by their name on this server.
    pub allow: BTreeSet<String>,
}

/// How lattice reaches a server. `Debug` shows what [`label`](Self::label)
/// does and the names of env vars and headers, never their values: those
/// carry tokens.
#[derive(Clone, PartialEq)]
pub enum McpTransport {
    /// A child process speaking MCP on stdin/stdout.
    Stdio {
        command: String,
        args: Vec<String>,
        /// Added to lattice's environment; values may name variables.
        env: BTreeMap<String, String>,
        /// Where it runs; lattice's own directory when absent.
        cwd: Option<PathBuf>,
    },
    /// MCP's streamable HTTP.
    Http {
        url: String,
        /// Sent with every request; values may name variables.
        headers: BTreeMap<String, String>,
    },
}

impl std::fmt::Debug for McpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stdio { env, cwd, .. } => f
                .debug_struct("Stdio")
                .field("label", &self.label())
                .field("env", &env.keys().collect::<Vec<_>>())
                .field("cwd", cwd)
                .finish_non_exhaustive(),
            Self::Http { headers, .. } => f
                .debug_struct("Http")
                .field("label", &self.label())
                .field("headers", &headers.keys().collect::<Vec<_>>())
                .finish_non_exhaustive(),
        }
    }
}

impl McpTransport {
    /// What `*llm-mcp*` shows: the command and its arguments, or the URL
    /// without its query or credentials. Anything that can hold a token is
    /// written `***`: the argument after a flag naming one (`--token X`), a
    /// flag's own value (`--api-key=X`), and an argument or path segment that
    /// looks like one.
    pub fn label(&self) -> String {
        match self {
            Self::Stdio { command, args, .. } if args.is_empty() => format!("stdio: {command}"),
            Self::Stdio { command, args, .. } => {
                format!("stdio: {command} {}", shown_args(args).join(" "))
            }
            Self::Http { url, .. } => format!("http: {}", shown_url(url)),
        }
    }
}

/// `args` with every token-shaped value written `***`.
fn shown_args(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut after_secret_flag = false;
    for arg in args {
        let shown = if after_secret_flag {
            "***".to_string()
        } else if let Some((flag, _)) = arg.split_once('=').filter(|(f, _)| names_secret(f)) {
            format!("{flag}=***")
        } else if looks_secret(arg) {
            "***".to_string()
        } else {
            arg.clone()
        };
        after_secret_flag = arg.starts_with('-') && !arg.contains('=') && names_secret(arg);
        out.push(shown);
    }
    out
}

/// A flag whose name says its value is a credential.
fn names_secret(flag: &str) -> bool {
    let f = flag.to_ascii_lowercase();
    f.starts_with('-')
        && [
            "token",
            "key",
            "secret",
            "password",
            "passwd",
            "auth",
            "credential",
        ]
        .iter()
        .any(|w| f.contains(w))
}

/// Text shaped like a credential: long, one unbroken run of letters, digits,
/// `-` and `_`, holding both letters and digits. Package names, paths and
/// words are not (they have `/`, `@` or `.`, or no digits).
fn looks_secret(s: &str) -> bool {
    s.len() >= 24
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && s.chars().any(|c| c.is_ascii_digit())
        && s.chars().any(|c| c.is_ascii_alphabetic())
}

/// `url` with any `user:pass@` and everything from `?` or `#` removed, and a
/// token-shaped path segment written `***`.
fn shown_url(url: &str) -> String {
    let cut = url.find(['?', '#']).map_or(url, |i| &url[..i]);
    match cut.split_once("://") {
        Some((scheme, rest)) => {
            let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
            let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
            let path: Vec<&str> = path
                .split('/')
                .map(|seg| if looks_secret(seg) { "***" } else { seg })
                .collect();
            format!("{scheme}://{host}{}", path.join("/"))
        }
        None => cut.to_string(),
    }
}

/// Expand `$NAME` and `${NAME}` from the environment; `$$` is a `$`. A
/// variable that is not set is an error naming it, never a silent empty.
pub fn expand_env(value: &str) -> Result<String, String> {
    expand_with(value, |name| std::env::var(name).ok())
}

fn expand_with(value: &str, lookup: impl Fn(&str) -> Option<String>) -> Result<String, String> {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        if let Some(tail) = after.strip_prefix('$') {
            out.push('$');
            rest = tail;
            continue;
        }
        let (name, tail) = if let Some(braced) = after.strip_prefix('{') {
            match braced.find('}') {
                Some(end) => (&braced[..end], &braced[end + 1..]),
                None => return Err(format!("unclosed `${{` in `{value}`")),
            }
        } else {
            let end = after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(after.len());
            (&after[..end], &after[end..])
        };
        if name.is_empty() {
            // A lone `$` is literal.
            out.push('$');
            rest = after;
            continue;
        }
        match lookup(name) {
            Some(v) => out.push_str(&v),
            None => return Err(format!("environment variable {name} is not set")),
        }
        rest = tail;
    }
    out.push_str(rest);
    Ok(out)
}

/// Replaces each secret it was given with `***`, and the credential inside an
/// `Authorization`-style value (`Bearer <token>`) on its own, since a server
/// that echoes the token rarely echoes the scheme with it.
#[derive(Clone, Default)]
struct Redactor(Arc<Vec<String>>);

impl Redactor {
    fn new(secrets: impl IntoIterator<Item = String>) -> Self {
        // Values under 6 characters ("1", "true", "debug") are settings, not
        // credentials; scrubbing them would garble every line that held the
        // letters and hide nothing a token-shaped secret does not.
        let mut v: Vec<String> = secrets
            .into_iter()
            .flat_map(|s| {
                let inner = s
                    .split_once(' ')
                    .map(|(_, rest)| rest.trim().to_string())
                    .filter(|rest| !rest.is_empty() && !rest.contains(' '));
                std::iter::once(s).chain(inner)
            })
            .filter(|s| s.len() >= 6)
            .collect();
        v.sort_by_key(|s| std::cmp::Reverse(s.len()));
        Self(Arc::new(v))
    }

    fn scrub(&self, text: &str) -> String {
        let mut out = text.to_string();
        for s in self.0.iter() {
            out = out.replace(s.as_str(), "***");
        }
        out
    }
}

/// The last lines a stdio server wrote to stderr, scrubbed.
#[derive(Clone, Default)]
pub struct StderrTail(Arc<Mutex<VecDeque<String>>>);

impl StderrTail {
    fn push(&self, line: String) {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if g.len() == STDERR_LINES {
            g.pop_front();
        }
        g.push_back(line.chars().take(500).collect());
    }

    /// The last non-empty line, if any.
    pub fn last(&self) -> Option<String> {
        let g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        g.iter().rev().find(|l| !l.trim().is_empty()).cloned()
    }
}

/// Where a server is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerState {
    /// No request has needed it yet.
    NotStarted,
    Starting,
    Running,
    /// It could not start. Not retried until restarted from `*llm-mcp*`.
    Failed(String),
    /// It was running and its connection ended. The next request that
    /// needs it starts it again.
    Exited(String),
    /// Stopped from `*llm-mcp*`; requests leave it alone.
    Stopped,
}

impl std::fmt::Display for ServerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotStarted => f.write_str("not started"),
            Self::Starting => f.write_str("starting"),
            Self::Running => f.write_str("running"),
            Self::Failed(why) => write!(f, "failed: {why}"),
            Self::Exited(why) => write!(f, "exited: {why}"),
            Self::Stopped => f.write_str("stopped"),
        }
    }
}

/// One tool as `*llm-mcp*` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedTool {
    pub name: String,
    pub description: String,
    /// The server says it only reads (`readOnlyHint`). Shown, not trusted.
    pub read_only_claim: bool,
    /// Allowed by name in the user's config: runs without asking.
    pub allowed: bool,
    /// Why it is not offered to any request, if it is not.
    pub skipped: Option<String>,
}

/// A server as `*llm-mcp*` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerStatus {
    pub name: String,
    pub transport: String,
    pub state: ServerState,
    pub tools: Vec<ListedTool>,
}

/// A started connection, as a [`Connect`] function returns it.
pub struct Connection {
    pub service: RunningService<RoleClient, McpClient>,
    /// Kills a stdio child's process group when dropped.
    pub child: Option<ChildTree>,
    pub stderr: Option<StderrTail>,
}

impl Connection {
    /// A connection with no child process: HTTP, or a test's in-memory pipe.
    pub fn new(service: RunningService<RoleClient, McpClient>) -> Self {
        Self {
            service,
            child: None,
            stderr: None,
        }
    }
}

/// How a server is reached: given its config and the client handler, make
/// the transport and run MCP's `initialize` over it. [`connect_transport`]
/// in the editor; a test serves an in-process server instead.
pub type Connect = Arc<
    dyn Fn(McpServerConfig, McpClient) -> BoxFuture<'static, Result<Connection, String>>
        + Send
        + Sync,
>;

/// Called after any server's state or tools change.
pub type OnChange = Arc<dyn Fn() + Send + Sync>;

/// Published on the event bus when a server's state or tools change, so
/// `*llm-mcp*` repaints.
#[derive(Debug, Clone)]
pub struct McpServersChanged;

lattice_protocol::register_event!(
    McpServersChanged,
    "llm.mcp-servers-changed",
    "Fired when an external MCP server starts, fails, exits or stops, or its tool list changes.",
    "lattice-ai",
);

/// A stdio child's process group, killed when this drops.
pub struct ChildTree(u32);

impl Drop for ChildTree {
    fn drop(&mut self) {
        lattice_core::process::kill_process_tree(self.0);
    }
}

struct State {
    phase: ServerState,
    /// Bumped by every start and stop; a task whose generation is stale
    /// writes nothing.
    generation: u64,
    tools: Vec<Tool>,
    peer: Option<Peer<RoleClient>>,
    task: Option<tokio::task::AbortHandle>,
    stderr: Option<StderrTail>,
}

struct Shared {
    name: String,
    allow: BTreeSet<String>,
    state: Mutex<State>,
    /// Counts changes, so a waiter can sleep until the next one.
    changed: watch::Sender<u64>,
    on_change: OnChange,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn notify(&self) {
        self.changed.send_modify(|n| *n = n.wrapping_add(1));
        (self.on_change)();
    }

    /// Apply `f` if `generation` is still current, then tell the waiters.
    fn update_if(&self, generation: u64, f: impl FnOnce(&mut State)) {
        {
            let mut g = self.lock();
            if g.generation != generation {
                return;
            }
            f(&mut g);
        }
        self.notify();
    }
}

/// The client side of one connection. It hears `tools/list_changed` and
/// lists the tools again.
#[derive(Clone)]
pub struct McpClient {
    shared: Weak<Shared>,
    generation: u64,
}

impl McpClient {
    /// A handler attached to nothing: for a test that only needs a client.
    pub fn detached() -> Self {
        Self {
            shared: Weak::new(),
            generation: 0,
        }
    }
}

/// The `initialize` params: lattice's name and version, no capabilities.
fn client_config() -> ClientConfig {
    let v = json!({
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": { "name": "lattice", "version": env!("CARGO_PKG_VERSION") },
    });
    serde_json::from_value(v).unwrap_or_else(|e| {
        // Unreachable for the literal above; a unit test pins it.
        tracing::warn!(target: "lattice::llm", error = %e, "mcp: client info does not map");
        ClientConfig::default()
    })
}

impl ClientHandler for McpClient {
    fn get_info(&self) -> ClientConfig {
        client_config()
    }

    async fn on_tool_list_changed(&self, context: NotificationContext<RoleClient>) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        match context.peer.list_all_tools().await {
            Ok(tools) => shared.update_if(self.generation, |s| s.tools = tools),
            Err(e) => tracing::debug!(
                target: "lattice::llm",
                server = %shared.name,
                "mcp: listing tools after list_changed failed: {e}"
            ),
        }
    }
}

/// One external server: its config, its connection when there is one, and
/// a tool source over it.
pub struct McpServer {
    config: McpServerConfig,
    connect: Connect,
    runtime: tokio::runtime::Handle,
    shared: Arc<Shared>,
}

impl McpServer {
    pub fn new(
        config: McpServerConfig,
        connect: Connect,
        runtime: tokio::runtime::Handle,
        on_change: OnChange,
    ) -> Self {
        let shared = Arc::new(Shared {
            name: config.name.clone(),
            allow: config.allow.clone(),
            state: Mutex::new(State {
                phase: ServerState::NotStarted,
                generation: 0,
                tools: Vec::new(),
                peer: None,
                task: None,
                stderr: None,
            }),
            changed: watch::channel(0).0,
            on_change,
        });
        Self {
            config,
            connect,
            runtime,
            shared,
        }
    }

    pub fn name(&self) -> &str {
        &self.config.name
    }

    pub fn state(&self) -> ServerState {
        self.shared.lock().phase.clone()
    }

    /// What `*llm-mcp*` shows for this server.
    pub fn status(&self) -> ServerStatus {
        let g = self.shared.lock();
        let tools = g
            .tools
            .iter()
            .map(|t| {
                let name = t.name.to_string();
                ListedTool {
                    skipped: unusable(&self.config.name, &name),
                    allowed: self.config.allow.contains(&name),
                    read_only_claim: read_only_claim(t),
                    description: t
                        .description
                        .as_deref()
                        .unwrap_or_default()
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_string(),
                    name,
                }
            })
            .collect();
        let mut state = g.phase.clone();
        if let ServerState::Exited(why) = &mut state
            && let Some(line) = g.stderr.as_ref().and_then(StderrTail::last)
        {
            why.push_str(&format!(" ({line})"));
        }
        ServerStatus {
            name: self.config.name.clone(),
            transport: self.config.transport.label(),
            state,
            tools,
        }
    }

    /// Start the server if it is not running and may be started, and wait
    /// until it is running or has failed. Bounded by [`STARTUP_TIMEOUT`],
    /// which the start itself enforces.
    pub async fn ensure_running(&self) -> Result<(), String> {
        let mut changes = self.shared.changed.subscribe();
        loop {
            let started = {
                let mut g = self.shared.lock();
                match &g.phase {
                    ServerState::Running => return Ok(()),
                    ServerState::Failed(why) => {
                        return Err(format!(
                            "it failed to start ({why}); restart it from :llm-mcp"
                        ));
                    }
                    ServerState::Stopped => {
                        return Err("it was stopped from :llm-mcp".to_string());
                    }
                    ServerState::Starting => false,
                    ServerState::NotStarted | ServerState::Exited(_) => {
                        self.spawn_start(&mut g);
                        true
                    }
                }
            };
            if started {
                self.shared.notify();
            }
            if changes.changed().await.is_err() {
                return Err("lattice is closing".to_string());
            }
        }
    }

    /// Stop it, or abandon its start: the connection and any child go, and
    /// requests leave it alone until it is restarted.
    pub fn stop(&self) {
        {
            let mut g = self.shared.lock();
            g.generation = g.generation.wrapping_add(1);
            if let Some(task) = g.task.take() {
                task.abort();
            }
            g.peer = None;
            g.tools.clear();
            g.phase = ServerState::Stopped;
        }
        self.shared.notify();
    }

    /// Stop it and start it again now, whatever state it was in.
    pub fn restart(&self) {
        {
            let mut g = self.shared.lock();
            if let Some(task) = g.task.take() {
                task.abort();
            }
            self.spawn_start(&mut g);
        }
        self.shared.notify();
    }

    /// Begin a start under a new generation. The caller notifies.
    fn spawn_start(&self, g: &mut State) {
        g.generation = g.generation.wrapping_add(1);
        g.phase = ServerState::Starting;
        g.peer = None;
        g.tools.clear();
        g.stderr = None;
        let task = self.runtime.spawn(run(
            self.shared.clone(),
            self.config.clone(),
            self.connect.clone(),
            g.generation,
        ));
        g.task = Some(task.abort_handle());
    }

    /// The tools a request may be offered: those whose wire name providers
    /// accept.
    fn source_tools(&self) -> Vec<SourceTool> {
        let g = self.shared.lock();
        g.tools
            .iter()
            .filter(|t| unusable(&self.config.name, &t.name).is_none())
            .map(|t| SourceTool {
                descriptor: ToolDescriptor {
                    name: t.name.to_string(),
                    description: t.description.as_deref().unwrap_or_default().to_string(),
                    input_schema: Value::Object((*t.input_schema).clone()),
                },
                // Never believed for another server's tool (§6).
                kind: ToolKind::Other,
            })
            .collect()
    }

    fn running_peer(&self) -> Result<Peer<RoleClient>, String> {
        let g = self.shared.lock();
        g.peer.clone().ok_or_else(|| g.phase.to_string())
    }

    fn read_only_claim_of(&self, name: &str) -> bool {
        self.shared
            .lock()
            .tools
            .iter()
            .any(|t| t.name == name && read_only_claim(t))
    }

    fn stderr_note(&self) -> String {
        self.shared
            .lock()
            .stderr
            .as_ref()
            .and_then(StderrTail::last)
            .map(|l| format!(" ({l})"))
            .unwrap_or_default()
    }
}

/// Start, then hold the connection until it ends. Aborting the task drops
/// the connection, which closes it and kills a stdio child's group.
async fn run(shared: Arc<Shared>, config: McpServerConfig, connect: Connect, generation: u64) {
    let client = McpClient {
        shared: Arc::downgrade(&shared),
        generation,
    };
    let started = tokio::time::timeout(STARTUP_TIMEOUT, async {
        let conn = connect(config.clone(), client).await?;
        let offers_tools = conn
            .service
            .peer()
            .peer_info()
            .is_some_and(|info| info.capabilities.tools.is_some());
        let tools = if offers_tools {
            conn.service
                .peer()
                .list_all_tools()
                .await
                .map_err(|e| format!("listing its tools failed: {e}"))?
        } else {
            Vec::new()
        };
        Ok::<_, String>((conn, tools))
    })
    .await;
    let (conn, tools) = match started {
        Ok(Ok(started)) => started,
        Ok(Err(why)) => {
            tracing::warn!(target: "lattice::llm", "llm: MCP server `{}` failed to start: {why}", config.name);
            shared.update_if(generation, |s| {
                s.phase = ServerState::Failed(why);
                s.task = None;
            });
            return;
        }
        Err(_) => {
            let why = format!("it did not start within {}s", STARTUP_TIMEOUT.as_secs());
            tracing::warn!(target: "lattice::llm", "llm: MCP server `{}`: {why}", config.name);
            shared.update_if(generation, |s| {
                s.phase = ServerState::Failed(why);
                s.task = None;
            });
            return;
        }
    };
    let Connection {
        service,
        child,
        stderr,
    } = conn;
    let peer = service.peer().clone();
    tracing::debug!(target: "lattice::llm", server = %config.name, tools = tools.len(), "mcp: server running");
    shared.update_if(generation, |s| {
        s.phase = ServerState::Running;
        s.peer = Some(peer);
        s.tools = tools;
        s.stderr = stderr;
    });
    let why = match service.waiting().await {
        Ok(reason) => quit_reason(&reason),
        Err(e) => format!("its connection task ended ({e})"),
    };
    // Whatever of the child's tree is left goes with the connection.
    drop(child);
    shared.update_if(generation, |s| {
        s.phase = ServerState::Exited(why);
        s.peer = None;
        s.task = None;
    });
}

fn quit_reason(reason: &rmcp::service::QuitReason) -> String {
    use rmcp::service::QuitReason;
    match reason {
        QuitReason::Closed => "its connection closed".to_string(),
        QuitReason::Cancelled => "it was stopped".to_string(),
        other => format!("{other:?}"),
    }
}

fn read_only_claim(tool: &Tool) -> bool {
    tool.annotations
        .as_ref()
        .and_then(|a| a.read_only_hint)
        .unwrap_or(false)
}

/// Why a tool cannot be offered under `server`, if it cannot: providers
/// take `[A-Za-z0-9_-]`, at most [`MAX_WIRE_NAME`] long, for the wire name
/// `server__tool`.
fn unusable(server: &str, tool: &str) -> Option<String> {
    if tool.is_empty()
        || !tool
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Some("its name has characters model providers do not accept".to_string());
    }
    if server.len() + 2 + tool.len() > MAX_WIRE_NAME {
        return Some(format!(
            "`{server}__{tool}` is longer than the {MAX_WIRE_NAME} characters providers accept"
        ));
    }
    None
}

impl ToolSource for McpServer {
    fn list(&self) -> BoxFuture<'_, Result<Vec<SourceTool>, ToolError>> {
        Box::pin(async move {
            self.ensure_running().await.map_err(ToolError::Failed)?;
            Ok(self.source_tools())
        })
    }

    fn call(
        &self,
        name: &str,
        input: Value,
        _ctx: CallContext,
    ) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
        let name = name.to_string();
        Box::pin(async move {
            let server = &self.config.name;
            let peer = self.running_peer().map_err(|state| {
                ToolError::Failed(format!(
                    "the `{server}` MCP server is not running ({state})"
                ))
            })?;
            let arguments = match input {
                Value::Object(map) => Some(map),
                Value::Null => None,
                other => {
                    return Err(ToolError::InvalidArguments(format!(
                        "expected an object, got {other}"
                    )));
                }
            };
            let params: CallToolRequestParams =
                serde_json::from_value(json!({ "name": name, "arguments": arguments }))
                    .map_err(|e| ToolError::InvalidArguments(e.to_string()))?;
            // Dropping this future (the request stopped) sends the server
            // `notifications/cancelled` for the call.
            match peer.call_tool_once(params).await {
                Ok(CallToolResponse::Complete(result)) => result_output(result),
                Ok(_) => Err(ToolError::Failed(format!(
                    "`{server}.{name}` asked for input or started a background task, \
                     which lattice does not support"
                ))),
                Err(ServiceError::McpError(e)) => {
                    Err(ToolError::Failed(format!("{server}: {}", e.message)))
                }
                Err(ServiceError::TransportClosed | ServiceError::TransportSend(_)) => {
                    Err(ToolError::Failed(format!(
                        "the `{server}` MCP server stopped during the call{}",
                        self.stderr_note()
                    )))
                }
                Err(e) => Err(ToolError::Failed(format!("{server}: {e}"))),
            }
        })
    }

    /// The arguments, and the server's read-only claim when it makes one,
    /// marked as a claim.
    fn describe(&self, name: &str, input: &Value) -> Option<String> {
        if !self.read_only_claim_of(name) {
            return None;
        }
        let args = serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string());
        Some(format!(
            "{args}\n(the server says this tool only reads; lattice does not rely on that)"
        ))
    }

    fn allowed(&self, name: &str) -> bool {
        self.shared.allow.contains(name)
    }
}

/// A result as text the model reads; an error result is a tool error.
fn result_output(result: CallToolResult) -> Result<ToolOutput, ToolError> {
    let is_error = result.is_error == Some(true);
    let v = serde_json::to_value(&result).unwrap_or(Value::Null);
    let mut parts: Vec<String> = Vec::new();
    for block in v["content"].as_array().into_iter().flatten() {
        let part = match block["type"].as_str() {
            Some("text") => block["text"].as_str().unwrap_or_default().to_string(),
            Some("image") => format!(
                "[image ({}) omitted]",
                block["mimeType"].as_str().unwrap_or("unknown type")
            ),
            Some("audio") => format!(
                "[audio ({}) omitted]",
                block["mimeType"].as_str().unwrap_or("unknown type")
            ),
            Some("resource") => match block["resource"]["text"].as_str() {
                Some(text) => text.to_string(),
                None => format!(
                    "[resource {} omitted]",
                    block["resource"]["uri"].as_str().unwrap_or("")
                ),
            },
            Some("resource_link") => {
                format!("[resource {}]", block["uri"].as_str().unwrap_or(""))
            }
            _ => block.to_string(),
        };
        parts.push(part);
    }
    if parts.is_empty()
        && let Some(structured) = result.structured_content.as_ref()
    {
        parts.push(structured.to_string());
    }
    let text = capped(parts.join("\n"));
    if is_error {
        Err(ToolError::Failed(if text.is_empty() {
            "the tool reported an error".to_string()
        } else {
            text
        }))
    } else {
        Ok(ToolOutput { text })
    }
}

fn capped(text: String) -> String {
    if text.len() <= MAX_RESULT_BYTES {
        return text;
    }
    let mut cut = MAX_RESULT_BYTES;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}\n[cut: the result was {} bytes; only the first {MAX_RESULT_BYTES} are shown]",
        &text[..cut],
        text.len()
    )
}

/// Every configured server.
pub struct McpServers {
    servers: Vec<Arc<McpServer>>,
}

/// Shared by the llm layer's install and `*llm-mcp*`.
pub type McpServersHandle = Arc<McpServers>;

impl McpServers {
    pub fn new(
        configs: &[McpServerConfig],
        connect: Connect,
        runtime: &tokio::runtime::Handle,
        on_change: OnChange,
    ) -> Self {
        Self {
            servers: configs
                .iter()
                .map(|c| {
                    Arc::new(McpServer::new(
                        c.clone(),
                        connect.clone(),
                        runtime.clone(),
                        on_change.clone(),
                    ))
                })
                .collect(),
        }
    }

    pub fn servers(&self) -> &[Arc<McpServer>] {
        &self.servers
    }

    pub fn get(&self, name: &str) -> Option<&Arc<McpServer>> {
        self.servers.iter().find(|s| s.name() == name)
    }

    pub fn statuses(&self) -> Vec<ServerStatus> {
        self.servers.iter().map(|s| s.status()).collect()
    }

    /// Add every server to `broker` under its name. Nothing starts.
    pub fn register(&self, broker: &super::ToolBroker) {
        for s in &self.servers {
            broker.add_source(s.name(), s.clone());
        }
    }
}

/// The editor's [`Connect`]: a stdio child or streamable HTTP, per config.
pub fn connect_transport() -> Connect {
    Arc::new(|config: McpServerConfig, client: McpClient| {
        Box::pin(async move {
            match &config.transport {
                McpTransport::Stdio {
                    command,
                    args,
                    env,
                    cwd,
                } => connect_stdio(&config.name, command, args, env, cwd.as_ref(), client).await,
                McpTransport::Http { url, headers } => connect_http(url, headers, client).await,
            }
        })
    })
}

async fn connect_stdio(
    name: &str,
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    cwd: Option<&PathBuf>,
    client: McpClient,
) -> Result<Connection, String> {
    let mut expanded = Vec::with_capacity(env.len());
    for (k, v) in env {
        expanded.push((
            k.clone(),
            expand_env(v).map_err(|e| format!("env {k}: {e}"))?,
        ));
    }
    // What the label hides of the arguments is scrubbed from stderr too.
    let hidden_args = args
        .iter()
        .zip(shown_args(args))
        .filter(|(arg, shown)| *arg != shown)
        .map(|(arg, _)| {
            arg.split_once('=')
                .map_or(arg.clone(), |(_, v)| v.to_string())
        });
    let redactor = Redactor::new(expanded.iter().map(|(_, v)| v.clone()).chain(hidden_args));
    let failed = redactor.clone();
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args).envs(expanded).kill_on_drop(true);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    lattice_core::process::own_process_group(cmd.as_std_mut());
    let (process, stderr) = rmcp::transport::TokioChildProcess::builder(cmd)
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!("command not found: `{command}`"),
            _ => format!("could not start `{command}`: {e}"),
        })?;
    let tree = process.id().map(ChildTree);
    let tail = StderrTail::default();
    let reader = stderr.map(|stderr| {
        let (tail, server) = (tail.clone(), name.to_string());
        tokio::spawn(async move {
            use tokio::io::AsyncBufReadExt as _;
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = redactor.scrub(&line);
                tracing::debug!(target: "lattice::llm", server = %server, "mcp stderr: {line}");
                tail.push(line);
            }
        })
    });
    match rmcp::serve_client(client, process).await {
        Ok(service) => Ok(Connection {
            service,
            child: tree,
            stderr: Some(tail),
        }),
        Err(e) => {
            // A server that exits at once says why on stderr; let the reader
            // catch up before naming it.
            if let Some(reader) = reader {
                let _ = tokio::time::timeout(Duration::from_millis(500), reader).await;
            }
            let note = tail.last().map(|l| format!(": {l}")).unwrap_or_default();
            Err(failed.scrub(&format!("initialize failed ({e}){note}")))
        }
    }
}

async fn connect_http(
    url: &str,
    headers: &BTreeMap<String, String>,
    client: McpClient,
) -> Result<Connection, String> {
    use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
    use std::collections::HashMap;
    let mut map = HashMap::new();
    let mut secrets = Vec::new();
    for (k, v) in headers {
        let value = expand_env(v).map_err(|e| format!("header {k}: {e}"))?;
        let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
            .map_err(|_| format!("`{k}` is not a valid header name"))?;
        let mut header = reqwest::header::HeaderValue::from_str(&value)
            .map_err(|_| format!("header {k}: the value is not valid in a header"))?;
        header.set_sensitive(true);
        secrets.push(value);
        map.insert(name, header);
    }
    let redactor = Redactor::new(secrets);
    let transport = rmcp::transport::StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(url).custom_headers(map),
    );
    rmcp::serve_client(client, transport)
        .await
        .map(Connection::new)
        .map_err(|e| redactor.scrub(&format!("initialize failed ({e})")))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn env(name: &str) -> Option<String> {
        match name {
            "TOKEN" => Some("sekrit-123".to_string()),
            "EMPTY" => Some(String::new()),
            _ => None,
        }
    }

    #[test]
    fn variables_expand_and_a_missing_one_is_named() {
        assert_eq!(expand_with("$TOKEN", env).unwrap(), "sekrit-123");
        assert_eq!(
            expand_with("Bearer ${TOKEN}!", env).unwrap(),
            "Bearer sekrit-123!"
        );
        assert_eq!(expand_with("a$$b $ c", env).unwrap(), "a$b $ c");
        assert_eq!(expand_with("x${EMPTY}y", env).unwrap(), "xy");
        let e = expand_with("$NOPE", env).unwrap_err();
        assert!(e.contains("NOPE") && e.contains("not set"), "{e}");
        assert!(expand_with("${TOKEN", env).is_err());
    }

    #[test]
    fn a_label_never_shows_credentials_or_a_query() {
        let http = McpTransport::Http {
            url: "https://user:pw@example.com/mcp?token=abc#x".to_string(),
            headers: BTreeMap::new(),
        };
        assert_eq!(http.label(), "http: https://example.com/mcp");
        let stdio = McpTransport::Stdio {
            command: "npx".to_string(),
            args: vec!["-y".into(), "server".into()],
            env: BTreeMap::from([("T".to_string(), "$TOKEN".to_string())]),
            cwd: None,
        };
        assert_eq!(stdio.label(), "stdio: npx -y server");
        let stdio = McpTransport::Stdio {
            command: "srv".to_string(),
            args: vec![
                "--token".into(),
                "abc".into(),
                "--api-key=xyz".into(),
                "ghp_0123456789abcdefghijklmnopqrstuv".into(),
                "@scope/package-name".into(),
            ],
            env: BTreeMap::new(),
            cwd: None,
        };
        assert_eq!(
            stdio.label(),
            "stdio: srv --token *** --api-key=*** *** @scope/package-name"
        );
        let http = McpTransport::Http {
            url: "https://mcp.example.com/k/a1b2c3d4e5f6a7b8c9d0e1f2/sse".to_string(),
            headers: BTreeMap::new(),
        };
        assert_eq!(http.label(), "http: https://mcp.example.com/k/***/sse");
    }

    #[test]
    fn secrets_are_scrubbed_and_short_values_are_not() {
        let r = Redactor::new(["sekrit-123".to_string(), "1".to_string()]);
        assert_eq!(r.scrub("token sekrit-123 at 1"), "token *** at 1");
        // A header's token alone, as a server echoes it.
        let r = Redactor::new(["Bearer sekrit-456".to_string()]);
        assert_eq!(r.scrub("bad token sekrit-456"), "bad token ***");
    }

    #[test]
    fn names_providers_reject_are_not_offered() {
        assert_eq!(unusable("github", "get_issue"), None);
        assert!(unusable("github", "get.issue").is_some());
        assert!(unusable("github", "").is_some());
        assert!(unusable("github", &"x".repeat(60)).is_some());
    }

    #[test]
    fn results_become_text_and_an_error_result_is_a_tool_error() {
        let ok: CallToolResult = serde_json::from_value(json!({
            "content": [
                { "type": "text", "text": "one" },
                { "type": "image", "data": "AAAA", "mimeType": "image/png" },
            ]
        }))
        .unwrap();
        assert_eq!(
            result_output(ok).unwrap().text,
            "one\n[image (image/png) omitted]"
        );
        let err: CallToolResult = serde_json::from_value(json!({
            "content": [{ "type": "text", "text": "no such repo" }],
            "isError": true
        }))
        .unwrap();
        assert_eq!(
            result_output(err).unwrap_err(),
            ToolError::Failed("no such repo".to_string())
        );
    }

    #[test]
    fn a_long_result_is_cut_with_a_note() {
        let text = capped("é".repeat(MAX_RESULT_BYTES));
        assert!(text.len() < MAX_RESULT_BYTES + 200);
        assert!(text.ends_with("are shown]"), "{}", &text[text.len() - 80..]);
    }

    #[test]
    fn the_client_info_maps() {
        let v = serde_json::to_value(client_config()).unwrap();
        assert_eq!(v["clientInfo"]["name"], "lattice");
    }

    #[test]
    fn debug_names_env_and_headers_but_never_their_values() {
        let stdio = McpTransport::Stdio {
            command: "srv".into(),
            args: vec![],
            env: BTreeMap::from([("GITHUB_TOKEN".into(), "ghp_secret".into())]),
            cwd: None,
        };
        let http = McpTransport::Http {
            url: "https://u:pw@x.test/mcp?key=q".into(),
            headers: BTreeMap::from([("Authorization".into(), "Bearer tok_secret".into())]),
        };
        let shown = format!("{stdio:?} {http:?}");
        for secret in ["ghp_secret", "tok_secret", "pw@", "key=q"] {
            assert!(!shown.contains(secret), "{shown}");
        }
        assert!(
            shown.contains("GITHUB_TOKEN") && shown.contains("Authorization"),
            "{shown}"
        );
    }
}
