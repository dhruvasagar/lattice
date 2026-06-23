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

use serde_json::{Value, json};

use lattice_protocol::jsonrpc::{
    Message, Notification, Request, RequestId, Response, ResponseError, error_codes,
};

use crate::protocol;
use crate::reads;

/// The state the dispatcher needs to answer `tools/call`: the read tools'
/// [`ReadContext`](crate::reads::ReadContext). Built once at server spawn
/// and shared (behind an `Arc`) across connections. The I1 methods
/// (`initialize` / `tools/list` / `prompts/list`) ignore it.
pub struct DispatchContext {
    /// Read-tool services (cache + generic buffer-store / diagnostics +
    /// workspace config).
    pub reads: reads::ReadContext,
}

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
pub fn dispatch_frame(bytes: &[u8], ctx: &DispatchContext) -> Vec<Outgoing> {
    match Message::from_json(bytes) {
        Ok(Message::Request(req)) => vec![Outgoing::Response(handle_request(&req, ctx))],
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
pub fn handle_request(req: &Request, ctx: &DispatchContext) -> Response {
    match req.method.as_str() {
        "initialize" => Response::ok(req.id.clone(), protocol::initialize_result()),
        "tools/list" => Response::ok(req.id.clone(), protocol::tools_list_result()),
        "prompts/list" => Response::ok(req.id.clone(), protocol::prompts_list_result()),
        "tools/call" => handle_tools_call(req, ctx),
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

/// Route `tools/call` to the read tools (I2). The tool's structured result
/// is wrapped in the MCP `CallToolResult` content envelope. An unknown tool
/// name → `METHOD_NOT_FOUND`. Writes (I3) + `openDiff` (I4) extend this.
fn handle_tools_call(req: &Request, ctx: &DispatchContext) -> Response {
    let params = req.params.as_ref();
    let name = params
        .and_then(|p| p.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let empty = json!({});
    let arguments = params.and_then(|p| p.get("arguments")).unwrap_or(&empty);

    let result = match name {
        "getCurrentSelection" => Some(reads::get_current_selection(&ctx.reads)),
        "getOpenEditors" => Some(reads::get_open_editors(&ctx.reads)),
        "getWorkspaceFolders" => Some(reads::get_workspace_folders(&ctx.reads)),
        "getDiagnostics" => Some(reads::get_diagnostics(&ctx.reads, arguments)),
        "checkDocumentDirty" => Some(reads::check_document_dirty(&ctx.reads, arguments)),
        _ => None,
    };

    match result {
        Some(data) => Response::ok(req.id.clone(), tool_text_result(&data)),
        None => Response::err(
            req.id.clone(),
            ResponseError {
                code: error_codes::METHOD_NOT_FOUND,
                message: format!("unknown tool: {name}"),
                data: None,
            },
        ),
    }
}

/// Wrap a read-tool result in the MCP `CallToolResult` envelope: structured
/// data is serialized into a single `text` content block.
fn tool_text_result(data: &Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": serde_json::to_string(data).unwrap_or_default() }],
        "isError": false,
    })
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
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use serde_json::json;

    fn req(id: i64, method: &str) -> Request {
        Request::new(RequestId::from_u64(id as u64), method, None)
    }

    /// An empty dispatch context: empty read cache, no buffer-store /
    /// diagnostics services, one workspace folder. The I1 methods ignore
    /// it; the read tools degrade to empty results.
    fn test_ctx() -> DispatchContext {
        DispatchContext {
            reads: crate::reads::ReadContext {
                cache: std::sync::Arc::new(std::sync::Mutex::new(
                    crate::snapshot::ClaudeCodeReadState::default(),
                )),
                buffer_store: None,
                diagnostics: None,
                workspace_folders: vec!["/work".to_string()],
            },
        }
    }

    #[test]
    fn initialize_handshake_returns_protocol_version_and_capabilities() {
        let r = handle_request(&req(1, "initialize"), &test_ctx());
        assert_eq!(r.id, RequestId::Number(1));
        let result = r.result.expect("ok result");
        assert_eq!(result["protocolVersion"], protocol::MCP_PROTOCOL_VERSION);
        assert_eq!(result["capabilities"]["tools"]["listChanged"], json!(true));
        assert_eq!(result["serverInfo"]["name"], protocol::SERVER_NAME);
        assert!(r.error.is_none());
    }

    #[test]
    fn tools_list_enumerates_the_full_catalog() {
        let r = handle_request(&req(2, "tools/list"), &test_ctx());
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
        let r = handle_request(&req(3, "prompts/list"), &test_ctx());
        let result = r.result.expect("ok result");
        assert_eq!(result["prompts"].as_array().map(|a| a.len()), Some(0));
    }

    #[test]
    fn unknown_method_is_method_not_found() {
        let r = handle_request(&req(4, "no/such/method"), &test_ctx());
        assert!(r.result.is_none());
        let e = r.error.expect("error");
        assert_eq!(e.code, error_codes::METHOD_NOT_FOUND);
    }

    #[test]
    fn tools_call_unknown_tool_is_method_not_found() {
        // `req(5, "tools/call")` carries no `name` → unknown tool.
        let r = handle_request(&req(5, "tools/call"), &test_ctx());
        let e = r.error.expect("unknown-tool error");
        assert_eq!(e.code, error_codes::METHOD_NOT_FOUND);
    }

    #[test]
    fn tools_call_known_read_tool_returns_content_envelope() {
        let call = Request::new(
            RequestId::from_u64(6),
            "tools/call",
            Some(json!({ "name": "getWorkspaceFolders", "arguments": {} })),
        );
        let r = handle_request(&call, &test_ctx());
        let result = r.result.expect("ok result");
        assert_eq!(result["isError"], json!(false));
        let text = result["content"][0]["text"].as_str().expect("text block");
        // The workspace result is JSON-stringified into the text block.
        assert!(text.contains("folders"), "got {text}");
    }

    #[test]
    fn malformed_frame_yields_parse_error_and_does_not_panic() {
        let out = dispatch_frame(b"{ this is not json", &test_ctx());
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
        let out = dispatch_frame(&bytes, &test_ctx());
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
        let out = dispatch_frame(&bytes, &test_ctx());
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], Outgoing::Response(r) if r.id == RequestId::Number(9)));
    }
}
