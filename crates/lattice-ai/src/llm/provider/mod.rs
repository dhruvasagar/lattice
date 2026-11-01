//! Provider adapters: request building and stream decoding, no I/O.

mod anthropic;
mod openai;

pub use anthropic::AnthropicProvider;
pub use openai::OpenAiProvider;

use super::{ApiKey, HttpRequest, LlmRequest, ProviderError, StreamEvent};

pub trait Provider: Send + Sync {
    fn build(&self, req: &LlmRequest, auth: &ApiKey) -> Result<HttpRequest, ProviderError>;
    fn decoder(&self) -> Box<dyn StreamDecoder>;
}

/// Turns response bytes into events. Chunks may split anywhere, including inside
/// a multi-byte character; implementations buffer and never panic on any input.
pub trait StreamDecoder: Send {
    fn push(&mut self, bytes: &[u8]) -> Vec<StreamEvent>;

    /// The transport reached end-of-stream. Flushes a trailing unterminated
    /// event and, when the provider never signalled completion, yields
    /// `Error(Truncated)`. Added to the brief's trait: `push` alone cannot
    /// observe a truncated stream.
    fn finish(&mut self) -> Vec<StreamEvent>;
}

/// One dispatched server-sent event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Incremental SSE parser. Lines end at an ASCII `\n`, which never occurs inside
/// a multi-byte UTF-8 sequence, so decoding whole lines is safe however the
/// network chunked the bytes.
#[derive(Default)]
pub(crate) struct SseParser {
    buf: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
    started: bool,
}

impl SseParser {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut start = 0;
        while let Some(rel) = self.buf[start..].iter().position(|b| *b == b'\n') {
            let end = start + rel;
            let line = self.buf[start..end].to_vec();
            start = end + 1;
            self.line(&line, &mut out);
        }
        self.buf.drain(..start);
        out
    }

    /// End of stream: treat leftover bytes as a final line, then dispatch.
    pub fn flush(&mut self) -> Vec<SseEvent> {
        let mut out = Vec::new();
        if !self.buf.is_empty() {
            let line = std::mem::take(&mut self.buf);
            self.line(&line, &mut out);
        }
        self.line(b"", &mut out);
        out
    }

    fn line(&mut self, raw: &[u8], out: &mut Vec<SseEvent>) {
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let text = String::from_utf8_lossy(raw);
        let mut line: &str = &text;
        if !self.started {
            self.started = true;
            line = line.strip_prefix('\u{feff}').unwrap_or(line);
        }
        if line.is_empty() {
            if !self.data.is_empty() || self.event.is_some() {
                out.push(SseEvent {
                    event: self.event.take(),
                    data: self.data.drain(..).collect::<Vec<_>>().join("\n"),
                });
            }
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => self.data.push(value.to_string()),
            _ => {}
        }
    }
}
