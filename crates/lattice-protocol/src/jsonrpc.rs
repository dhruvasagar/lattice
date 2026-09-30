//! JSON-RPC 2.0 message types -- the wire shape of every LSP
//! exchange. Transport-agnostic: these types serialize through
//! `serde_json::to_string`, the codec writes the bytes.
//!
//! Also the wire shape of the Claude Code IDE peer (`lattice-claude-code`)
//! and the MCP server in `lattice-ai`; none of them owns the types, so none
//! of them depends on another to share them. Framing (LSP's `Content-Length`
//! headers, a WebSocket frame) is each peer's codec's business.
//!
//! # Examples
//!
//! ```
//! use lattice_protocol::{Message, Request, RequestId, Response, ResponseError};
//! use serde_json::json;
//!
//! // Outgoing: build, then serialize for the codec.
//! let req = Request::new(RequestId::from_u64(1), "initialize", Some(json!({})));
//! let bytes = Message::Request(req).to_json().unwrap();
//! assert!(bytes.starts_with(br#"{"jsonrpc":"2.0","id":1,"method":"initialize""#));
//!
//! // Incoming: decode by shape — `id` + `result` is a response.
//! let wire = br#"{"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}"#;
//! let Message::Response(resp) = Message::from_json(wire).unwrap() else {
//!     panic!("expected a response");
//! };
//! assert_eq!(resp.id, RequestId::Number(1));
//! assert!(resp.error.is_none());
//!
//! // `method` without `id` is a notification.
//! let note = br#"{"jsonrpc":"2.0","method":"$/progress","params":{}}"#;
//! assert!(matches!(Message::from_json(note), Ok(Message::Notification(_))));
//!
//! // Answering a server-initiated request we cannot serve.
//! let reply = Response::err(
//!     RequestId::String("cfg-1".into()),
//!     ResponseError {
//!         code: lattice_protocol::jsonrpc::error_codes::METHOD_NOT_FOUND,
//!         message: "unsupported".into(),
//!         data: None,
//!     },
//! );
//! let json = String::from_utf8(Message::Response(reply).to_json().unwrap()).unwrap();
//! assert!(json.contains(r#""id":"cfg-1""#) && !json.contains("result"));
//! ```
//!
//! ## Why we keep `params` / `result` as `serde_json::Value`
//!
//! `lsp-types` has typed structs for every method. We could
//! parameterize on `<P, R>` but that would push the typing into
//! every actor and complicate dynamic dispatch (one mailbox
//! handling 30+ method shapes). Instead the codec yields
//! `serde_json::Value`-bearing messages and the actor downcasts
//! per method via `serde_json::from_value::<T>(...)`. That
//! confines the type discipline to the message-handling layer
//! where it belongs.
//!
//! ## Id correlation
//!
//! Every outgoing request gets a fresh [`RequestId::Number`] from
//! the actor's monotonic counter. Incoming responses are matched
//! against an id → oneshot map; matching is a `HashMap` lookup.
//! Server-initiated requests (e.g. `workspace/configuration`) use
//! their own id; we echo it back on the response unchanged.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC 2.0 protocol version literal. Always exactly this
/// string on the wire; we preserve it as-is so a non-conforming
/// server can be flagged early.
pub const JSONRPC_VERSION: &str = "2.0";

/// Per JSON-RPC, an id is a string, a number, or null. LSP
/// servers in the wild send all three; clients (us) only emit
/// `Number`, but we accept all on the read path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    /// A numeric id — what lattice itself always sends.
    Number(i64),
    /// A string id, as some servers use for their own requests.
    String(String),
    /// Some LSP servers send `null` for cancellation acks. The
    /// JSON-RPC spec discourages it but doesn't forbid it.
    Null,
}

