//! `llm` -- the native LLM layer (LL.3: provider adapters).
//!
//! Wire formats for direct model APIs, with **no I/O**: a [`Provider`] turns an
//! [`LlmRequest`] into a plain [`HttpRequest`] and hands back a
//! [`StreamDecoder`] that turns response bytes into [`StreamEvent`]s. LL.4 adds
//! the transport that connects the two, so every wire detail here is testable
//! from recorded fixtures with no socket.
//!
//! The types are deliberately minimal: just enough for later slices to
//! round-trip text, reasoning (with opaque provider data), tool calls and tool
//! results. Nothing here depends on the `acp` feature.

pub mod agents_md;
pub mod chat;
pub mod commands;
pub mod config;
pub mod context;
pub mod export;
pub mod history;
pub mod install;
pub mod menu;
pub mod modeline;
pub mod modes;
pub mod operators;
pub mod options;
pub mod path_rules;
pub mod prompt_cookie;
pub mod provider;
pub mod request;
pub mod rewrite;
pub mod session_file;
pub mod settings;
pub mod sink;
pub mod tool_loop;
pub mod tools;
pub mod transport;

pub use request::{
    RequestId, RequestInfo, RequestRegistry, RequestRegistryHandle, RequestState,
    RequestStateChanged, RequestTarget, Round, RoundStatus, Rounds, Sink,
};
pub use transport::SinkFlow;

pub use provider::{AnthropicProvider, GeminiProvider, OpenAiProvider, Provider, StreamDecoder};

use serde_json::Value;

/// An API key. `Debug` is redacted so a key cannot reach a log line, a panic
/// message or an error string by formatting a struct that holds one; reading it
/// takes the explicit [`ApiKey::expose`].
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// The secret itself. Call only to place it in a request header.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// True for the empty key local servers (Ollama, llama.cpp) run with.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

/// `url` with nothing secret in it, for whatever shows or logs it: no user or
/// password, each query value written `…` (the names stay, so
/// `?api-version=…&key=…` still says what was sent), no fragment. A key
/// put in a custom `base_url` (`https://user:key@host`, `?key=…`) is the case
/// this is for.
pub fn redacted_url(url: &str) -> String {
    let url = url.split_once('#').map_or(url, |(u, _)| u);
    let (url, query) = match url.split_once('?') {
        Some((u, q)) => (u, Some(q)),
        None => (url, None),
    };
    let mut out = match url.split_once("://") {
        Some((scheme, rest)) => {
            let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
            let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
            format!("{scheme}://{host}{path}")
        }
        None => url.to_string(),
    };
    if let Some(query) = query {
        let names: Vec<String> = query
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|p| match p.split_once('=') {
                Some((name, _)) => format!("{name}=…"),
                None => p.to_string(),
            })
            .collect();
        if !names.is_empty() {
            out.push('?');
            out.push_str(&names.join("&"));
        }
    }
    out
}

/// A fully-built HTTP request, as plain data. The transport (LL.4) executes it.
/// `Debug` names the headers but not their values, which hold the API key, and
/// shows the URL [`redacted_url`].
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl std::fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &redacted_url(&self.url))
            .field(
                "headers",
                &self.headers.iter().map(|(k, _)| k).collect::<Vec<_>>(),
            )
            .field("body_len", &self.body.len())
            .finish()
    }
}

impl HttpRequest {
    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A tool the model may call. MCP's shape: name, description, JSON Schema input.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Reasoning data a provider needs back verbatim on the next turn (Anthropic's
/// thinking `signature`). Opaque to everything but the provider named in
/// `provider`; any other provider omits it when building a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningData {
    pub provider: String,
    pub data: String,
}

/// One block of an assistant message.
#[derive(Debug, Clone, PartialEq)]
pub enum AssistantPart {
    Text(String),
    Reasoning {
        text: String,
        data: Option<ReasoningData>,
    },
    ToolCall {
        id: String,
        name: String,
        input: Value,
    },
}

/// A conversation message as the providers consume it.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    User(String),
    Assistant(Vec<AssistantPart>),
    ToolResult {
        id: String,
        content: String,
        is_error: bool,
    },
}

/// Sampling parameters. `None` leaves the provider default (Anthropic requires
/// `max_tokens`, so its adapter substitutes [`DEFAULT_MAX_TOKENS`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Params {
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

/// Used only where a provider insists on a limit and the caller gave none.
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

#[derive(Debug, Clone, PartialEq)]
pub struct LlmRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDescriptor>,
    pub params: Params,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    ToolUse,
    /// A provider reason with no portable meaning (`refusal`, `content_filter`).
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    /// The request could not be built.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// The provider reported an error in-band on the stream.
    #[error("provider error ({kind}): {message}")]
    Api { kind: String, message: String },
    /// A single SSE line exceeded the decoder's size cap.
    #[error("stream line exceeded the {0}-byte limit")]
    LineTooLong(usize),
    /// The stream ended before the provider said it was finished.
    #[error("stream ended before the response completed")]
    Truncated,
}

/// What a decoder yields, in stream order.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Text(String),
    Reasoning(String),
    /// Opaque round-trip data for the reasoning just streamed. Added to the
    /// brief's variant list: Anthropic's `signature_delta` has no other carrier.
    ReasoningData(ReasoningData),
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallInput {
        id: String,
        json_fragment: String,
    },
    ToolCallEnd {
        id: String,
    },
    /// Cumulative usage so far; a later event supersedes an earlier one.
    Usage(Usage),
    Done(StopReason),
    Error(ProviderError),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key in a custom `base_url` (its user part or a query value) never
    /// reaches anything that shows the URL; where it went still reads.
    #[test]
    fn a_shown_url_keeps_no_secret() {
        assert_eq!(
            redacted_url("https://me:sk-1@gw.example:8443/v1/chat?api-version=2024&key=sk-2#x"),
            "https://gw.example:8443/v1/chat?api-version=…&key=…"
        );
        assert_eq!(
            redacted_url("http://localhost:11434/v1"),
            "http://localhost:11434/v1"
        );
        let http = HttpRequest {
            method: "POST".into(),
            url: "https://u:sk-1@h/v1?key=sk-2".into(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        let shown = format!("{http:?}");
        assert!(
            !shown.contains("sk-1") && !shown.contains("sk-2"),
            "{shown}"
        );
        assert!(!context::show(&http).contains("sk-"), "inspect too");
    }
}
