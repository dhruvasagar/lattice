//! Shared helpers for the LL.4 transport tests: a local TCP mock server, a
//! recording sink and small waiting utilities. No external network.
#![allow(dead_code)]

use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lattice_ai::llm::{
    ApiKey, LlmRequest, Message, OpenAiProvider, Params, Provider, RequestId, RequestInfo,
    RequestRegistry, RequestState, RequestTarget, Sink, SinkFlow, StreamEvent,
};
use lattice_core::BufferId;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub const SSE_HEAD: &str =
    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";

pub const OPENAI_OK_BODY: &str = include_str!("../fixtures/llm/openai_text_reasoning.sse");

/// Read one HTTP request (head and body) and return it as text.
pub async fn read_request(s: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = s.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..pos]).to_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if buf.len() >= pos + 4 + len {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Serve every connection with `f(request_text, stream)`.
pub async fn serve<F, Fut>(f: F) -> SocketAddr
where
    F: Fn(String, TcpStream) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let f = Arc::new(f);
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let f = f.clone();
            tokio::spawn(async move {
                let req = read_request(&mut s).await;
                f(req, s).await;
            });
        }
    });
    addr
}

/// A one-shot canned reply: write `bytes` then close.
pub async fn serve_bytes(bytes: Vec<u8>) -> SocketAddr {
    serve(move |_, mut s| {
        let bytes = bytes.clone();
        async move {
            let _ = s.write_all(&bytes).await;
            let _ = s.shutdown().await;
        }
    })
    .await
}

pub fn provider_for(addr: SocketAddr) -> Arc<dyn Provider> {
    Arc::new(OpenAiProvider::new(format!("http://{addr}/v1")))
}

pub fn request() -> LlmRequest {
    LlmRequest {
        model: "test-model".into(),
        system: None,
        messages: vec![Message::User("hi".into())],
        tools: vec![],
        params: Params::default(),
    }
}

pub fn target() -> RequestTarget {
    RequestTarget {
        buffer: BufferId(1),
        label: "test".into(),
    }
}

/// Records every event; optionally stops after `stop_after` events.
#[derive(Clone, Default)]
pub struct Recorder {
    pub events: Arc<Mutex<Vec<StreamEvent>>>,
    pub threads: Arc<Mutex<Vec<std::thread::ThreadId>>>,
    pub stop_after: Option<usize>,
}

impl Sink for Recorder {
    fn event(&mut self, ev: StreamEvent) -> SinkFlow {
        self.threads
            .lock()
            .unwrap()
            .push(std::thread::current().id());
        let mut g = self.events.lock().unwrap();
        g.push(ev);
        match self.stop_after {
            Some(n) if g.len() >= n => SinkFlow::Stop,
            _ => SinkFlow::Continue,
        }
    }
}

impl Recorder {
    pub fn text(&self) -> String {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }
}

pub fn info(reg: &RequestRegistry, id: RequestId) -> Option<RequestInfo> {
    reg.snapshot().iter().find(|i| i.id == id).cloned()
}

