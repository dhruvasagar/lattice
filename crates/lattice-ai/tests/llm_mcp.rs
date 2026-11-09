//! LL.24: external MCP servers as tool sources.
//!
//! The servers here are in-process rmcp servers over a duplex pipe (the
//! workspace's LSP tests use the same shape), so every case is
//! deterministic and waits on what the server or the client reports, never
//! on a timer. Three cases use the real transports, because the transport is
//! what they test: a command that does not exist, a child whose process tree
//! must die with it, and streamable HTTP against a local responder.
#![cfg(feature = "llm")]
#![allow(clippy::unwrap_used, clippy::panic)]

mod llm_support;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lattice_ai::llm::tools::broker::{Permission, ToolBroker};
use lattice_ai::llm::tools::mcp::{
    Connect, Connection, McpClient, McpServer, McpServerConfig, McpTransport, ServerState,
    connect_transport,
};
use lattice_ai::llm::tools::{Answer, CallContext, ToolError, ToolSource};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ErrorCode, ListToolsResult, PaginatedRequestParams,
    ServerConfig, Tool,
};
use rmcp::service::{NotificationContext, RequestContext, RoleServer};
use rmcp::{ErrorData, Peer, ServerHandler};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};

/// Long enough never to fire on a healthy run; a hang fails the test instead
/// of wedging the suite.
const GUARD: Duration = Duration::from_secs(20);

async fn within<T>(what: &str, f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(GUARD, f)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

fn tool(name: &str, read_only: bool) -> Tool {
    let mut t = json!({
        "name": name,
        "description": format!("{name} things"),
        "inputSchema": { "type": "object" },
    });
    if read_only {
        t["annotations"] = json!({ "readOnlyHint": true });
    }
    serde_json::from_value(t).unwrap()
}

/// What the fixture server saw and can be told to do.
#[derive(Default)]
struct Seen {
    peer: Mutex<Option<Peer<RoleServer>>>,
    initialized: Notify,
    hanging: Notify,
    cancelled: Notify,
}

#[derive(Clone)]
struct Fixture {
    tools: Arc<Mutex<Vec<Tool>>>,
    seen: Arc<Seen>,
}

impl Fixture {
    fn new(tools: Vec<Tool>) -> Self {
        Self {
            tools: Arc::new(Mutex::new(tools)),
            seen: Arc::default(),
        }
    }
}

impl ServerHandler for Fixture {
    fn get_info(&self) -> ServerConfig {
        serde_json::from_value(json!({
            "protocolVersion": "2025-06-18",
            "capabilities": { "tools": { "listChanged": true } },
            "serverInfo": { "name": "fixture", "version": "0" },
        }))
        .unwrap()
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools = self.tools.lock().unwrap().clone();
        Ok(serde_json::from_value(json!({ "tools": tools })).unwrap())
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let args = request.arguments.map(Value::Object).unwrap_or(json!({}));
        let result = match request.name.as_ref() {
            "echo" => json!({ "content": [{ "type": "text", "text": format!("echo {args}") }] }),
            "fail" => {
                json!({ "content": [{ "type": "text", "text": "no such thing" }], "isError": true })
            }
            "hang" => {
                self.seen.hanging.notify_one();
                context.ct.cancelled().await;
                self.seen.cancelled.notify_one();
                json!({ "content": [] })
            }
            other => {
                return Err(ErrorData::new(
                    ErrorCode::METHOD_NOT_FOUND,
                    format!("unknown tool {other}"),
                    None,
                ));
            }
        };
        let result: rmcp::model::CallToolResult = serde_json::from_value(result).unwrap();
        Ok(CallToolResponse::from(result))
    }

    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        *self.seen.peer.lock().unwrap() = Some(context.peer.clone());
        self.seen.initialized.notify_one();
    }
}

