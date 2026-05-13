//! LSP `Content-Length` header framing (Microsoft LSP base
//! protocol). Pure parser -- no IO, no allocations beyond the
//! returned [`FrameHeader`].
//!
//! ```text
//! Content-Length: 47\r\n
//! Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n
//! \r\n
//! {"jsonrpc":"2.0","method":"initialized","params":{}}
//! ```
//!
//! The header block is ASCII; the body is UTF-8 JSON.
//! `Content-Length` is mandatory. Any other header -- including the
//! optional `Content-Type` -- is captured but not interpreted.
//!
//! ### Error model
//!
//! Returns [`FrameError`] for ill-formed headers; the codec
//! propagates these as `ProtocolError` and tears down the
//! transport. A misbehaving server cannot wedge the editor: the
//! actor restarts the server with backoff (§5.4 crash recovery).

use std::str;

use thiserror::Error;

/// One parsed LSP message header.
///
/// Only `content_length` is load-bearing. `content_type` is
/// preserved (LSP servers in the wild do send it) but not
/// validated -- the body is always UTF-8 JSON in practice, and
/// rejecting unknown content types would be hostile to forward
/// compatibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameHeader {
    /// Body length in bytes. Reader allocates exactly this much
    /// for the JSON payload.
    pub content_length: u64,
    /// Optional `Content-Type` field. Captured for telemetry /
    /// debug but not enforced.
    pub content_type: Option<String>,
}

/// Header-parse failures.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum FrameError {
    /// Header block contained no `Content-Length` field. The
    /// LSP base protocol says it MUST be present; any message
    /// missing it is unframable.
    #[error("missing Content-Length header")]
    MissingContentLength,
    /// `Content-Length` value didn't parse as an unsigned
    /// integer.
    #[error("invalid Content-Length value: {0:?}")]
    InvalidContentLength(String),
    /// Header line lacked the `name: value` separator. ASCII
    /// only; whitespace around `:` is tolerated.
    #[error("malformed header line: {0:?}")]
    MalformedHeader(String),
    /// Header bytes were not valid ASCII (or the bytes before
    /// the body terminator weren't, which forces a teardown).
    #[error("non-ascii bytes in header block")]
    NonAsciiHeader,
    /// Two `Content-Length` fields in the same header block.
    /// The LSP base protocol doesn't say what to do; we reject.
    /// Forwarding the wrong length would either truncate the
    /// body or read past it into the next message.
    #[error("duplicate Content-Length header")]
    DuplicateContentLength,
    /// Content length exceeds the configured per-message ceiling.
    /// Defends against a malicious / runaway server pinning a
    /// gigabyte of memory per message.
    #[error("Content-Length {got} exceeds ceiling {limit}")]
    OverlargeMessage { got: u64, limit: u64 },
}

/// Per-message size ceiling. 64 MiB is comfortably above the
/// largest realistic LSP payload (workspace symbol indices on
/// huge monorepos sit in the few-megabyte range) while bounding
/// memory pressure from a runaway server.
pub const MAX_MESSAGE_BYTES: u64 = 64 * 1024 * 1024;

/// Parse the bytes of one header block (everything BEFORE the
/// `\r\n\r\n` terminator) into a [`FrameHeader`].
///
/// `block` must contain CRLF-separated lines per LSP base
/// protocol. The codec strips the terminator before calling.
pub fn parse_header_block(block: &[u8]) -> Result<FrameHeader, FrameError> {
    parse_header_block_with_limit(block, MAX_MESSAGE_BYTES)
}

/// [`parse_header_block`] with an explicit ceiling. Tests use
/// this to verify the bound is enforced; production paths go
/// through the public wrapper.
pub fn parse_header_block_with_limit(block: &[u8], limit: u64) -> Result<FrameHeader, FrameError> {
    let text = str::from_utf8(block).map_err(|_| FrameError::NonAsciiHeader)?;
    if !text.is_ascii() {
        return Err(FrameError::NonAsciiHeader);
    }

    let mut content_length: Option<u64> = None;
    let mut content_type: Option<String> = None;

    for line in text.split("\r\n") {
        if line.is_empty() {
            // The terminator-empty-line is stripped by the codec.
            // Any further empty lines inside the header block are
            // tolerated; servers occasionally emit them after a
            // restart.
            continue;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| FrameError::MalformedHeader(line.to_string()))?;
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err(FrameError::DuplicateContentLength);
            }
            let parsed: u64 = value
                .parse()
                .map_err(|_| FrameError::InvalidContentLength(value.to_string()))?;
            if parsed > limit {
                return Err(FrameError::OverlargeMessage { got: parsed, limit });
            }
            content_length = Some(parsed);
        } else if name.eq_ignore_ascii_case("content-type") {
            content_type = Some(value.to_string());
        }
        // Any other header is silently dropped. LSP doesn't define
        // them; servers that send them shouldn't crash the client.
    }

    let content_length = content_length.ok_or(FrameError::MissingContentLength)?;
    Ok(FrameHeader {
        content_length,
        content_type,
    })
}

