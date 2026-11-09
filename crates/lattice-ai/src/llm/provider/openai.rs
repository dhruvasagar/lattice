//! OpenAI-compatible Chat Completions (`POST {base}/chat/completions`, SSE).
//!
//! The base URL is per instance so one adapter serves OpenAI, Ollama,
//! llama.cpp, vLLM and the like. Local servers run with an empty key, in which
//! case no `Authorization` header is sent.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{Provider, SseParser, StreamDecoder};
use crate::llm::{
    ApiKey, AssistantPart, HttpRequest, LlmRequest, Message, ProviderError, StopReason,
    StreamEvent, Usage,
};

#[derive(Debug, Clone)]
pub struct OpenAiProvider {
    base_url: String,
    /// JSON key for the output limit: `max_completion_tokens` for OpenAI proper
    /// (newer models reject `max_tokens`), `max_tokens` for compatible servers.
    max_tokens_key: String,
}

impl OpenAiProvider {
    /// `base_url` includes any version segment, e.g. `https://api.openai.com/v1`
    /// or `http://localhost:11434/v1`.
    pub fn new(base_url: impl Into<String>) -> Self {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let key = if host_of(&base_url) == Some("api.openai.com") {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        Self {
            base_url,
            max_tokens_key: key.to_string(),
        }
    }

    /// Override the output-limit key for a server that wants the other one.
    pub fn with_max_tokens_key(mut self, key: impl Into<String>) -> Self {
        self.max_tokens_key = key.into();
        self
    }
}

impl Provider for OpenAiProvider {
    fn build(&self, req: &LlmRequest, auth: &ApiKey) -> Result<HttpRequest, ProviderError> {
        let mut messages = Vec::new();
        if let Some(system) = &req.system {
            messages.push(json!({"role": "system", "content": system}));
        }
        messages.extend(req.messages.iter().map(message));
        let mut body = json!({
            "model": req.model,
            "messages": messages,
            "stream": true,
            "stream_options": {"include_usage": true},
        });
        if let Some(n) = req.params.max_tokens {
            body[self.max_tokens_key.as_str()] = json!(n);
        }
        if let Some(t) = req.params.temperature {
            body["temperature"] = json!(t);
        }
        if !req.tools.is_empty() {
            body["tools"] = req
                .tools
                .iter()
                .map(|t| {
                    json!({"type": "function", "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    }})
                })
                .collect();
        }
        let body =
            serde_json::to_vec(&body).map_err(|e| ProviderError::InvalidRequest(e.to_string()))?;
        let mut headers = vec![
            ("content-type".to_string(), "application/json".to_string()),
            ("accept".to_string(), "text/event-stream".to_string()),
        ];
        if !auth.is_empty() {
            headers.push((
                "authorization".to_string(),
                format!("Bearer {}", auth.expose()),
            ));
        }
        Ok(HttpRequest {
            method: "POST".into(),
            url: format!("{}/chat/completions", self.base_url),
            headers,
            body,
        })
    }

    fn decoder(&self) -> Box<dyn StreamDecoder> {
        Box::new(OpenAiDecoder::default())
    }
}

fn host_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    authority.split(':').next()
}

/// Reasoning is never sent back: the wire format has no slot for it that
/// servers agree on, so it is omitted (and any provider data with it).
fn message(m: &Message) -> Value {
    match m {
        Message::User(t) => json!({"role": "user", "content": t}),
        Message::Assistant(parts) => {
            let text: String = parts
                .iter()
                .filter_map(|p| match p {
                    AssistantPart::Text(t) => Some(t.as_str()),
                    _ => None,
                })
                .collect();
            let calls: Vec<Value> = parts
                .iter()
                .filter_map(|p| match p {
                    AssistantPart::ToolCall { id, name, input } => Some(json!({
                        "id": id,
                        "type": "function",
                        "function": {"name": name, "arguments": input.to_string()},
                    })),
                    _ => None,
                })
                .collect();
            // `null` content is valid only beside tool calls; a turn that was
            // all reasoning sends an empty string, which servers accept.
            let content = match (text.is_empty(), calls.is_empty()) {
                (true, false) => Value::Null,
                _ => json!(text),
            };
            let mut v = json!({"role": "assistant", "content": content});
            if !calls.is_empty() {
                v["tool_calls"] = Value::Array(calls);
            }
            v
        }
        Message::ToolResult { id, content, .. } => {
            json!({"role": "tool", "tool_call_id": id, "content": content})
        }
    }
}

#[derive(Default)]
struct OpenAiDecoder {
    sse: SseParser,
    /// Tool-call index -> id, in index order, for calls not yet ended.
    open: BTreeMap<u64, String>,
    /// Counter and latest key for tool calls the server sent without `index`.
    anon: u64,
    last_anon: Option<u64>,
    finish: Option<StopReason>,
    /// A tool call arrived this turn: some servers finish it with `stop`.
    saw_call: bool,
    done: bool,
}

/// Keys for index-less calls sit above any real wire index.
const ANON_BASE: u64 = 1 << 32;

impl StreamDecoder for OpenAiDecoder {
    fn push(&mut self, bytes: &[u8]) -> Vec<StreamEvent> {
        let events = self.sse.push(bytes);
        let mut out = self.handle(events);
        if self.sse.overflowed() && !self.done {
            self.done = true;
            out.push(StreamEvent::Error(ProviderError::LineTooLong(
                super::MAX_LINE_BYTES,
            )));
        }
        out
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        let events = self.sse.flush();
        let mut out = self.handle(events);
        if !self.done {
            self.done = true;
            // Some local servers close without `[DONE]` after a finish_reason.
            out.push(match self.finish.take() {
                Some(r) => StreamEvent::Done(r),
                None => StreamEvent::Error(ProviderError::Truncated),
            });
        }
        out
    }
}

impl OpenAiDecoder {
    fn handle(&mut self, events: Vec<super::SseEvent>) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        for ev in events {
            if self.done {
                break;
            }
            if ev.data.trim() == "[DONE]" {
                self.done = true;
                self.end_calls(&mut out);
                // A `[DONE]` with no finish_reason after calls still runs them.
                let unsaid = if self.saw_call {
                    StopReason::ToolUse
                } else {
                    StopReason::EndTurn
                };
                out.push(StreamEvent::Done(self.finish.take().unwrap_or(unsaid)));
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&ev.data) else {
                tracing::debug!("openai: skipping non-JSON event data");
                continue;
            };
            self.chunk(&v, &mut out);
        }
        out
    }