/// A [`Connect`] serving `fixture` over an in-memory pipe. Counts its
/// starts, and keeps the pipe's relay so a test can cut it: the client then
/// sees end-of-file mid-call, as from a process that died.
#[derive(Clone)]
struct InProcess {
    fixture: Fixture,
    starts: Arc<AtomicUsize>,
    relay: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl InProcess {
    fn new(fixture: Fixture) -> Self {
        Self {
            fixture,
            starts: Arc::default(),
            relay: Arc::default(),
        }
    }

    fn connect(&self) -> Connect {
        let me = self.clone();
        Arc::new(move |_config, client: McpClient| {
            let me = me.clone();
            Box::pin(async move {
                me.starts.fetch_add(1, Ordering::SeqCst);
                // client <-> relay <-> server: aborting the relay drops both
                // of its ends at once.
                let (client_io, mut near) = tokio::io::duplex(64 * 1024);
                let (mut far, server_io) = tokio::io::duplex(64 * 1024);
                let relay = tokio::spawn(async move {
                    let _ = tokio::io::copy_bidirectional(&mut near, &mut far).await;
                });
                *me.relay.lock().unwrap() = Some(relay);
                let fixture = me.fixture.clone();
                tokio::spawn(async move {
                    if let Ok(running) = rmcp::serve_server(fixture, server_io).await {
                        let _ = running.waiting().await;
                    }
                });
                rmcp::serve_client(client, client_io)
                    .await
                    .map(Connection::new)
                    .map_err(|e| e.to_string())
            })
        })
    }

    /// The server dies: the pipe is cut with no reply in flight.
    fn kill(&self) {
        if let Some(task) = self.relay.lock().unwrap().take() {
            task.abort();
        }
    }

    fn starts(&self) -> usize {
        self.starts.load(Ordering::SeqCst)
    }
}

/// Every change a server reports, as a channel a test can wait on.
fn changes() -> (
    lattice_ai::llm::tools::mcp::OnChange,
    mpsc::UnboundedReceiver<()>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    (
        Arc::new(move || {
            let _ = tx.send(());
        }),
        rx,
    )
}

fn config(name: &str, allow: &[&str]) -> McpServerConfig {
    McpServerConfig {
        name: name.to_string(),
        transport: McpTransport::Stdio {
            command: "unused-in-process".to_string(),
            args: vec![],
            env: BTreeMap::new(),
            cwd: None,
        },
        allow: allow.iter().map(|s| s.to_string()).collect(),
        call_timeout: lattice_ai::llm::tools::mcp::DEFAULT_CALL_TIMEOUT,
    }
}

fn server(name: &str, allow: &[&str], connect: Connect) -> Arc<McpServer> {
    let (on_change, _rx) = changes();
    Arc::new(McpServer::new(
        config(name, allow),
        connect,
        tokio::runtime::Handle::current(),
        on_change,
    ))
}

fn selection(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn asking(answer: Answer, asked: Arc<Mutex<Vec<String>>>) -> CallContext {
    CallContext {
        ask: Some(Arc::new(move |q| {
            asked
                .lock()
                .unwrap()
                .push(format!("{}|{}", q.tool, q.detail.unwrap_or_default()));
            Box::pin(async move { answer })
        })),
        ..CallContext::default()
    }
}

/// Wait until `cond` holds, re-checking on each change the server reports.
async fn until(what: &str, rx: &mut mpsc::UnboundedReceiver<()>, cond: impl Fn() -> bool) {
    within(what, async {
        while !cond() {
            rx.recv().await;
        }
    })
    .await;
}

/// `github.*` selects the server's tools, namespaced on the wire; a request
/// that selects none of them does not start it.
#[tokio::test]
async fn tools_are_namespaced_and_selected_and_an_unselected_server_never_starts() {
    let fixture = InProcess::new(Fixture::new(vec![tool("echo", false), tool("fail", false)]));
    let broker = ToolBroker::new();
    let github = server("github", &[], fixture.connect());
    broker.add_source("github", github.clone());

    let none = within(
        "an empty offer",
        broker.offer(&selection(&["lattice.read"])),
    )
    .await;
    assert!(none.is_empty());
    assert_eq!(github.state(), ServerState::NotStarted);
    assert_eq!(fixture.starts(), 0);

    let offer = within("the offer", broker.offer(&selection(&["github.*"]))).await;
    let wire: Vec<&str> = offer.tools().iter().map(|t| t.wire.as_str()).collect();
    assert_eq!(wire, ["github__echo", "github__fail"]);
    assert_eq!(github.state(), ServerState::Running);

    let one = within("one tool", broker.offer(&selection(&["github.fail"]))).await;
    let wire: Vec<&str> = one.tools().iter().map(|t| t.wire.as_str()).collect();
    assert_eq!(wire, ["github__fail"]);
    assert_eq!(fixture.starts(), 1, "a running server is reused");
}

/// An external tool asks unless the user's config allows it by name; a
/// denial is the tool's result, and with nothing to ask it is refused.
#[tokio::test]
async fn external_tools_ask_unless_allowed_by_name() {
    let fixture = InProcess::new(Fixture::new(vec![tool("echo", false), tool("fail", false)]));
    let broker = ToolBroker::new();
    broker.add_source("github", server("github", &["echo"], fixture.connect()));
    let offer = within("the offer", broker.offer(&selection(&["github.*"]))).await;
    assert_eq!(
        offer.find("github__echo").unwrap().permission,
        Permission::Run
    );
    assert_eq!(
        offer.find("github__fail").unwrap().permission,
        Permission::Ask
    );

    let asked = Arc::new(Mutex::new(Vec::new()));
    let ran = within(
        "the allowed call",
        offer.call(
            "github__echo",
            json!({ "x": 1 }),
            asking(Answer::Deny, asked.clone()),
        ),
    )
    .await
    .unwrap();
    assert_eq!(ran.text, r#"echo {"x":1}"#);
    assert!(
        asked.lock().unwrap().is_empty(),
        "an allowed tool never asks"
    );

    let denied = within(
        "the denied call",
        offer.call(
            "github__fail",
            json!({}),
            asking(Answer::Deny, asked.clone()),
        ),
    )
    .await
    .unwrap_err();
    assert!(matches!(denied, ToolError::Refused(_)), "{denied:?}");
    assert_eq!(asked.lock().unwrap().len(), 1);
    assert!(asked.lock().unwrap()[0].starts_with("github.fail|"));

    let nobody = within(
        "the unaskable call",
        offer.call("github__fail", json!({}), CallContext::default()),
    )
    .await
    .unwrap_err();
    assert!(matches!(nobody, ToolError::Refused(_)), "{nobody:?}");

    let allowed_once = within(
        "the allowed-once call",
        offer.call("github__fail", json!({}), asking(Answer::Once, asked)),
    )
    .await
    .unwrap_err();
    assert_eq!(allowed_once, ToolError::Failed("no such thing".to_string()));
}

/// `readOnlyHint` is shown, in the question and the listing, and changes
/// nothing: the tool still asks.
#[tokio::test]
async fn read_only_hint_is_shown_and_not_trusted() {
    let fixture = InProcess::new(Fixture::new(vec![tool("echo", true)]));
    let github = server("github", &[], fixture.connect());
    let broker = ToolBroker::new();
    broker.add_source("github", github.clone());
    let offer = within("the offer", broker.offer(&selection(&["github.*"]))).await;
    assert_eq!(
        offer.find("github__echo").unwrap().permission,
        Permission::Ask
    );

    let asked = Arc::new(Mutex::new(Vec::new()));
    within(
        "the call",
        offer.call(
            "github__echo",
            json!({}),
            asking(Answer::Once, asked.clone()),
        ),
    )
    .await
    .unwrap();
    let question = asked.lock().unwrap()[0].clone();
    assert!(question.contains("only reads"), "{question}");
    assert!(github.status().tools[0].read_only_claim);
}

/// A server that cannot start is left out of the offer, the request goes
/// on with the rest, the failure is in its status, and the next request
/// does not try it again.
#[tokio::test]
async fn a_server_failing_to_start_is_reported_and_blocks_nothing() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let broken: Connect = {
        let attempts = attempts.clone();
        Arc::new(move |_config, _client| {
            attempts.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err("initialize failed (boom)".to_string()) })
        })
    };
    let fine = InProcess::new(Fixture::new(vec![tool("echo", false)]));
    let broker = ToolBroker::new();
    let (on_change, mut changed) = changes();
    let bad = Arc::new(McpServer::new(
        config("bad", &[]),
        broken,
        tokio::runtime::Handle::current(),
        on_change,
    ));
    broker.add_source("bad", bad.clone());
    broker.add_source("good", server("good", &[], fine.connect()));

