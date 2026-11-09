//! A mock model for the llm tool tests (LL.17, LL.18): an OpenAI-compatible
//! server on 127.0.0.1 whose first reply makes the tool calls given and whose
//! later replies say "done", recording every request body it receives.

#![cfg(test)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

/// Every request body the mock received, in order.
pub(super) type Log = Arc<Mutex<Vec<Value>>>;

fn body_of(s: &mut std::net::TcpStream) -> Option<Value> {
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 65536];
    loop {
        let n = s.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf).to_string();
        if let Some(head_end) = text.find("\r\n\r\n") {
            let len = text[..head_end]
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            if buf.len() >= head_end + 4 + len {
                return serde_json::from_slice(&buf[head_end + 4..head_end + 4 + len]).ok();
            }
        }
    }
}

fn sse(events: &[Value]) -> String {
    let mut out = String::from(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
    );
    for e in events {
        out.push_str(&format!("data: {e}\n\n"));
    }
    out.push_str("data: [DONE]\n\n");
    out
}

/// Serve: the first reply (with any `calls`; a plain "done" without) calls lattice's `calls` (tool name, arguments), in
/// order; every later reply says "done".
pub(super) fn serve(calls: Vec<(&'static str, Value)>) -> (std::net::SocketAddr, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let log: Log = Arc::default();
    let seen = log.clone();
    let calls: Vec<Value> = calls
        .into_iter()
        .enumerate()
        .map(|(i, (name, args))| {
            json!({"index": i, "id": format!("call_{i}"), "type": "function",
                   "function": {"name": format!("lattice__{name}"), "arguments": args.to_string()}})
        })
        .collect();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { continue };
            let (seen, calls) = (seen.clone(), calls.clone());
            std::thread::spawn(move || {
                let n = {
                    let mut g = seen.lock().unwrap();
                    g.push(body_of(&mut s).unwrap_or_default());
                    g.len()
                };
                // No calls: a plain answer from the start.
                let reply = if n == 1 && !calls.is_empty() {
                    sse(&[
                        json!({"choices":[{"index":0,"delta":{"tool_calls":calls}}]}),
                        json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
                    ])
                } else {
                    sse(&[
                        json!({"choices":[{"index":0,"delta":{"content":"done"}}]}),
                        json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
                    ])
                };
                let _ = s.write_all(reply.as_bytes());
            });
        }
    });
    (addr, log)
}

/// Every tool result the second request carried, in order: what the model
/// was told its calls returned.
pub(super) fn results(log: &Log) -> Vec<String> {
    let sent = log.lock().unwrap().clone();
    sent.get(1)
        .and_then(|r| r["messages"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter(|m| m["role"] == "tool")
        .filter_map(|m| m["content"].as_str().map(str::to_string))
        .collect()
}

/// The tool names the first request offered.
pub(super) fn offered(log: &Log) -> Vec<String> {
    let sent = log.lock().unwrap().clone();
    sent.first()
        .and_then(|r| r["tools"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
        .collect()
}