/// Poll until the request is terminal; panics after `limit`.
pub async fn wait_terminal(reg: &RequestRegistry, id: RequestId, limit: Duration) -> RequestState {
    let start = Instant::now();
    loop {
        if let Some(i) = info(reg, id)
            && i.state.is_terminal()
        {
            return i.state;
        }
        assert!(
            start.elapsed() < limit,
            "request {id:?} not terminal within {limit:?}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

pub fn key() -> ApiKey {
    ApiKey::new("sk-test-SECRET-0123456789")
}

pub fn failed_message(state: RequestState) -> String {
    match state {
        RequestState::Failed(m) => m,
        other => panic!("expected Failed, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// LL.16: tool calls on the OpenAI-compatible wire, and a strict reading of
// what comes back.
// ---------------------------------------------------------------------------

fn sse_events(events: &[serde_json::Value]) -> Vec<u8> {
    let mut out = SSE_HEAD.to_string();
    for e in events {
        out.push_str(&format!("data: {e}\n\n"));
    }
    out.push_str("data: [DONE]\n\n");
    out.into_bytes()
}

/// A reply that is one tool call, its arguments sent raw (they may be broken).
pub fn openai_tool_call(id: &str, name: &str, raw_arguments: &str) -> Vec<u8> {
    use serde_json::json;
    sse_events(&[
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":id,"type":"function","function":{"name":name,"arguments":""}}]}}]}),
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":raw_arguments}}]}}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
    ])
}

/// A reply that is plain text.
pub fn openai_text(text: &str) -> Vec<u8> {
    use serde_json::json;
    sse_events(&[
        json!({"choices":[{"index":0,"delta":{"content":text}}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
    ])
}

/// The JSON body of a recorded request.
pub fn body_json(request: &str) -> serde_json::Value {
    let body = request.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    serde_json::from_str(body).unwrap_or(serde_json::Value::Null)
}

/// The tool names a recorded request offered.
pub fn offered_tools(request: &str) -> Vec<String> {
    body_json(request)["tools"]
        .as_array()
        .map(|t| {
            t.iter()
                .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// What a strict OpenAI-compatible server checks before accepting a request:
/// every assistant tool call is answered by a `tool` message before anything
/// else follows, no `tool` message answers a call that was not made, and
/// every call's arguments are a JSON object.
pub fn strictly_valid(request: &str) -> Result<(), String> {
    let body = body_json(request);
    let messages = body["messages"]
        .as_array()
        .ok_or("no messages array")?
        .clone();
    let mut open: Vec<String> = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        let role = m["role"].as_str().unwrap_or("");
        if role == "tool" {
            let id = m["tool_call_id"].as_str().unwrap_or("");
            let Some(pos) = open.iter().position(|o| o == id) else {
                return Err(format!(
                    "message {i}: a result for `{id}`, which no open call made"
                ));
            };
            open.remove(pos);
            continue;
        }
        if !open.is_empty() {
            return Err(format!(
                "message {i} ({role}) follows calls left unanswered: {open:?}"
            ));
        }
        if let Some(calls) = m["tool_calls"].as_array() {
            for c in calls {
                let args = c["function"]["arguments"].as_str().unwrap_or("");
                let parsed: serde_json::Value = serde_json::from_str(args)
                    .map_err(|e| format!("message {i}: arguments `{args}` are not JSON: {e}"))?;
                if !parsed.is_object() {
                    return Err(format!("message {i}: arguments `{args}` are not an object"));
                }
                open.push(c["id"].as_str().unwrap_or("").to_string());
            }
        }
    }
    if open.is_empty() {
        Ok(())
    } else {
        Err(format!("the request ends with calls unanswered: {open:?}"))
    }
}

/// A model that answers each request with the next of `replies` (the last
/// one repeats) and records every request it receives.
pub async fn scripted(replies: Vec<Vec<u8>>) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = log.clone();
    let replies = Arc::new(replies);
    let addr = serve(move |req, mut s| {
        let (seen, replies) = (seen.clone(), replies.clone());
        async move {
            let n = {
                let mut g = seen.lock().unwrap();
                g.push(req);
                g.len()
            };
            let reply = replies[(n - 1).min(replies.len() - 1)].clone();
            let _ = s.write_all(&reply).await;
            let _ = s.shutdown().await;
        }
    })
    .await;
    (addr, log)
}

/// The chat layer over a mock OpenAI-compatible server at `addr`, with
/// `tools` as the global `llm.tools` and `source` as lattice's tools.
pub fn tool_chats(
    addr: SocketAddr,
    tools: &str,
    source: Arc<dyn lattice_ai::llm::tools::ToolSource>,
) -> lattice_ai::llm::chat::LlmChatsHandle {
    use lattice_ai::llm::chat::{LlmChats, spawn_turn_listener};
    use lattice_ai::llm::config::LlmConfig;
    let text = format!(
        "[llm]\ntools = \"{tools}\"\n[llm.providers.mock]\nkind = \"openai-compatible\"\nbase_url = \"http://{addr}/v1\"\nmodel = \"test-model\"\n"
    );
    let config = LlmConfig::from_table(&text.parse::<toml::Table>().unwrap());
    let bus = Arc::new(lattice_runtime::EventBus::new());
    let handle = tokio::runtime::Handle::current();
    let requests = Arc::new(RequestRegistry::new(
        handle.clone(),
        Some(bus.clone()),
        None,
    ));
    let chats = Arc::new(LlmChats::new(handle, requests, bus.clone(), config));
    spawn_turn_listener(chats.clone(), &bus);
    chats
        .tools()
        .add_source(lattice_ai::llm::tools::broker::LATTICE, source);
    chats
}

/// Test tools, under lattice's namespace so they run without asking:
/// `echo {text}` answers `echo: <text>`; `secret` must never run unless
/// offered; `wait` blocks until released (or forever).
#[derive(Default)]
pub struct TestTools {
    pub ran: Mutex<Vec<(String, serde_json::Value)>>,
    pub release: tokio::sync::Notify,
    pub echo_says: Mutex<Option<String>>,
}

impl lattice_ai::llm::tools::ToolSource for TestTools {
    fn list(
        &self,
    ) -> lattice_ai::llm::tools::BoxFuture<
        '_,
        Result<Vec<lattice_ai::llm::tools::SourceTool>, lattice_ai::llm::tools::ToolError>,
    > {
        use lattice_ai::conversation::ToolKind;
        use lattice_ai::llm::ToolDescriptor;
        use lattice_ai::llm::tools::SourceTool;
        let tool = |name: &str, schema| SourceTool {
            descriptor: ToolDescriptor {
                name: name.into(),
                description: format!("test tool {name}"),
                input_schema: schema,
            },
            kind: ToolKind::Read,
        };
        let tools = vec![
            tool(
                "echo",
                serde_json::json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}),
            ),
            tool("secret", serde_json::json!({"type":"object"})),
            tool("wait", serde_json::json!({"type":"object"})),
        ];
        Box::pin(async move { Ok(tools) })
    }

    fn call(
        &self,
        name: &str,
        input: serde_json::Value,
        _ctx: lattice_ai::llm::tools::CallContext,
    ) -> lattice_ai::llm::tools::BoxFuture<
        '_,
        Result<lattice_ai::llm::tools::ToolOutput, lattice_ai::llm::tools::ToolError>,
    > {
        use lattice_ai::llm::tools::{ToolError, ToolOutput};
        let name = name.to_string();
        Box::pin(async move {
            self.ran.lock().unwrap().push((name.clone(), input.clone()));
            match name.as_str() {
                "echo" => {
                    let Some(text) = input["text"].as_str() else {
                        return Err(ToolError::InvalidArguments("missing field `text`".into()));
                    };
                    let said = self.echo_says.lock().unwrap().clone();
                    Ok(ToolOutput {
                        text: said.unwrap_or_else(|| format!("echo: {text}")),
                    })
                }
                "secret" => Ok(ToolOutput {
                    text: "the secret".into(),
                }),
                "wait" => {
                    self.release.notified().await;
                    Ok(ToolOutput {
                        text: "released".into(),
                    })
                }
                other => Err(ToolError::UnknownTool(other.into())),
            }
        })
    }
}

/// Wait until `f` holds, failing after five seconds.
pub async fn until(what: &str, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timed out waiting for: {what}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// The chat's tool blocks: (title, status, output).
pub fn tool_blocks(
    session: &lattice_ai::llm::chat::ChatSession,
) -> Vec<(String, lattice_ai::conversation::ToolStatus, Option<String>)> {
    use lattice_ai::conversation::Block;
    session
        .store
        .snapshot()
        .turns
        .iter()
        .flat_map(|t| t.blocks.iter())
        .filter_map(|b| match b {
            Block::ToolCall {
                title,
                status,
                output,
                ..
            } => Some((title.clone(), *status, output.clone())),
            _ => None,
        })
        .collect()
}