    let offer = within("the offer", broker.offer(&selection(&["bad.*", "good.*"]))).await;
    let wire: Vec<&str> = offer.tools().iter().map(|t| t.wire.as_str()).collect();
    assert_eq!(wire, ["good__echo"]);
    assert!(
        matches!(bad.status().state, ServerState::Failed(ref why) if why.contains("boom")),
        "{:?}",
        bad.status().state
    );

    let again = within("a second offer", broker.offer(&selection(&["bad.*"]))).await;
    assert!(again.is_empty());
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "a failed start waits for a restart"
    );

    bad.restart();
    until("the restart to fail again", &mut changed, || {
        matches!(bad.state(), ServerState::Failed(_))
    })
    .await;
    assert_eq!(attempts.load(Ordering::SeqCst), 2, "a restart tries again");
}

/// A command that does not exist fails to start with a message naming it.
#[tokio::test]
async fn a_missing_command_fails_with_its_name() {
    let mut cfg = config("missing", &[]);
    cfg.transport = McpTransport::Stdio {
        command: "lattice-no-such-mcp-server-xyz".to_string(),
        args: vec![],
        env: BTreeMap::new(),
        cwd: None,
    };
    let (on_change, _rx) = changes();
    let s = McpServer::new(
        cfg,
        connect_transport(),
        tokio::runtime::Handle::current(),
        on_change,
    );
    let why = within("the failed start", s.ensure_running())
        .await
        .unwrap_err();
    assert!(why.contains("lattice-no-such-mcp-server-xyz"), "{why}");
    assert!(
        matches!(s.state(), ServerState::Failed(ref w) if w.contains("command not found")),
        "{:?}",
        s.state()
    );
}

