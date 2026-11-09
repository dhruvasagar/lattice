//! Google Gemini (`POST {base}/models/{model}:streamGenerateContent?alt=sse`).
//!
//! The key goes in the `x-goog-api-key` header, never in the URL's `key=`
//! parameter the API also accepts: a URL is what proxies and logs keep.
//!
//! # Thought signatures
//!
//! A thinking model signs its reasoning, and the signature rides on a part of
//! the reply, usually its first function call. Newer models reject a function
//! call sent back without the signature it came with, which would brick a
//! tool loop on its second round. The model keeps round-trip data on
//! reasoning blocks (`(turn, block)` → data), so the decoder opens one just
//! before a signed call (empty unless the model also sent thought text; the
//! projection shows nothing for it) and the data names the call it belongs
//! to. Building a request puts each signature back on that call.
//!
//! A signature on a text part is dropped: it is optional there, and keeping it
//! would mean splitting the reply's text around a block of its own.

use std::collections::HashMap;

use serde_json::{Value, json};

use super::{Provider, SseParser, StreamDecoder};
use crate::llm::{
    ApiKey, AssistantPart, HttpRequest, LlmRequest, Message, ProviderError, ReasoningData,
    StopReason, StreamEvent, Usage,
};

/// Tag on [`ReasoningData`] this adapter produces and is the only one to accept.
pub(crate) const PROVIDER_TAG: &str = "gemini";

#[derive(Debug, Clone)]
pub struct GeminiProvider {
    base_url: String,
}

impl GeminiProvider {
    /// `base_url` includes the version segment, e.g.
    /// `https://generativelanguage.googleapis.com/v1beta`.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }
}

impl Default for GeminiProvider {
    fn default() -> Self {
        Self::new("https://generativelanguage.googleapis.com/v1beta")
    }
}

impl Provider for GeminiProvider {
    fn build(&self, req: &LlmRequest, auth: &ApiKey) -> Result<HttpRequest, ProviderError> {
        let mut body = json!({ "contents": contents(&req.messages) });
        if let Some(system) = &req.system {
            body["systemInstruction"] = json!({"parts": [{"text": system}]});
        }
        let mut config = serde_json::Map::new();
        if let Some(n) = req.params.max_tokens {
            config.insert("maxOutputTokens".into(), json!(n));
        }
        if let Some(t) = req.params.temperature {
            config.insert("temperature".into(), json!(t));
        }
        if !config.is_empty() {
            body["generationConfig"] = Value::Object(config);
        }
        if !req.tools.is_empty() {
            // `parametersJsonSchema` takes JSON Schema as tools describe it;
            // `parameters` takes an OpenAPI subset that rejects common keywords.
            let declarations: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "parametersJsonSchema": t.input_schema,
                    })
                })
                .collect();
            body["tools"] = json!([{ "functionDeclarations": declarations }]);
        }
        let body =
            serde_json::to_vec(&body).map_err(|e| ProviderError::InvalidRequest(e.to_string()))?;
        let mut headers = vec![
            ("content-type".to_string(), "application/json".to_string()),
            ("accept".to_string(), "text/event-stream".to_string()),
        ];
        if !auth.is_empty() {
            headers.push(("x-goog-api-key".to_string(), auth.expose().to_string()));
        }
        let model = req.model.strip_prefix("models/").unwrap_or(&req.model);
        Ok(HttpRequest {
            method: "POST".into(),
            url: format!(
                "{}/models/{model}:streamGenerateContent?alt=sse",
                self.base_url
            ),
            headers,
            body,
        })
    }

    fn decoder(&self) -> Box<dyn StreamDecoder> {
        Box::new(GeminiDecoder::default())
    }
}

/// The history as `contents`. A function response names its function, which
/// the history's result does not carry, so names are looked up by call id
/// from the calls before it. Consecutive results go back as one turn, as the
/// calls that asked for them came in one.
fn contents(messages: &[Message]) -> Vec<Value> {
    let mut names: HashMap<&str, &str> = HashMap::new();
    let mut out: Vec<Value> = Vec::new();
    let mut results: Vec<Value> = Vec::new();
    let flush = |results: &mut Vec<Value>, out: &mut Vec<Value>| {
        if !results.is_empty() {
            out.push(json!({"role": "user", "parts": std::mem::take(results)}));
        }
    };
    for m in messages {
        match m {
            Message::User(text) => {
                flush(&mut results, &mut out);
                out.push(json!({"role": "user", "parts": [{"text": text}]}));
            }
            Message::Assistant(parts) => {
                flush(&mut results, &mut out);
                for p in parts {
                    if let AssistantPart::ToolCall { id, name, .. } = p {
                        names.insert(id, name);
                    }
                }
                let parts = model_parts(parts);
                if !parts.is_empty() {
                    out.push(json!({"role": "model", "parts": parts}));
                }
            }
            Message::ToolResult {
                id,
                content,
                is_error,
            } => {
                let key = if *is_error { "error" } else { "content" };
                results.push(json!({"functionResponse": {
                    "id": id,
                    "name": names.get(id.as_str()).copied().unwrap_or(""),
                    "response": { key: content },
                }}));
            }
        }
    }
    flush(&mut results, &mut out);
    out
}

