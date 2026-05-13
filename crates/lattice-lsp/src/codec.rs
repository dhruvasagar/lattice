//! Tokio-async codec gluing [`framing`] + [`jsonrpc`] onto
//! `AsyncBufRead` / `AsyncWrite`. One `read_message` /
//! `write_message` per LSP message.
//!
//! [`framing`]: crate::framing
//! [`jsonrpc`]: crate::jsonrpc
//!
//! ## Read protocol (per message)
//!
//! 1. Read header lines (CRLF-terminated, ASCII) until the empty
//!    `\r\n` terminator.
//! 2. Parse header block via [`crate::framing::parse_header_block`].
//! 3. Read exactly `content_length` body bytes.
//! 4. Decode body as one [`crate::jsonrpc::Message`].
//!
//! ## Write protocol (per message)
//!
//! 1. JSON-encode the body (one allocation).
//! 2. Write `Content-Length: N\r\n\r\n` header.
//! 3. Write the body. Flush.
//!
//! ## Cancellation safety
//!
//! Both `read_message` and `write_message` are cancel-safe at
//! `.await` points only when the caller holds an exclusive
//! reference. We don't promise mid-message resumption -- the
//! actor wraps these in a single-task loop with no concurrent
//! readers.

use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use crate::framing::{self, FrameError, FrameHeader};
use crate::jsonrpc::{Message, MessageDecodeError};

/// One-message-at-a-time async reader.
///
/// Wraps any `AsyncBufRead`. Construct via
/// [`LspReader::new`] from an `AsyncRead` (we wrap in `BufReader`
/// internally) or use [`LspReader::from_buf_reader`] when the
/// caller already has buffering.
pub struct LspReader<R> {
    inner: R,
    /// Reused header-line scratch -- avoids one allocation per
    /// line, which adds up at high message rates (e.g. semantic
    /// tokens stream during a fast scroll).
    line_buf: Vec<u8>,
    /// Reused header-block scratch.
    header_buf: Vec<u8>,
    /// Reused body scratch. Sized up to the largest message seen
    /// so far; bounded by [`framing::MAX_MESSAGE_BYTES`].
    body_buf: Vec<u8>,
}

impl<R: AsyncRead + Unpin> LspReader<BufReader<R>> {
    /// Wrap an unbuffered `AsyncRead` in a `BufReader`. The 8 KiB
    /// default buffer is enough for header blocks; bodies are
    /// read with `read_exact` straight into our scratch.
    pub fn new(inner: R) -> Self {
        Self::from_buf_reader(BufReader::new(inner))
    }
}

impl<R: AsyncBufRead + Unpin> LspReader<R> {
    /// Wrap an already-buffered reader. Use this when the caller
    /// has tuned the buffer size or shares one across multiple
    /// streams.
    pub fn from_buf_reader(inner: R) -> Self {
        Self {
            inner,
            line_buf: Vec::with_capacity(64),
            header_buf: Vec::with_capacity(128),
            body_buf: Vec::with_capacity(1024),
        }
    }

    /// Read one complete LSP message. Resolves to:
    /// - `Ok(Some(msg))` -- a message arrived.
    /// - `Ok(None)` -- the stream closed cleanly between
    ///   messages (graceful shutdown, server exited).
    /// - `Err(_)` -- mid-message I/O error or framing/decode
    ///   failure. The caller should tear the transport down.
    pub async fn read_message(&mut self) -> Result<Option<Message>, CodecError> {
        // Read header lines until empty terminator. The
        // first read also detects clean EOF.
        self.header_buf.clear();
        let mut got_any = false;
        loop {
            self.line_buf.clear();
            let n = self
                .inner
                .read_until(b'\n', &mut self.line_buf)
                .await
                .map_err(CodecError::Io)?;
            if n == 0 {
                // EOF.
                if !got_any {
                    return Ok(None);
                }
                return Err(CodecError::UnexpectedEof);
            }
            got_any = true;

            // Strip trailing `\r\n` (preferred) or just `\n`.
            let line = strip_crlf(&self.line_buf);

            if line.is_empty() {
                // Header terminator.
                break;
            }
            if !self.header_buf.is_empty() {
                self.header_buf.extend_from_slice(b"\r\n");
            }
            self.header_buf.extend_from_slice(line);
        }

        let header: FrameHeader =
            framing::parse_header_block(&self.header_buf).map_err(CodecError::Frame)?;

        let len = header.content_length as usize;
        self.body_buf.clear();
        self.body_buf.resize(len, 0);
        tokio::io::AsyncReadExt::read_exact(&mut self.inner, &mut self.body_buf)
            .await
            .map_err(CodecError::Io)?;

        let msg = Message::from_json(&self.body_buf).map_err(CodecError::Decode)?;
        Ok(Some(msg))
    }
}

