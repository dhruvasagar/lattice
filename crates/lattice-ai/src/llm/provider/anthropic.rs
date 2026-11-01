//! Anthropic Messages API (`POST {base}/v1/messages`, SSE streaming).

use std::collections::HashMap;

use serde_json::{Value, json};

use super::{Provider, SseParser, StreamDecoder};
use crate::llm::{
    ApiKey, AssistantPart, DEFAULT_MAX_TOKENS, HttpRequest, LlmRequest, Message, ProviderError,
    ReasoningData, StopReason, StreamEvent, Usage,
};

const API_VERSION: &str = "2023-06-01";
/// Tag on [`ReasoningData`] this adapter produces and is the only one to accept.
const PROVIDER_TAG: &str = "anthropic";

#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    base_url: String,
}

impl AnthropicProvider {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }
}

impl Default for AnthropicProvider {
    fn default() -> Self {
        Self::new("https://api.anthropic.com")
    }
}

impl Provider for AnthropicProvider {
    fn build(&self, req: &LlmRequest, auth: &ApiKey) -> Result<HttpRequest, ProviderError> {
        let mut body = json!({
            "model": req.model,
            "max_tokens": req.params.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            "stream": true,
            "messages": messages(&req.messages),
        });
        if let Some(system) = &req.system {
            body["system"] = json!(system);
        }
        if let Some(t) = req.params.temperature {
            body["temperature"] = json!(t);
        }
        if !req.tools.is_empty() {
            body["tools"] = req
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.input_schema,
                    })
                })
                .collect();
        }
        let body =
            serde_json::to_vec(&body).map_err(|e| ProviderError::InvalidRequest(e.to_string()))?;
        Ok(HttpRequest {
            method: "POST".into(),
            url: format!("{}/v1/messages", self.base_url),
            headers: vec![
                ("content-type".into(), "application/json".into()),
                ("accept".into(), "text/event-stream".into()),
                ("anthropic-version".into(), API_VERSION.into()),
                ("x-api-key".into(), auth.expose().to_string()),
            ],
            body,
        })
    }

    fn decoder(&self) -> Box<dyn StreamDecoder> {
        Box::new(AnthropicDecoder::default())
    }
}

fn messages(msgs: &[Message]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for m in msgs {
        match m {
            Message::User(text) => out.push(json!({"role": "user", "content": text})),
            Message::Assistant(parts) => {
                let blocks: Vec<Value> = parts.iter().filter_map(assistant_block).collect();
                // The API rejects an assistant message with no content.
                if !blocks.is_empty() {
                    out.push(json!({"role": "assistant", "content": blocks}));
                }
            }
            Message::ToolResult {
                id,
                content,
                is_error,
            } => {
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": id,
                    "content": content,
                    "is_error": is_error,
                });
                // Results for parallel calls must share one user message.
                let merged = out
                    .last_mut()
                    .filter(|l| l["role"] == "user")
                    .and_then(|l| l["content"].as_array_mut())
                    .filter(|c| c.iter().all(|b| b["type"] == "tool_result"))
                    .map(|c| c.push(block.clone()))
                    .is_some();
                if !merged {
                    out.push(json!({"role": "user", "content": [block]}));
                }
            }
        }
    }
    out
}

fn assistant_block(part: &AssistantPart) -> Option<Value> {
    match part {
        AssistantPart::Text(t) if t.is_empty() => None,
        AssistantPart::Text(t) => Some(json!({"type": "text", "text": t})),
        // Thinking blocks are only valid with the signature that came with them;
        // reasoning from another provider (or without one) is omitted.
        AssistantPart::Reasoning {
            text,
            data: Some(d),
        } if d.provider == PROVIDER_TAG => {
            Some(json!({"type": "thinking", "thinking": text, "signature": d.data}))
        }
        AssistantPart::Reasoning { .. } => None,
        AssistantPart::ToolCall { id, name, input } => {
            Some(json!({"type": "tool_use", "id": id, "name": name, "input": input}))
        }
    }
}

#[derive(Default)]
struct AnthropicDecoder {
    sse: SseParser,
    /// Content-block index -> tool-call id, for blocks that are tool calls.
    tools: HashMap<u64, String>,
    usage: Usage,
    stop: Option<StopReason>,
    done: bool,
}

impl StreamDecoder for AnthropicDecoder {
    fn push(&mut self, bytes: &[u8]) -> Vec<StreamEvent> {
        let events = self.sse.push(bytes);
        self.handle(events)
    }

    fn finish(&mut self) -> Vec<StreamEvent> {
        let events = self.sse.flush();
        let mut out = self.handle(events);
        if !self.done {
            self.done = true;
            out.push(StreamEvent::Error(ProviderError::Truncated));
        }
        out
    }
}