impl RequestId {
    /// Construct a `Number` id; the common case for our actor's
    /// outgoing requests.
    ///
    /// Values above `i64::MAX` wrap (an `as` cast); a monotonic counter
    /// never gets there.
    pub fn from_u64(n: u64) -> Self {
        // i64 fits any sane request count; the actor counter is
        // monotonic from 0 and we'll never overflow in practice.
        // i64::MAX is ~9.2 quintillion.
        RequestId::Number(n as i64)
    }
}

/// One JSON-RPC request that expects a response. `params` is
/// optional per JSON-RPC; LSP methods that take no parameters
/// should send `params: null` (or omit). We always include it as
/// `null` to keep the wire layout uniform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// Protocol version; [`JSONRPC_VERSION`] on everything we build. Not
    /// validated on decode.
    pub jsonrpc: String,
    /// Correlates the eventual [`Response`].
    pub id: RequestId,
    /// Method name, e.g. `textDocument/hover`.
    pub method: String,
    /// Method-specific parameters. The actor downcasts this with
    /// `serde_json::from_value::<lsp_types::FooParams>(...)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    /// Build an outgoing request. `id` is supplied by the actor
    /// from its monotonic counter; pairs the response back.
    pub fn new(id: RequestId, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            method: method.into(),
            params,
        }
    }
}

/// One JSON-RPC notification: fire-and-forget, no response. LSP
/// uses these for `textDocument/didChange`,
/// `textDocument/publishDiagnostics`, `$/progress`, and the like.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    /// Protocol version; [`JSONRPC_VERSION`] on everything we build.
    pub jsonrpc: String,
    /// Method name, e.g. `textDocument/didChange`.
    pub method: String,
    /// Method-specific parameters; omitted from the wire when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Notification {
    /// Build an outgoing notification.
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.into(),
            params,
        }
    }
}

/// One JSON-RPC response. Either `result` or `error` is set;
/// never both.
///
/// Decoding does not enforce the "never both" half: a message carrying both
/// decodes with both set, and consumers check `error` first. A message with
/// an `id` but *neither* key is not a response at all —
/// [`Message::from_json`] rejects it as [`MessageDecodeError::Malformed`].
///
/// A successful result of JSON `null` (e.g. `shutdown`'s) deserializes to
/// `result: None`, indistinguishable from an absent key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    /// Protocol version; [`JSONRPC_VERSION`] on everything we build.
    pub jsonrpc: String,
    /// The id of the [`Request`] this answers, echoed unchanged.
    pub id: RequestId,
    /// The success payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// The failure payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
}