/// One-message-at-a-time async writer.
///
/// Wraps any `AsyncWrite`. The actor wraps stdin of the child
/// process; tests wrap a `Vec<u8>` or duplex pipe.
pub struct LspWriter<W> {
    inner: W,
    /// Reused encode scratch. JSON serialize → here → wire.
    encode_buf: Vec<u8>,
}

impl<W: AsyncWrite + Unpin> LspWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            encode_buf: Vec::with_capacity(1024),
        }
    }

    /// Encode `msg` as JSON, prepend the framing header, write
    /// header + body, and flush. One `flush` per message keeps
    /// latency bounded -- LSP servers expect timely delivery of
    /// `didChange` etc.
    pub async fn write_message(&mut self, msg: &Message) -> Result<(), CodecError> {
        self.encode_buf.clear();
        let body = msg
            .to_json()
            .map_err(|e| CodecError::Decode(MessageDecodeError::Json(e)))?;
        let header = framing::encode_header(body.len());
        // Issue header + body as separate write_all calls; tokio
        // coalesces small writes via the underlying buffered
        // stream. Avoiding the intermediate concat keeps the
        // hot-path allocation count at one (the body Vec from
        // serde_json).
        self.inner
            .write_all(header.as_bytes())
            .await
            .map_err(CodecError::Io)?;
        self.inner.write_all(&body).await.map_err(CodecError::Io)?;
        self.inner.flush().await.map_err(CodecError::Io)?;
        Ok(())
    }

    /// Borrow the inner writer. Used by the transport to close
    /// stdin on shutdown.
    pub fn inner_mut(&mut self) -> &mut W {
        &mut self.inner
    }
}