/// An assistant message's parts. Thought text is not sent back (the API does
/// not take it); a signature is, on the call it was given with.
fn model_parts(parts: &[AssistantPart]) -> Vec<Value> {
    let signatures: HashMap<String, String> = parts
        .iter()
        .filter_map(|p| match p {
            AssistantPart::Reasoning { data: Some(d), .. } if d.provider == PROVIDER_TAG => {
                anchor_of(&d.data)
            }
            _ => None,
        })
        .collect();
    parts
        .iter()
        .filter_map(|p| match p {
            AssistantPart::Text(t) if t.is_empty() => None,
            AssistantPart::Text(t) => Some(json!({"text": t})),
            AssistantPart::Reasoning { .. } => None,
            AssistantPart::ToolCall { id, name, input } => {
                let mut part = json!({"functionCall": {"id": id, "name": name, "args": input}});
                if let Some(sig) = signatures.get(id) {
                    part["thoughtSignature"] = json!(sig);
                }
                Some(part)
            }
        })
        .collect()
}

/// The round-trip data for a signature on call `id`.
fn anchor(id: &str, signature: &str) -> String {
    json!({"call": id, "signature": signature}).to_string()
}

/// `(call id, signature)` from [`anchor`]'s text; `None` for anything else.
fn anchor_of(data: &str) -> Option<(String, String)> {
    let v: Value = serde_json::from_str(data).ok()?;
    Some((
        v["call"].as_str()?.to_string(),
        v["signature"].as_str()?.to_string(),
    ))
}

#[derive(Default)]
struct GeminiDecoder {
    sse: SseParser,
    /// Calls the server sent without an id get `gemini_call_<n>`.
    unnamed: u64,
    /// The reply called a tool: its `STOP` means "run them", not "done".
    called: bool,
    finish: Option<String>,
    usage: Option<Usage>,
    done: bool,
}

impl StreamDecoder for GeminiDecoder {
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

    /// Gemini sends no end marker: the stream ends after the chunk that
    /// carries a `finishReason`. Without one, it was cut off.
    fn finish(&mut self) -> Vec<StreamEvent> {
        let events = self.sse.flush();
        let mut out = self.handle(events);
        if !self.done {
            self.done = true;
            match self.finish.take() {
                Some(reason) => {
                    if let Some(u) = self.usage.take() {
                        out.push(StreamEvent::Usage(u));
                    }
                    out.push(StreamEvent::Done(match reason.as_str() {
                        "STOP" if self.called => StopReason::ToolUse,
                        "STOP" => StopReason::EndTurn,
                        "MAX_TOKENS" => StopReason::MaxTokens,
                        other => StopReason::Other(other.to_ascii_lowercase()),
                    }));
                }
                None => out.push(StreamEvent::Error(ProviderError::Truncated)),
            }
        }
        out
    }
}

