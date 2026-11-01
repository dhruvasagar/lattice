//! The HTTP transport: executes an [`HttpRequest`], feeds the response body to a
//! [`StreamDecoder`] and reports how it ended.
//!
//! Async on `reqwest` because dropping the future is what makes a cancel on a
//! stalled stream immediate: the select below drops the response (and with it
//! the socket) the instant the token fires, with no blocked read to wait out.
//!
//! The response *status* is checked before any decoding. A non-2xx answer, or a
//! 2xx whose body is not an event stream (a proxy's HTML page, a bare JSON
//! error), becomes a [`Outcome::Failed`] built from the status and a bounded,
//! scrubbed excerpt of the body -- never fed to the SSE decoder, which would
//! silently produce nothing and report a truncated stream.
//!
//! Every message that can leave this module passes through [`scrub`], so an API
//! key cannot reach a user, a log or the registry snapshot through an error.

use std::time::Duration;

use futures::StreamExt;
use reqwest::header::CONTENT_TYPE;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{ApiKey, HttpRequest, ProviderError, StreamDecoder, StreamEvent};

/// How long a connection may take to establish.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a response may stay silent between chunks before it is declared
/// dead. There is deliberately no *total* timeout (a long generation is
/// legitimate), so this is what stops a half-open connection waiting forever.
/// Providers send keep-alive pings well inside it.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// How much of an error body is read at all.
const MAX_ERROR_BODY_BYTES: usize = 16 * 1024;

/// How much of an error body is shown, in characters.
pub const MAX_ERROR_EXCERPT_CHARS: usize = 300;

/// Whether the sink wants more events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SinkFlow {
    Continue,
    Stop,
}

/// How a request ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The provider signalled completion.
    Done,
    /// The sink asked to stop before the provider finished.
    Stopped,
    /// The cancellation token fired.
    Cancelled,
    /// A scrubbed, user-presentable reason.
    Failed(String),
}

/// Build the shared client: a connect timeout and no total timeout.
pub fn build_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|e| format!("could not create the HTTP client: {e}"))
}

/// Remove the key, and anything shaped like an auth header value, from `text`.
///
/// Covers the exact key value (which is also the whole `x-api-key` value) and
/// `Bearer <token>` / `Basic <token>` in any case, because a proxy or provider
/// may echo a header it was sent, or a differently-cased derivative of it.
pub fn scrub(text: &str, key: &ApiKey) -> String {
    let mut out = text.to_string();
    if !key.is_empty() {
        out = out.replace(key.expose(), "[redacted]");
    }
    redact_auth_schemes(&out)
}

fn redact_auth_schemes(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let rest = &lower[i..];
        let scheme = ["bearer ", "basic "]
            .into_iter()
            .find(|s| rest.starts_with(s));
        let at_word_start = i == 0 || !lower.as_bytes()[i - 1].is_ascii_alphanumeric();
        match scheme {
            Some(s) if at_word_start => {
                let after = i + s.len();
                let token_len = text[after..]
                    .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | ','))
                    .unwrap_or(text.len() - after);
                if token_len > 0 {
                    out.push_str(&text[i..after]);
                    out.push_str("[redacted]");
                    i = after + token_len;
                    continue;
                }
                out.push_str(&text[i..after]);
                i = after;
            }
            _ => {
                let ch = text[i..].chars().next().unwrap_or(' ');
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

/// A bounded, scrubbed, single-line excerpt of an error body. Prefers the
/// provider's JSON `message`; otherwise collapses whitespace in the raw text
/// (so an HTML page reads as one line rather than a screenful).
fn excerpt(body: &[u8], key: &ApiKey) -> String {
    let text = String::from_utf8_lossy(body);
    let chosen = json_message(&text).unwrap_or_else(|| text.to_string());
    let one_line = chosen.split_whitespace().collect::<Vec<_>>().join(" ");
    // Scrub BEFORE truncating, so the cut cannot leave a key prefix behind.
    let scrubbed = scrub(&one_line, key);
    if scrubbed.chars().count() > MAX_ERROR_EXCERPT_CHARS {
        let cut: String = scrubbed.chars().take(MAX_ERROR_EXCERPT_CHARS).collect();
        format!("{cut}...")
    } else {
        scrubbed
    }
}

/// Both Anthropic (`{"error":{"type":..,"message":..}}`) and OpenAI-compatible
/// servers (`{"error":{"message":..}}`, sometimes `{"error":"..."}` or a
/// top-level `message`) put the human text in one of these places.
fn json_message(text: &str) -> Option<String> {
    let v: Value = serde_json::from_str(text.trim()).ok()?;
    let found = v
        .pointer("/error/message")
        .or_else(|| v.get("error").filter(|e| e.is_string()))
        .or_else(|| v.get("message"))?;
    found.as_str().map(str::to_string)
}

fn status_label(code: u16) -> &'static str {
    match code {
        400 => "bad request",
        401 => "authentication failed",
        403 => "access denied",
        404 => "not found",
        408 => "request timed out",
        413 => "request too large",
        429 => "rate limited",
        502..=504 => "provider unavailable",
        529 => "provider overloaded",
        _ if (400..500).contains(&code) => "request rejected",
        _ if code >= 500 => "provider error",
        _ => "unexpected response",
    }
}