impl Response {
    /// Successful response constructor.
    pub fn ok(id: RequestId, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Error-response constructor.
    pub fn err(id: RequestId, error: ResponseError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// JSON-RPC error envelope. `code` is one of the standard
/// integers from the JSON-RPC spec ([`error_codes`]), a JSON-RPC
/// implementation-defined server error (`-32099 ..= -32000`), or one of
/// LSP's own codes (`-32899 ..= -32800`); `message` is
/// human-readable; `data` is whatever the server attaches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResponseError {
    /// Numeric error code; see [`error_codes`].
    pub code: i64,
    /// Human-readable description.
    pub message: String,
    /// Optional structured detail, as the sender chose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Standard JSON-RPC 2.0 error codes plus the LSP-defined ones.
/// Used by the actor to build error responses to server-initiated
/// requests we can't fulfil.
pub mod error_codes {
    /// Invalid JSON received by the server.
    pub const PARSE_ERROR: i64 = -32700;
    /// JSON sent is not a valid Request object.
    pub const INVALID_REQUEST: i64 = -32600;
    /// The method does not exist / is not available.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Invalid method parameter(s).
    pub const INVALID_PARAMS: i64 = -32602;
    /// Internal JSON-RPC error.
    pub const INTERNAL_ERROR: i64 = -32603;

    // LSP-defined codes. `SERVER_NOT_INITIALIZED` sits in JSON-RPC's
    // implementation-defined -32099..=-32000 range; the rest in LSP's own
    // reserved -32899..=-32800 range.
    /// A request arrived before the `initialize` handshake completed.
    pub const SERVER_NOT_INITIALIZED: i64 = -32002;
    /// The request was well-formed and understood, but failed (LSP 3.17).
    pub const REQUEST_FAILED: i64 = -32803;
    /// The request was cancelled (`$/cancelRequest`) and the server stopped
    /// work on it.
    pub const REQUEST_CANCELLED: i64 = -32800;
    /// The document changed while the request was in flight, so the result
    /// would be stale; the client should re-request if still relevant.
    pub const CONTENT_MODIFIED: i64 = -32801;
}

/// One incoming or outgoing JSON-RPC message. The codec yields
/// `Message` from the wire; the actor matches on the variant.
///
/// We tag variants by the presence of `id` and `method`:
/// - request: `id` + `method`
/// - response: `id` + (`result` xor `error`), no `method`
/// - notification: `method`, no `id`
///
/// The custom deserialize (rather than `#[serde(untagged)]`)
/// avoids an O(n²) try-each-variant pattern for every incoming
/// blob and gives precise error messages.
#[derive(Debug, Clone)]
pub enum Message {
    /// Has `id` and `method`: expects a [`Response`].
    Request(Request),
    /// Has `id` and `result` / `error`, no `method`.
    Response(Response),
    /// Has `method`, no `id`: fire-and-forget.
    Notification(Notification),
}

impl Message {
    /// Decode one JSON-RPC message from a UTF-8 JSON byte slice.
    /// Returns the typed variant; structurally invalid input
    /// (missing both id and method, etc.) returns
    /// [`MessageDecodeError::Malformed`].
    pub fn from_json(bytes: &[u8]) -> Result<Self, MessageDecodeError> {
        // Parse to Value first so we can branch on shape without
        // allocating three different typed parses on failure.
        let v: Value = serde_json::from_slice(bytes).map_err(MessageDecodeError::Json)?;
        let obj = v
            .as_object()
            .ok_or_else(|| MessageDecodeError::Malformed("top-level not an object".into()))?;
        let has_method = obj.contains_key("method");
        let has_id = obj.contains_key("id");
        let has_result = obj.contains_key("result");
        let has_error = obj.contains_key("error");

        if has_method && has_id {
            let req: Request = serde_json::from_value(v).map_err(MessageDecodeError::Json)?;
            Ok(Message::Request(req))
        } else if has_method {
            let n: Notification = serde_json::from_value(v).map_err(MessageDecodeError::Json)?;
            Ok(Message::Notification(n))
        } else if has_id && (has_result || has_error) {
            let r: Response = serde_json::from_value(v).map_err(MessageDecodeError::Json)?;
            Ok(Message::Response(r))
        } else {
            Err(MessageDecodeError::Malformed(
                "neither method nor result/error present".into(),
            ))
        }
    }

    /// Serialize this message to JSON bytes ready for the
    /// codec. Uses compact (no-indent) JSON; LSP servers are
    /// agnostic and we save a few bytes per message.
    pub fn to_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        match self {
            Message::Request(r) => serde_json::to_vec(r),
            Message::Response(r) => serde_json::to_vec(r),
            Message::Notification(n) => serde_json::to_vec(n),
        }
    }
}

/// Decode failures from [`Message::from_json`].
#[derive(Debug, thiserror::Error)]
pub enum MessageDecodeError {
    /// `serde_json` couldn't parse the bytes as JSON.
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// JSON parsed, but the object didn't match
    /// request/response/notification shapes.
    #[error("malformed JSON-RPC message: {0}")]
    Malformed(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_round_trip() {
        let req = Request::new(
            RequestId::from_u64(1),
            "textDocument/hover",
            Some(
                json!({"textDocument": {"uri": "file:///x"}, "position": {"line": 0, "character": 0}}),
            ),
        );
        let bytes = serde_json::to_vec(&req).unwrap();
        let parsed = Message::from_json(&bytes).unwrap();
        match parsed {
            Message::Request(r) => {
                assert_eq!(r.method, "textDocument/hover");
                assert_eq!(r.id, RequestId::Number(1));
                assert!(r.params.is_some());
            }
            _ => panic!("expected Request"),
        }
    }

    #[test]
    fn notification_round_trip() {
        let n = Notification::new("initialized", Some(json!({})));
        let bytes = serde_json::to_vec(&n).unwrap();
        let parsed = Message::from_json(&bytes).unwrap();
        assert!(matches!(parsed, Message::Notification(_)));
    }

    #[test]
    fn response_ok_round_trip() {
        let r = Response::ok(RequestId::from_u64(7), json!({"capabilities": {}}));
        let bytes = serde_json::to_vec(&r).unwrap();
        let parsed = Message::from_json(&bytes).unwrap();
        match parsed {
            Message::Response(resp) => {
                assert_eq!(resp.id, RequestId::Number(7));
                assert!(resp.result.is_some());
                assert!(resp.error.is_none());
            }
            _ => panic!("expected Response"),
        }
    }

    #[test]
    fn response_err_round_trip() {
        let r = Response::err(
            RequestId::from_u64(7),
            ResponseError {
                code: error_codes::METHOD_NOT_FOUND,
                message: "no such method".into(),
                data: None,
            },
        );
        let bytes = serde_json::to_vec(&r).unwrap();
        let parsed = Message::from_json(&bytes).unwrap();
        match parsed {
            Message::Response(resp) => {
                assert!(resp.result.is_none());
                let e = resp.error.unwrap();
                assert_eq!(e.code, error_codes::METHOD_NOT_FOUND);
            }
            _ => panic!("expected Response"),
        }
    }

    #[test]
    fn string_request_id_round_trips() {
        // LSP servers MAY use string ids; we MUST preserve them
        // verbatim on the response we send back.
        let raw =
            br#"{"jsonrpc":"2.0","id":"abc-123","method":"workspace/configuration","params":{}}"#;
        let parsed = Message::from_json(raw).unwrap();
        match parsed {
            Message::Request(r) => assert_eq!(r.id, RequestId::String("abc-123".into())),
            _ => panic!("expected Request"),
        }
    }

    #[test]
    fn null_request_id_round_trips() {
        // Some servers send null id on cancellation acks. Accept
        // it, don't crash.
        let raw = br#"{"jsonrpc":"2.0","id":null,"result":null}"#;
        let parsed = Message::from_json(raw).unwrap();
        match parsed {
            Message::Response(r) => assert_eq!(r.id, RequestId::Null),
            _ => panic!("expected Response"),
        }
    }

    #[test]
    fn malformed_json_is_error() {
        let err = Message::from_json(b"{ not json").unwrap_err();
        assert!(matches!(err, MessageDecodeError::Json(_)));
    }

    #[test]
    fn message_without_method_or_result_is_malformed() {
        // Has id but neither method nor result/error.
        let err = Message::from_json(br#"{"jsonrpc":"2.0","id":1}"#).unwrap_err();
        assert!(matches!(err, MessageDecodeError::Malformed(_)));
    }

    #[test]
    fn top_level_not_object_is_malformed() {
        let err = Message::from_json(b"42").unwrap_err();
        assert!(matches!(err, MessageDecodeError::Malformed(_)));
    }

    #[test]
    fn omitted_params_round_trips_as_none() {
        // Some servers omit `params` for parameterless methods
        // like `shutdown`.
        let raw = br#"{"jsonrpc":"2.0","id":99,"method":"shutdown"}"#;
        let parsed = Message::from_json(raw).unwrap();
        match parsed {
            Message::Request(r) => assert!(r.params.is_none()),
            _ => panic!("expected Request"),
        }
    }
}