    fn chunk(&mut self, v: &Value, out: &mut Vec<StreamEvent>) {
        if v["error"].is_object() {
            self.done = true;
            out.push(StreamEvent::Error(ProviderError::Api {
                kind: v["error"]["type"]
                    .as_str()
                    .or(v["error"]["code"].as_str())
                    .unwrap_or("unknown")
                    .into(),
                message: v["error"]["message"].as_str().unwrap_or("").into(),
            }));
            return;
        }
        let choice = &v["choices"][0];
        let delta = &choice["delta"];
        if let Some(t) = delta["reasoning_content"]
            .as_str()
            .filter(|t| !t.is_empty())
        {
            out.push(StreamEvent::Reasoning(t.into()));
        }
        if let Some(t) = delta["content"].as_str().filter(|t| !t.is_empty()) {
            out.push(StreamEvent::Text(t.into()));
        }
        if let Some(calls) = delta["tool_calls"].as_array() {
            for c in calls {
                self.tool_call(c, out);
            }
        }
        if let Some(r) = choice["finish_reason"].as_str() {
            self.finish = Some(match r {
                "stop" if self.saw_call => StopReason::ToolUse,
                "stop" => StopReason::EndTurn,
                "length" => StopReason::MaxTokens,
                "tool_calls" | "function_call" => StopReason::ToolUse,
                other => StopReason::Other(other.into()),
            });
            self.end_calls(out);
        }
        if v["usage"].is_object() {
            let u = &v["usage"];
            out.push(StreamEvent::Usage(Usage {
                input_tokens: u["prompt_tokens"].as_u64().unwrap_or(0),
                output_tokens: u["completion_tokens"].as_u64().unwrap_or(0),
                cache_read_tokens: u["prompt_tokens_details"]["cached_tokens"]
                    .as_u64()
                    .unwrap_or(0),
                cache_write_tokens: 0,
            }));
        }
    }