impl GeminiDecoder {
    fn handle(&mut self, events: Vec<super::SseEvent>) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        for ev in events {
            if self.done {
                break;
            }
            let Ok(v) = serde_json::from_str::<Value>(&ev.data) else {
                tracing::debug!("gemini: skipping non-JSON event data");
                continue;
            };
            self.chunk(&v, &mut out);
        }
        out
    }

    fn chunk(&mut self, v: &Value, out: &mut Vec<StreamEvent>) {
        if v["error"].is_object() {
            self.done = true;
            let e = &v["error"];
            let kind = match (e["status"].as_str(), e["code"].as_u64()) {
                (Some(s), _) => s.to_string(),
                (None, Some(c)) => c.to_string(),
                (None, None) => "unknown".to_string(),
            };
            out.push(StreamEvent::Error(ProviderError::Api {
                kind,
                message: e["message"].as_str().unwrap_or("").into(),
            }));
            return;
        }
        if let Some(reason) = v["promptFeedback"]["blockReason"].as_str() {
            self.done = true;
            out.push(StreamEvent::Error(ProviderError::Api {
                kind: "blocked".into(),
                message: format!("the prompt was blocked ({reason})"),
            }));
            return;
        }
        if v["usageMetadata"].is_object() {
            let u = &v["usageMetadata"];
            let n = |k: &str| u[k].as_u64().unwrap_or(0);
            self.usage = Some(Usage {
                input_tokens: n("promptTokenCount"),
                // Thinking is billed as output.
                output_tokens: n("candidatesTokenCount") + n("thoughtsTokenCount"),
                cache_read_tokens: n("cachedContentTokenCount"),
                cache_write_tokens: 0,
            });
        }
        let candidate = &v["candidates"][0];
        if let Some(parts) = candidate["content"]["parts"].as_array() {
            for p in parts {
                self.part(p, out);
            }
        }
        if let Some(r) = candidate["finishReason"].as_str() {
            self.finish = Some(r.to_string());
        }
    }

    fn part(&mut self, p: &Value, out: &mut Vec<StreamEvent>) {
        let call = &p["functionCall"];
        if call.is_object() {
            let id = match call["id"].as_str().filter(|i| !i.is_empty()) {
                Some(id) => id.to_string(),
                None => {
                    self.unnamed += 1;
                    format!("gemini_call_{}", self.unnamed)
                }
            };
            if let Some(sig) = p["thoughtSignature"].as_str().filter(|s| !s.is_empty()) {
                // The block the signature is kept on; see the module docs.
                out.push(StreamEvent::Reasoning(String::new()));
                out.push(StreamEvent::ReasoningData(ReasoningData {
                    provider: PROVIDER_TAG.into(),
                    data: anchor(&id, sig),
                }));
            }
            let args = match &call["args"] {
                a @ Value::Object(_) => a.to_string(),
                _ => "{}".to_string(),
            };
            out.push(StreamEvent::ToolCallStart {
                id: id.clone(),
                name: call["name"].as_str().unwrap_or("").to_string(),
            });
            out.push(StreamEvent::ToolCallInput {
                id: id.clone(),
                json_fragment: args,
            });
            out.push(StreamEvent::ToolCallEnd { id });
            self.called = true;
            return;
        }
        if let Some(t) = p["text"].as_str().filter(|t| !t.is_empty()) {
            if p["thought"].as_bool() == Some(true) {
                out.push(StreamEvent::Reasoning(t.into()));
            } else {
                out.push(StreamEvent::Text(t.into()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::llm::{Params, ToolDescriptor};

    fn req() -> LlmRequest {
        LlmRequest {
            model: "models/gemini-2.5-flash".into(),
            system: Some("be brief".into()),
            messages: vec![
                Message::User("hi".into()),
                Message::Assistant(vec![
                    AssistantPart::Reasoning {
                        text: "thinking".into(),
                        data: Some(ReasoningData {
                            provider: PROVIDER_TAG.into(),
                            data: anchor("c1", "sig-1"),
                        }),
                    },
                    AssistantPart::Text("let me look".into()),
                    AssistantPart::ToolCall {
                        id: "c1".into(),
                        name: "read".into(),
                        input: json!({"p": 1}),
                    },
                    AssistantPart::ToolCall {
                        id: "c2".into(),
                        name: "list".into(),
                        input: json!({}),
                    },
                ]),
                Message::ToolResult {
                    id: "c1".into(),
                    content: "ok".into(),
                    is_error: false,
                },
                Message::ToolResult {
                    id: "c2".into(),
                    content: "no such dir".into(),
                    is_error: true,
                },
            ],
            tools: vec![ToolDescriptor {
                name: "read".into(),
                description: "reads".into(),
                input_schema: json!({"type": "object", "additionalProperties": false}),
            }],
            params: Params {
                max_tokens: Some(50),
                temperature: Some(0.5),
            },
        }
    }

    #[test]
    fn build_emits_exact_body_and_keeps_the_key_in_its_header_only() {
        let http = GeminiProvider::new("https://g.example/v1beta/")
            .build(&req(), &ApiKey::new("AIza-secret-123"))
            .unwrap();
        assert_eq!(
            http.url,
            "https://g.example/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );
        assert_eq!(http.header("x-goog-api-key"), Some("AIza-secret-123"));
        assert!(!http.url.contains("secret"), "never in the URL");
        let body: Value = serde_json::from_slice(&http.body).unwrap();
        assert_eq!(
            body,
            json!({
                "systemInstruction": {"parts": [{"text": "be brief"}]},
                "generationConfig": {"maxOutputTokens": 50, "temperature": 0.5},
                "contents": [
                    {"role": "user", "parts": [{"text": "hi"}]},
                    {"role": "model", "parts": [
                        {"text": "let me look"},
                        {"functionCall": {"id": "c1", "name": "read", "args": {"p": 1}},
                         "thoughtSignature": "sig-1"},
                        {"functionCall": {"id": "c2", "name": "list", "args": {}}},
                    ]},
                    {"role": "user", "parts": [
                        {"functionResponse": {"id": "c1", "name": "read", "response": {"content": "ok"}}},
                        {"functionResponse": {"id": "c2", "name": "list", "response": {"error": "no such dir"}}},
                    ]},
                ],
                "tools": [{"functionDeclarations": [{
                    "name": "read", "description": "reads",
                    "parametersJsonSchema": {"type": "object", "additionalProperties": false}}]}],
            })
        );
        assert!(!String::from_utf8_lossy(&http.body).contains("AIza-secret-123"));
    }

    #[test]
    fn another_providers_reasoning_is_not_sent() {
        let mut r = req();
        r.messages[1] = Message::Assistant(vec![
            AssistantPart::Reasoning {
                text: "t".into(),
                data: Some(ReasoningData {
                    provider: "anthropic".into(),
                    data: "sig".into(),
                }),
            },
            AssistantPart::ToolCall {
                id: "c1".into(),
                name: "read".into(),
                input: json!({}),
            },
        ]);
        let http = GeminiProvider::default()
            .build(&r, &ApiKey::new(""))
            .unwrap();
        let text = String::from_utf8_lossy(&http.body);
        assert!(
            !text.contains("thoughtSignature") && !text.contains("sig"),
            "{text}"
        );
        assert!(http.header("x-goog-api-key").is_none(), "no key, no header");
    }
}