fn http_failure(code: u16, detail: &str) -> Outcome {
    let label = status_label(code);
    if detail.is_empty() {
        Outcome::Failed(format!("{label} (HTTP {code})"))
    } else {
        Outcome::Failed(format!("{label} (HTTP {code}): {detail}"))
    }
}

/// Read at most [`MAX_ERROR_BODY_BYTES`] of `resp`, giving up on a silent peer.
async fn read_error_body(resp: reqwest::Response, idle: Duration) -> Vec<u8> {
    let mut body = Vec::new();
    let mut stream = resp.bytes_stream();
    while body.len() < MAX_ERROR_BODY_BYTES {
        match tokio::time::timeout(idle, stream.next()).await {
            Ok(Some(Ok(chunk))) => body.extend_from_slice(&chunk),
            _ => break,
        }
    }
    body.truncate(MAX_ERROR_BODY_BYTES);
    body
}

fn is_event_stream(resp: &reqwest::Response) -> bool {
    resp.headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("text/event-stream"))
}

fn event_error_text(err: &ProviderError, key: &ApiKey) -> String {
    scrub(&err.to_string(), key)
}

/// What one exchange needs besides the request itself.
pub struct Exchange<'a> {
    pub client: &'a reqwest::Client,
    pub key: &'a ApiKey,
    pub cancel: &'a CancellationToken,
    /// Max silence between chunks; see [`IDLE_TIMEOUT`].
    pub idle: Duration,
}

/// Execute `http`, decode the stream and hand each batch of events to
/// `on_events` (which reports whether to continue). `on_state_streaming` is
/// called once, when a healthy event-stream response begins.
///
/// Returns when the provider finishes, the sink stops, the token fires, or the
/// request fails. Dropping the response on any exit closes the socket.
pub async fn run(
    ex: Exchange<'_>,
    http: HttpRequest,
    mut decoder: Box<dyn StreamDecoder>,
    mut on_streaming: impl FnMut(),
    mut on_events: impl FnMut(Vec<StreamEvent>) -> SinkFlow,
) -> Outcome {
    let Exchange {
        client,
        key,
        cancel,
        idle,
    } = ex;
    let method = match reqwest::Method::from_bytes(http.method.as_bytes()) {
        Ok(m) => m,
        Err(_) => return Outcome::Failed(format!("invalid HTTP method {:?}", http.method)),
    };
    let mut builder = client.request(method, &http.url).body(http.body);
    for (name, value) in &http.headers {
        builder = builder.header(name, value);
    }

    let sent = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Outcome::Cancelled,
        r = builder.send() => r,
    };
    let resp = match sent {
        Ok(r) => r,
        Err(e) => return Outcome::Failed(scrub(&describe_error(&e), key)),
    };

    let code = resp.status().as_u16();
    if !resp.status().is_success() {
        let body = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Outcome::Cancelled,
            b = read_error_body(resp, idle) => b,
        };
        return http_failure(code, &excerpt(&body, key));
    }
    if !is_event_stream(&resp) {
        let ctype = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| scrub(v, key))
            .unwrap_or_else(|| "none".into());
        let body = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Outcome::Cancelled,
            b = read_error_body(resp, idle) => b,
        };
        let detail = excerpt(&body, key);
        return Outcome::Failed(format!(
            "unexpected response: not an event stream (content-type {ctype}): {detail}"
        ));
    }

    on_streaming();
    let mut stream = resp.bytes_stream();
    loop {
        let next = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Outcome::Cancelled,
            n = tokio::time::timeout(idle, stream.next()) => n,
        };
        let mut eof = false;
        let events = match next {
            Err(_) => {
                return Outcome::Failed(format!(
                    "no data from the provider for {}s; connection abandoned",
                    idle.as_secs()
                ));
            }
            Ok(Some(Err(e))) => {
                return Outcome::Failed(format!(
                    "connection lost mid-stream: {}",
                    scrub(&describe_error(&e), key)
                ));
            }
            Ok(Some(Ok(chunk))) => {
                tracing::debug!(bytes = chunk.len(), "llm chunk");
                decoder.push(&chunk)
            }
            Ok(None) => {
                eof = true;
                decoder.finish()
            }
        };
        let ended = matches!(
            events.last(),
            Some(StreamEvent::Done(_) | StreamEvent::Error(_))
        );
        let failure = events.iter().find_map(|ev| match ev {
            StreamEvent::Error(e) => Some(event_error_text(e, key)),
            _ => None,
        });
        // The sink sees every event, errors included, before the request ends.
        let flow = if events.is_empty() {
            SinkFlow::Continue
        } else {
            on_events(events)
        };
        if let Some(msg) = failure {
            return Outcome::Failed(msg);
        }
        if flow == SinkFlow::Stop {
            return Outcome::Stopped;
        }
        if ended || eof {
            return Outcome::Done;
        }
    }
}

/// `reqwest::Error`'s Display omits the root cause; walk the chain.
fn describe_error(e: &reqwest::Error) -> String {
    use std::error::Error;
    let mut text = if e.is_timeout() {
        "timed out".to_string()
    } else if e.is_connect() {
        "could not connect".to_string()
    } else {
        "request failed".to_string()
    };
    let mut src = e.source();
    while let Some(s) = src {
        text.push_str(": ");
        text.push_str(&s.to_string());
        src = s.source();
    }
    if let Some(url) = e.url() {
        text.push_str(&format!(" ({url})"));
    }
    text
}