impl AnthropicDecoder {
    fn handle(&mut self, events: Vec<super::SseEvent>) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        for ev in events {
            if self.done {
                break;
            }
            let Ok(v) = serde_json::from_str::<Value>(&ev.data) else {
                tracing::debug!("anthropic: skipping non-JSON event data");
                continue;
            };
            let kind = v["type"].as_str().or(ev.event.as_deref()).unwrap_or("");
            self.event(kind, &v, &mut out);
        }
        out
    }

    fn event(&mut self, kind: &str, v: &Value, out: &mut Vec<StreamEvent>) {
        match kind {
            "message_start" => {
                self.merge_usage(&v["message"]["usage"]);
                out.push(StreamEvent::Usage(self.usage.clone()));
            }
            "content_block_start" => {
                let block = &v["content_block"];
                if block["type"] == "tool_use"
                    && let (Some(idx), Some(id), Some(name)) = (
                        v["index"].as_u64(),
                        block["id"].as_str(),
                        block["name"].as_str(),
                    )
                {
                    self.tools.insert(idx, id.to_string());
                    out.push(StreamEvent::ToolCallStart {
                        id: id.into(),
                        name: name.into(),
                    });
                }
            }
            "content_block_delta" => self.delta(v, out),
            "content_block_stop" => {
                if let Some(id) = v["index"].as_u64().and_then(|i| self.tools.remove(&i)) {
                    out.push(StreamEvent::ToolCallEnd { id });
                }
            }
            "message_delta" => {
                if let Some(r) = v["delta"]["stop_reason"].as_str() {
                    self.stop = Some(stop_reason(r));
                }
                self.merge_usage(&v["usage"]);
                out.push(StreamEvent::Usage(self.usage.clone()));
            }
            "message_stop" => {
                self.done = true;
                out.push(StreamEvent::Done(
                    self.stop.take().unwrap_or(StopReason::EndTurn),
                ));
            }
            "error" => {
                self.done = true;
                out.push(StreamEvent::Error(ProviderError::Api {
                    kind: v["error"]["type"].as_str().unwrap_or("unknown").into(),
                    message: v["error"]["message"].as_str().unwrap_or("").into(),
                }));
            }
            // `ping` and event types added later: skipped by design.
            _ => {}
        }
    }

    fn delta(&mut self, v: &Value, out: &mut Vec<StreamEvent>) {
        let d = &v["delta"];
        match d["type"].as_str() {
            Some("text_delta") => {
                if let Some(t) = d["text"].as_str().filter(|t| !t.is_empty()) {
                    out.push(StreamEvent::Text(t.into()));
                }
            }
            Some("thinking_delta") => {
                if let Some(t) = d["thinking"].as_str().filter(|t| !t.is_empty()) {
                    out.push(StreamEvent::Reasoning(t.into()));
                }
            }
            Some("signature_delta") => {
                if let Some(s) = d["signature"].as_str() {
                    out.push(StreamEvent::ReasoningData(ReasoningData {
                        provider: PROVIDER_TAG.into(),
                        data: s.into(),
                    }));
                }
            }
            Some("input_json_delta") => {
                let id = v["index"].as_u64().and_then(|i| self.tools.get(&i));
                if let (Some(id), Some(frag)) = (id, d["partial_json"].as_str())
                    && !frag.is_empty()
                {
                    out.push(StreamEvent::ToolCallInput {
                        id: id.clone(),
                        json_fragment: frag.into(),
                    });
                }
            }
            _ => {}
        }
    }

    fn merge_usage(&mut self, u: &Value) {
        let set = |slot: &mut u64, key: &str| {
            if let Some(n) = u[key].as_u64() {
                *slot = n;
            }
        };
        set(&mut self.usage.input_tokens, "input_tokens");
        set(&mut self.usage.output_tokens, "output_tokens");
        set(&mut self.usage.cache_read_tokens, "cache_read_input_tokens");
        set(
            &mut self.usage.cache_write_tokens,
            "cache_creation_input_tokens",
        );
    }
}

fn stop_reason(r: &str) -> StopReason {
    match r {
        "end_turn" | "stop_sequence" => StopReason::EndTurn,
        "max_tokens" => StopReason::MaxTokens,
        "tool_use" => StopReason::ToolUse,
        other => StopReason::Other(other.into()),
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
                        text: "hmm".into(),
                        data: Some(ReasoningData {
                            provider: "anthropic".into(),
                            data: "SIG".into(),
                        }),
                    },
                    AssistantPart::Reasoning {
                        text: "foreign".into(),
                        data: Some(ReasoningData {
                            provider: "other".into(),
                            data: "X".into(),
                        }),
                    },
                    AssistantPart::ToolCall {
                        id: "t1".into(),
                        name: "read".into(),
                        input: json!({"p": 1}),
                    },
                ]),
                Message::ToolResult {
                    id: "t1".into(),
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
                max_tokens: Some(100),
                temperature: None,
            },
        }
    }

    #[test]
    fn build_emits_exact_body_and_keeps_the_key_in_its_header_only() {
        let http = AnthropicProvider::default()
            .build(&req(), &ApiKey::new("sk-secret-123"))
            .unwrap();
        assert_eq!(http.url, "https://api.anthropic.com/v1/messages");
        assert_eq!(http.header("x-api-key"), Some("sk-secret-123"));
        assert_eq!(http.header("anthropic-version"), Some("2023-06-01"));
        let body: Value = serde_json::from_slice(&http.body).unwrap();
        assert_eq!(
            body,
            json!({
                "model": "test-model",
                "max_tokens": 100,
                "stream": true,
                "system": "be brief",
                "messages": [
                    {"role": "user", "content": "hi"},
                    {"role": "assistant", "content": [
                        {"type": "thinking", "thinking": "hmm", "signature": "SIG"},
                        {"type": "tool_use", "id": "t1", "name": "read", "input": {"p": 1}},
                    ]},
                    {"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": "t1", "content": "ok", "is_error": false},
                    ]},
                ],
                "tools": [{"name": "read", "description": "reads", "input_schema": {"type": "object"}}],
            })
        );
        assert!(!String::from_utf8_lossy(&http.body).contains("sk-secret-123"));
    }

    #[test]
    fn parallel_tool_results_share_one_user_message() {
        let r = |id: &str| Message::ToolResult {
            id: id.into(),
            content: "x".into(),
            is_error: false,
        };
        let out = messages(&[r("a"), r("b")]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["content"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn api_key_debug_is_redacted() {
        assert!(!format!("{:?}", ApiKey::new("sk-secret")).contains("sk-secret"));
    }
}