/// Combined error surface for the codec layer.
#[derive(Debug, Error)]
pub enum CodecError {
    /// Underlying I/O failed (server died, pipe closed, etc.).
    #[error("io error: {0}")]
    Io(#[source] std::io::Error),
    /// Stream EOF mid-message. Distinguishable from clean EOF
    /// (which returns `Ok(None)` from `read_message`).
    #[error("unexpected EOF mid-message")]
    UnexpectedEof,
    /// Header block was ill-formed.
    #[error("framing: {0}")]
    Frame(#[source] FrameError),
    /// JSON body decoded but didn't match a JSON-RPC message
    /// shape.
    #[error("decode: {0}")]
    Decode(#[source] MessageDecodeError),
}

fn strip_crlf(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    if end > 0 && line[end - 1] == b'\n' {
        end -= 1;
    }
    if end > 0 && line[end - 1] == b'\r' {
        end -= 1;
    }
    &line[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jsonrpc::{Notification, Request, RequestId, Response};
    use serde_json::json;
    use tokio::io::duplex;

    /// Encode one LSP frame for tests.
    fn frame_one(body: &[u8]) -> Vec<u8> {
        let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        out.extend_from_slice(body);
        out
    }

    #[tokio::test]
    async fn reads_one_request() {
        let body = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let stream = frame_one(body);
        let mut r = LspReader::new(&stream[..]);
        let msg = r.read_message().await.unwrap().unwrap();
        match msg {
            Message::Request(req) => {
                assert_eq!(req.method, "initialize");
                assert_eq!(req.id, RequestId::Number(1));
            }
            _ => panic!("expected request"),
        }
    }

    #[tokio::test]
    async fn reads_back_to_back_messages() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&frame_one(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
        ));
        stream.extend_from_slice(&frame_one(
            br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
        ));
        stream.extend_from_slice(&frame_one(
            br#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#,
        ));
        let mut r = LspReader::new(&stream[..]);
        // Three messages, one EOF.
        assert!(matches!(
            r.read_message().await.unwrap().unwrap(),
            Message::Request(_)
        ));
        assert!(matches!(
            r.read_message().await.unwrap().unwrap(),
            Message::Notification(_)
        ));
        assert!(matches!(
            r.read_message().await.unwrap().unwrap(),
            Message::Request(_)
        ));
        assert!(r.read_message().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn clean_eof_before_first_message_returns_none() {
        let mut r = LspReader::new(&[][..]);
        assert!(r.read_message().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn eof_mid_header_is_error() {
        let stream = b"Content-Length: 5\r\n";
        let mut r = LspReader::new(&stream[..]);
        let err = r.read_message().await.unwrap_err();
        assert!(matches!(err, CodecError::UnexpectedEof));
    }

    #[tokio::test]
    async fn eof_mid_body_is_error() {
        // Header says 100 bytes, only 5 follow.
        let mut stream = b"Content-Length: 100\r\n\r\n".to_vec();
        stream.extend_from_slice(b"hello");
        let mut r = LspReader::new(&stream[..]);
        let err = r.read_message().await.unwrap_err();
        assert!(matches!(err, CodecError::Io(_)));
    }

    #[tokio::test]
    async fn malformed_header_is_error() {
        let stream = b"Content-Length: not-a-number\r\n\r\n";
        let mut r = LspReader::new(&stream[..]);
        let err = r.read_message().await.unwrap_err();
        assert!(matches!(err, CodecError::Frame(_)));
    }

    #[tokio::test]
    async fn invalid_json_body_is_error() {
        let body = b"{ not json";
        let stream = frame_one(body);
        let mut r = LspReader::new(&stream[..]);
        let err = r.read_message().await.unwrap_err();
        assert!(matches!(err, CodecError::Decode(_)));
    }

    #[tokio::test]
    async fn write_message_produces_valid_frame() {
        let mut buf = Vec::new();
        {
            let mut w = LspWriter::new(&mut buf);
            let req = Message::Request(Request::new(
                RequestId::from_u64(5),
                "textDocument/hover",
                Some(json!({"textDocument": {"uri": "file:///a.rs"}})),
            ));
            w.write_message(&req).await.unwrap();
        }
        // The output must be parseable by our own reader.
        let mut r = LspReader::new(&buf[..]);
        let msg = r.read_message().await.unwrap().unwrap();
        match msg {
            Message::Request(req) => {
                assert_eq!(req.method, "textDocument/hover");
                assert_eq!(req.id, RequestId::Number(5));
            }
            _ => panic!("expected request"),
        }
    }

    #[tokio::test]
    async fn write_then_read_via_duplex_pipe() {
        // Models the actor's bidirectional setup: the client
        // writer feeds the server's reader (here, the same task,
        // but the topology is identical).
        let (a, b) = duplex(64 * 1024);
        let (a_read, a_write) = tokio::io::split(a);
        let (b_read, b_write) = tokio::io::split(b);

        let mut writer = LspWriter::new(a_write);
        let mut reader = LspReader::new(b_read);
        let _bridge_read = a_read;
        let _bridge_write = b_write;

        let n = Message::Notification(Notification::new(
            "$/progress",
            Some(json!({"token": "t1"})),
        ));
        writer.write_message(&n).await.unwrap();
        let got = reader.read_message().await.unwrap().unwrap();
        assert!(matches!(got, Message::Notification(_)));
    }

    #[tokio::test]
    async fn handles_lf_only_line_endings() {
        // Some servers (or stdio buffering quirks) drop the \r;
        // tolerate LF-only line endings on input. We always EMIT
        // CRLF.
        let body = br#"{"jsonrpc":"2.0","id":1,"method":"x"}"#;
        let mut stream = format!("Content-Length: {}\n\n", body.len()).into_bytes();
        stream.extend_from_slice(body);
        let mut r = LspReader::new(&stream[..]);
        let msg = r.read_message().await.unwrap().unwrap();
        assert!(matches!(msg, Message::Request(_)));
    }

    #[tokio::test]
    async fn round_trip_response_through_pipe() {
        let mut buf = Vec::new();
        {
            let mut w = LspWriter::new(&mut buf);
            let resp = Message::Response(Response::ok(
                RequestId::from_u64(1),
                json!({"capabilities": {"hoverProvider": true}}),
            ));
            w.write_message(&resp).await.unwrap();
        }
        let mut r = LspReader::new(&buf[..]);
        let msg = r.read_message().await.unwrap().unwrap();
        match msg {
            Message::Response(resp) => {
                assert!(resp.error.is_none());
                assert_eq!(resp.id, RequestId::Number(1));
            }
            _ => panic!("expected response"),
        }
    }
}