/// A server dying mid-call becomes that call's error; the server shows it
/// exited, and the next request that needs it starts it again.
#[tokio::test]
async fn a_server_dying_mid_call_is_a_tool_error_and_restarts_on_next_need() {
    let fixture = InProcess::new(Fixture::new(vec![tool("hang", false)]));
    let (on_change, mut changed) = changes();
    let github = Arc::new(McpServer::new(
        config("github", &["hang"]),
        fixture.connect(),
        tokio::runtime::Handle::current(),
        on_change,
    ));
    let broker = ToolBroker::new();
    broker.add_source("github", github.clone());
    let offer = within("the offer", broker.offer(&selection(&["github.*"]))).await;

    let call = offer.call("github__hang", json!({}), CallContext::default());
    tokio::pin!(call);
    let seen = fixture.fixture.seen.clone();
    tokio::select! {
        _ = &mut call => panic!("the call returned before the server died"),
        _ = within("the call to reach the server", seen.hanging.notified()) => {}
    }
    fixture.kill();
    let err = within("the call to fail", call).await.unwrap_err();
    assert!(
        matches!(&err, ToolError::Failed(why) if why.contains("stopped during the call")),
        "{err:?}"
    );
    until("the server to show it exited", &mut changed, || {
        matches!(github.state(), ServerState::Exited(_))
    })
    .await;

    let again = within("the next offer", broker.offer(&selection(&["github.*"]))).await;
    assert_eq!(again.tools().len(), 1);
    assert_eq!(fixture.starts(), 2);
}

