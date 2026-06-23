//! MCP method dispatch: decode an incoming JSON-RPC frame, route it to its
//! handler, and produce the outgoing frame(s).
//!
//! Pure + panic-free: every path returns a value; malformed input yields a
//! JSON-RPC parse-error response. This is the unit the connection loop
//! (`server::serve_connection`) drives; keeping it pure makes the protocol
//! contract testable without a socket.
//!
//! I1 handles `initialize` / `tools/list` / `prompts/list` and emits
//! `notifications/tools/list_changed` after the client's `initialized`
//! notification. `tools/call` is stubbed (reads I2, writes I3, diff I4).

use lattice_protocol::jsonrpc::{
    Message, Notification, Request, RequestId, Response, ResponseError, error_codes,
};

use crate::protocol;

/// A frame the server should send back to the agent in response to an
/// incoming frame.
#[derive(Debug)]
pub enum Outgoing {
    /// A reply to a request (carries the request's id).
    Response(Response),
    /// A server-initiated notification (no id, no reply expected).
    Notification(Notification),
}

/// Decode + route one incoming frame, returning the frames to send back.
///
/// Never panics. Malformed bytes yield a single parse-error response with
/// a null id (the id can't be recovered from unparseable input).
/// Client→server responses are ignored (lattice is the server; it issues
/// no requests in I1). A notification may produce zero or more follow-ups
/// (e.g. `initialized` → `tools/list_changed`).
pub fn dispatch_frame(bytes: &[u8]) -> Vec<Outgoing> {
    match Message::from_json(bytes) {
        Ok(Message::Request(req)) => vec![Outgoing::Response(handle_request(&req))],
        Ok(Message::Notification(note)) => handle_notification(&note),
        Ok(Message::Response(_)) => Vec::new(),
        Err(e) => vec![Outgoing::Response(Response::err(
            RequestId::Null,
            ResponseError {
                code: error_codes::PARSE_ERROR,
                message: format!("parse error: {e}"),
                data: None,
            },
        ))],
    }
}

/// Route one request to its MCP response.
pub fn handle_request(req: &Request) -> Response {
    match req.method.as_str() {
        "initialize" => Response::ok(req.id.clone(), protocol::initialize_result()),
        "tools/list" => Response::ok(req.id.clone(), protocol::tools_list_result()),
        "prompts/list" => Response::ok(req.id.clone(), protocol::prompts_list_result()),
        // I1: tools are enumerated but not yet callable.
        "tools/call" => Response::err(
            req.id.clone(),
            ResponseError {
                code: error_codes::METHOD_NOT_FOUND,
                message: "tool not yet implemented (I1 skeleton)".to_string(),
                data: None,
            },
        ),
        other => Response::err(
            req.id.clone(),
            ResponseError {
                code: error_codes::METHOD_NOT_FOUND,
                message: format!("method not found: {other}"),
                data: None,
            },
        ),
    }
}

/// Route one notification, returning any server-initiated follow-ups.
fn handle_notification(note: &Notification) -> Vec<Outgoing> {
    match note.method.as_str() {
        // Post-init: advertise the tool list once the client signals ready.
        "notifications/initialized" => vec![Outgoing::Notification(Notification::new(
            "notifications/tools/list_changed",
            None,
        ))],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn req(id: i64, method: &str) -> Request {
        Request::new(RequestId::from_u64(id as u64), method, None)
    }

    #[test]
    fn initialize_handshake_returns_protocol_version_and_capabilities() {
        let r = handle_request(&req(1, "initialize"));
        assert_eq!(r.id, RequestId::Number(1));
        let result = r.result.expect("ok result");
        assert_eq!(result["protocolVersion"], protocol::MCP_PROTOCOL_VERSION);
        assert_eq!(result["capabilities"]["tools"]["listChanged"], json!(true));
        assert_eq!(result["serverInfo"]["name"], protocol::SERVER_NAME);
        assert!(r.error.is_none());
    }

    #[test]
    fn tools_list_enumerates_the_full_catalog() {
        let r = handle_request(&req(2, "tools/list"));
        let result = r.result.expect("ok result");
        let tools = result["tools"].as_array().expect("tools array");
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        for expected in [
            "getCurrentSelection",
            "getOpenEditors",
            "getWorkspaceFolders",
            "getDiagnostics",
            "checkDocumentDirty",
            "openFile",
            "saveDocument",
            "close_tab",
            "openDiff",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }
    }

    #[test]
    fn prompts_list_is_empty() {
        let r = handle_request(&req(3, "prompts/list"));
        let result = r.result.expect("ok result");
        assert_eq!(result["prompts"].as_array().map(|a| a.len()), Some(0));
    }

    #[test]
    fn unknown_method_is_method_not_found() {
        let r = handle_request(&req(4, "no/such/method"));
        assert!(r.result.is_none());
        let e = r.error.expect("error");
        assert_eq!(e.code, error_codes::METHOD_NOT_FOUND);
    }

    #[test]
    fn tools_call_is_stubbed_not_panicking() {
        let r = handle_request(&req(5, "tools/call"));
        let e = r.error.expect("stub error");
        assert_eq!(e.code, error_codes::METHOD_NOT_FOUND);
    }

    #[test]
    fn malformed_frame_yields_parse_error_and_does_not_panic() {
        let out = dispatch_frame(b"{ this is not json");
        assert_eq!(out.len(), 1);
        match &out[0] {
            Outgoing::Response(r) => {
                let e = r.error.as_ref().expect("parse error");
                assert_eq!(e.code, error_codes::PARSE_ERROR);
                assert_eq!(r.id, RequestId::Null);
            }
            other => panic!("expected a response, got {other:?}"),
        }
    }

    #[test]
    fn initialized_notification_triggers_tools_list_changed() {
        let note = Notification::new("notifications/initialized", None);
        let bytes = serde_json::to_vec(&note).unwrap();
        let out = dispatch_frame(&bytes);
        assert_eq!(out.len(), 1);
        match &out[0] {
            Outgoing::Notification(n) => {
                assert_eq!(n.method, "notifications/tools/list_changed")
            }
            other => panic!("expected a notification, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_frame_routes_a_request_preserving_id() {
        let bytes = serde_json::to_vec(&req(9, "tools/list")).unwrap();
        let out = dispatch_frame(&bytes);
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], Outgoing::Response(r) if r.id == RequestId::Number(9)));
    }
}