    fn tool_call(&mut self, c: &Value, out: &mut Vec<StreamEvent>) {
        let wire_id = c["id"].as_str().filter(|i| !i.is_empty());
        let key = match c["index"].as_u64() {
            Some(i) => i,
            // Servers that omit `index`: a new id opens a new call; no id
            // continues the latest one.
            None => match wire_id {
                Some(id) => self
                    .open
                    .iter()
                    .find(|(_, open_id)| open_id.as_str() == id)
                    .map(|(k, _)| *k)
                    .unwrap_or_else(|| {
                        self.anon += 1;
                        ANON_BASE + self.anon
                    }),
                None => self.last_anon.unwrap_or(0),
            },
        };
        if c["index"].is_null() {
            self.last_anon = Some(key);
        }
        if let std::collections::btree_map::Entry::Vacant(slot) = self.open.entry(key) {
            let id = wire_id
                .map(String::from)
                .unwrap_or_else(|| format!("call_{key}"));
            let name = c["function"]["name"].as_str().unwrap_or("").to_string();
            slot.insert(id.clone());
            self.saw_call = true;
            out.push(StreamEvent::ToolCallStart { id, name });
        }
        if let (Some(id), Some(frag)) = (
            self.open.get(&key),
            c["function"]["arguments"]
                .as_str()
                .filter(|a| !a.is_empty()),
        ) {
            out.push(StreamEvent::ToolCallInput {
                id: id.clone(),
                json_fragment: frag.into(),
            });
        }
    }

    fn end_calls(&mut self, out: &mut Vec<StreamEvent>) {
        out.extend(
            std::mem::take(&mut self.open)
                .into_values()
                .map(|id| StreamEvent::ToolCallEnd { id }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{Params, ToolDescriptor};

    fn req() -> LlmRequest {
        LlmRequest {
            model: "test-model".into(),
            system: Some("be brief".into()),
            messages: vec![
                Message::User("hi".into()),
                Message::Assistant(vec![
                    AssistantPart::Reasoning {
                        text: "dropped".into(),
                        data: None,
                    },
                    AssistantPart::ToolCall {
                        id: "c1".into(),
                        name: "read".into(),
                        input: json!({"p": 1}),
                    },
                ]),
                Message::ToolResult {
                    id: "c1".into(),
                    content: "ok".into(),
                    is_error: false,
                },
            ],
            tools: vec![ToolDescriptor {
                name: "read".into(),
                description: "reads".into(),
                input_schema: json!({"type": "object"}),
            }],
            params: Params {
                max_tokens: Some(50),
                temperature: Some(0.5),
            },
        }
    }

    #[test]
    fn build_emits_exact_body_and_keeps_the_key_in_its_header_only() {
        let http = OpenAiProvider::new("http://localhost:1234/v1/")
            .build(&req(), &ApiKey::new("sk-secret-123"))
            .unwrap();
        assert_eq!(http.url, "http://localhost:1234/v1/chat/completions");
        assert_eq!(http.header("authorization"), Some("Bearer sk-secret-123"));
        let body: Value = serde_json::from_slice(&http.body).unwrap();
        assert_eq!(
            body,
            json!({
                "model": "test-model",
                "stream": true,
                "stream_options": {"include_usage": true},
                "max_tokens": 50,
                "temperature": 0.5,
                "messages": [
                    {"role": "system", "content": "be brief"},
                    {"role": "user", "content": "hi"},
                    {"role": "assistant", "content": null, "tool_calls": [
                        {"id": "c1", "type": "function", "function": {"name": "read", "arguments": "{\"p\":1}"}},
                    ]},
                    {"role": "tool", "tool_call_id": "c1", "content": "ok"},
                ],
                "tools": [{"type": "function", "function": {
                    "name": "read", "description": "reads", "parameters": {"type": "object"}}}],
            })
        );
        assert!(!String::from_utf8_lossy(&http.body).contains("sk-secret-123"));
        assert!(!format!("{http:?}").contains("sk-secret-123"));
    }

    #[test]
    fn a_turn_of_only_reasoning_goes_back_with_empty_content() {
        let m = Message::Assistant(vec![AssistantPart::Reasoning {
            text: "thinking".into(),
            data: None,
        }]);
        assert_eq!(message(&m), json!({"role": "assistant", "content": ""}));
    }

    #[test]
    fn output_limit_key_follows_the_host() {
        let key = |base: &str| {
            let http = OpenAiProvider::new(base)
                .build(&req(), &ApiKey::new("k"))
                .unwrap();
            let body: Value = serde_json::from_slice(&http.body).unwrap();
            (
                body.get("max_completion_tokens").is_some(),
                body.get("max_tokens").is_some(),
            )
        };
        assert_eq!(key("https://api.openai.com/v1"), (true, false));
        assert_eq!(key("http://localhost:11434/v1"), (false, true));
        assert_eq!(key("https://api.openai.com.evil.test/v1"), (false, true));
    }

    #[test]
    fn empty_key_sends_no_authorization_header() {
        let http = OpenAiProvider::new("http://localhost:11434/v1")
            .build(&req(), &ApiKey::new(""))
            .unwrap();
        assert_eq!(http.header("authorization"), None);
    }
}