/// `notifications/tools/list_changed` updates the list; the next offer has
/// the new tool.
#[tokio::test]
async fn tools_list_changed_updates_the_list() {
    let fixture = Fixture::new(vec![tool("echo", false)]);
    let in_process = InProcess::new(fixture.clone());
    let (on_change, mut changed) = changes();
    let github = Arc::new(McpServer::new(
        config("github", &[]),
        in_process.connect(),
        tokio::runtime::Handle::current(),
        on_change,
    ));
    let broker = ToolBroker::new();
    broker.add_source("github", github.clone());
    within("the first offer", broker.offer(&selection(&["github.*"]))).await;
    // `on_initialized` may already have run; `Notify` keeps one permit.
    within("initialized", async {
        if fixture.seen.peer.lock().unwrap().is_none() {
            fixture.seen.initialized.notified().await;
        }
    })
    .await;

    fixture.tools.lock().unwrap().push(tool("fail", false));
    let peer = fixture.seen.peer.lock().unwrap().clone().unwrap();
    peer.notify_tool_list_changed().await.unwrap();
    until("the new list", &mut changed, || {
        github.status().tools.len() == 2
    })
    .await;

    let offer = within("the next offer", broker.offer(&selection(&["github.*"]))).await;
    let wire: Vec<&str> = offer.tools().iter().map(|t| t.wire.as_str()).collect();
    assert_eq!(wire, ["github__echo", "github__fail"]);
}

/// Dropping a call (the request stopped) cancels it on the server, and the
/// server stays up for the next one.
#[tokio::test]
async fn a_stopped_call_is_cancelled_on_the_server() {
    let fixture = InProcess::new(Fixture::new(vec![tool("hang", false), tool("echo", false)]));
    let github = server("github", &["hang", "echo"], fixture.connect());
    let broker = ToolBroker::new();
    broker.add_source("github", github.clone());
    let offer = within("the offer", broker.offer(&selection(&["github.*"]))).await;
    let seen = fixture.fixture.seen.clone();

    let call = tokio::spawn({
        let offer = offer.clone();
        async move {
            offer
                .call("github__hang", json!({}), CallContext::default())
                .await
        }
    });
    within("the call to reach the server", seen.hanging.notified()).await;
    call.abort();
    within("the server to hear the cancel", seen.cancelled.notified()).await;

    let next = within(
        "the next call",
        offer.call("github__echo", json!({}), CallContext::default()),
    )
    .await
    .unwrap();
    assert_eq!(next.text, "echo {}");
    assert_eq!(github.state(), ServerState::Running);
}

/// A call that runs past the server's `timeout` fails, saying how to allow
/// longer, and is cancelled on the server, which stays up.
#[tokio::test]
async fn a_call_past_its_timeout_is_cancelled_and_fails() {
    let fixture = InProcess::new(Fixture::new(vec![tool("hang", false)]));
    let (on_change, _rx) = changes();
    let github = Arc::new(McpServer::new(
        McpServerConfig {
            call_timeout: std::time::Duration::from_millis(200),
            ..config("github", &["hang"])
        },
        fixture.connect(),
        tokio::runtime::Handle::current(),
        on_change,
    ));
    let broker = ToolBroker::new();
    broker.add_source("github", github.clone());
    let offer = within("the offer", broker.offer(&selection(&["github.*"]))).await;
    let seen = fixture.fixture.seen.clone();
    let err = within(
        "the call to time out",
        offer.call("github__hang", json!({}), CallContext::default()),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, ToolError::Failed(why) if why.contains("took longer") && why.contains("timeout")),
        "{err:?}"
    );
    within("the server to hear the cancel", seen.cancelled.notified()).await;
    assert_eq!(github.state(), ServerState::Running);
}

