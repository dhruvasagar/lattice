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