/// Encode a frame header for an outgoing message body of
/// `body_len` bytes. Returns the ASCII header bytes including the
/// trailing `\r\n\r\n` terminator -- ready to be written before
/// the body. Allocation-light: one `String` of bounded size.
pub fn encode_header(body_len: usize) -> String {
    // We deliberately don't emit Content-Type; LSP base protocol
    // makes it optional, and most servers ignore it on input.
    // Skipping it saves ~50 bytes per message.
    format!("Content-Length: {}\r\n\r\n", body_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_header() {
        let h = parse_header_block(b"Content-Length: 42").unwrap();
        assert_eq!(h.content_length, 42);
        assert_eq!(h.content_type, None);
    }

    #[test]
    fn parses_header_with_content_type() {
        let block =
            b"Content-Length: 13\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8";
        let h = parse_header_block(block).unwrap();
        assert_eq!(h.content_length, 13);
        assert_eq!(
            h.content_type.as_deref(),
            Some("application/vscode-jsonrpc; charset=utf-8")
        );
    }

    #[test]
    fn header_name_is_case_insensitive() {
        let h = parse_header_block(b"content-length: 7").unwrap();
        assert_eq!(h.content_length, 7);
        let h = parse_header_block(b"CONTENT-LENGTH: 7").unwrap();
        assert_eq!(h.content_length, 7);
    }

    #[test]
    fn tolerates_whitespace_around_value() {
        let h = parse_header_block(b"Content-Length:    99   ").unwrap();
        assert_eq!(h.content_length, 99);
    }

    #[test]
    fn ignores_unknown_headers() {
        let h = parse_header_block(b"X-Trace-Id: abc\r\nContent-Length: 1").unwrap();
        assert_eq!(h.content_length, 1);
    }

    #[test]
    fn missing_content_length_is_error() {
        let err = parse_header_block(b"Content-Type: application/json").unwrap_err();
        assert_eq!(err, FrameError::MissingContentLength);
    }

    #[test]
    fn invalid_content_length_is_error() {
        let err = parse_header_block(b"Content-Length: not-a-number").unwrap_err();
        assert!(matches!(err, FrameError::InvalidContentLength(_)));
    }

    #[test]
    fn malformed_line_is_error() {
        let err = parse_header_block(b"Content-Length 42").unwrap_err();
        assert!(matches!(err, FrameError::MalformedHeader(_)));
    }

    #[test]
    fn duplicate_content_length_is_error() {
        let err = parse_header_block(b"Content-Length: 5\r\nContent-Length: 6").unwrap_err();
        assert_eq!(err, FrameError::DuplicateContentLength);
    }

    #[test]
    fn non_ascii_header_is_error() {
        // The LSP spec says headers are ASCII. A server putting a
        // non-ASCII byte in there is broken; refusing protects the
        // body byte count from desync.
        let mut block = b"Content-Length: 5\r\nX-Note: ".to_vec();
        block.push(0xFF);
        let err = parse_header_block(&block).unwrap_err();
        assert_eq!(err, FrameError::NonAsciiHeader);
    }

    #[test]
    fn body_size_ceiling_is_enforced() {
        // Default ceiling lets a normal message through.
        assert!(parse_header_block(b"Content-Length: 1024").is_ok());
        // Above the ceiling, error out.
        let err = parse_header_block_with_limit(b"Content-Length: 1024", 1023).unwrap_err();
        assert!(matches!(
            err,
            FrameError::OverlargeMessage {
                got: 1024,
                limit: 1023
            }
        ));
    }

    #[test]
    fn encode_header_roundtrips() {
        // Encoder output, when fed back to the parser (header
        // block stripped of trailing terminator), must yield the
        // same content_length.
        let h = encode_header(1234);
        assert_eq!(h, "Content-Length: 1234\r\n\r\n");
        let block = h.trim_end_matches("\r\n\r\n");
        let parsed = parse_header_block(block.as_bytes()).unwrap();
        assert_eq!(parsed.content_length, 1234);
    }

    #[test]
    fn empty_block_is_missing_content_length() {
        let err = parse_header_block(b"").unwrap_err();
        assert_eq!(err, FrameError::MissingContentLength);
    }
}