/// Stopping a stdio server kills its child and the child's own children:
/// the tree runs in its own process group.
#[cfg(unix)]
#[tokio::test]
async fn stopping_a_stdio_server_kills_its_process_tree() {
    let dir = std::env::temp_dir().join(format!("lattice-mcp-tree-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pid_file = dir.join("grandchild.pid");
    let _ = std::fs::remove_file(&pid_file);
    // The shell starts a grandchild and never speaks MCP, so the server
    // stays `Starting` until stopped.
    let script = format!("sleep 600 & echo $! > '{}'; wait", pid_file.display());
    let mut cfg = config("tree", &[]);
    cfg.transport = McpTransport::Stdio {
        command: "sh".to_string(),
        args: vec!["-c".to_string(), script],
        env: BTreeMap::new(),
        cwd: None,
    };
    let (on_change, _rx) = changes();
    let s = Arc::new(McpServer::new(
        cfg,
        connect_transport(),
        tokio::runtime::Handle::current(),
        on_change,
    ));
    let waiting = tokio::spawn({
        let s = s.clone();
        async move { s.ensure_running().await }
    });
    // The pid file is written by a process outside the runtime; there is no
    // event to wait on, so this polls it (bounded by the guard).
    let pid = within("the grandchild's pid", async {
        loop {
            if let Some(pid) = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|t| t.trim().parse::<u32>().ok())
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(alive(pid), "the grandchild should be running");

    s.stop();
    let result = within("the waiter", waiting).await.unwrap();
    assert!(result.unwrap_err().contains("stopped"));
    // Likewise: process death is the OS's, reported to no one here.
    within("the grandchild to die", async {
        while alive(pid) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Streamable HTTP: the configured header reaches the server, its value
/// expanded from the environment, and a call round-trips.
#[tokio::test]
async fn streamable_http_sends_the_expanded_header_and_calls_a_tool() {
    use tokio::io::AsyncWriteExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let auth_seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let responder = tokio::spawn({
        let auth_seen = auth_seen.clone();
        async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let auth_seen = auth_seen.clone();
                tokio::spawn(async move {
                    let request = llm_support::read_request(&mut socket).await;
                    let (head, body) = request.split_once("\r\n\r\n").unwrap_or((&request, ""));
                    if let Some(line) = head
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                    {
                        auth_seen
                            .lock()
                            .unwrap()
                            .push(line["authorization:".len()..].trim().to_string());
                    }
                    let response = if !head.starts_with("POST") {
                        "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                    } else {
                        let msg: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                        match http_reply(&msg) {
                            Some(reply) => {
                                let reply = reply.to_string();
                                format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                                    reply.len()
                                )
                            }
                            None => "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                        }
                    };
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        }
    });

    // Setting a variable needs `unsafe` (edition 2024), which the workspace
    // denies, so the header names one every test process has: `PATH`.
    let path_value = std::env::var("PATH").unwrap();
    let cfg = McpServerConfig {
        name: "docs".to_string(),
        transport: McpTransport::Http {
            url: format!("http://{addr}/mcp"),
            headers: BTreeMap::from([("Authorization".to_string(), "Bearer ${PATH}".to_string())]),
        },
        allow: BTreeSet::from(["echo".to_string()]),
        call_timeout: lattice_ai::llm::tools::mcp::DEFAULT_CALL_TIMEOUT,
    };
    let (on_change, _rx) = changes();
    let docs = Arc::new(McpServer::new(
        cfg,
        connect_transport(),
        tokio::runtime::Handle::current(),
        on_change,
    ));
    within("the start", docs.ensure_running()).await.unwrap();
    let tools = within("the list", docs.list()).await.unwrap();
    assert_eq!(tools[0].descriptor.name, "echo");
    let out = within(
        "the call",
        docs.call("echo", json!({ "q": "x" }), CallContext::default()),
    )
    .await
    .unwrap();
    assert_eq!(out.text, "http echo");
    let seen = auth_seen.lock().unwrap().clone();
    assert!(!seen.is_empty(), "no request carried the header");
    assert!(
        seen.iter().all(|v| *v == format!("Bearer {path_value}")),
        "{seen:?}"
    );
    assert!(
        !docs.status().transport.contains(&path_value),
        "the listing must not show a header value"
    );
    responder.abort();
}

/// The HTTP responder's answer to one JSON-RPC message; `None` for a
/// notification.
fn http_reply(msg: &Value) -> Option<Value> {
    let id = msg.get("id")?.clone();
    let result = match msg["method"].as_str() {
        Some("initialize") => json!({
            "protocolVersion": msg["params"]["protocolVersion"],
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "http-fixture", "version": "0" },
        }),
        Some("tools/list") => json!({
            "tools": [{ "name": "echo", "description": "echo", "inputSchema": { "type": "object" } }]
        }),
        Some("tools/call") => json!({ "content": [{ "type": "text", "text": "http echo" }] }),
        _ => {
            return Some(
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "no" } }),
            );
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}
