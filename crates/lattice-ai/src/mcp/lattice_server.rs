//! LL.22: `LatticeServer`, lattice's MCP server, as one `rmcp`
//! [`ServerHandler`].
//!
//! Today it serves the Claude Code IDE contract over the loopback WebSocket
//! (`server.rs` adapts the socket to an rmcp transport): MCP `2024-11-05`, a
//! `tools` capability with `listChanged`, and the catalogue VS Code offers.
//! The catalogue and the result envelopes are kept as the JSON that contract
//! is written in and mapped into rmcp's types once, so what the agent sees is
//! byte-for-byte what it saw before rmcp, and a test can say so.
//!
//! Version: the server answers `2024-11-05` whatever the client asks for, as
//! before. rmcp would otherwise agree to any newer version the client names,
//! and a newer version changes what the client may expect of the server.

use std::borrow::Cow;
use std::sync::{Arc, LazyLock};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ErrorCode, InitializeResult,
    ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerConfig, Tool,
};
use rmcp::service::{NotificationContext, RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler};
use serde_json::{Value, json};

use lattice_diff::ProgrammaticDiffBus;

use crate::mcp::{diff, reads, writes};

/// MCP protocol version lattice speaks: the VS Code contract the `claude`
/// CLI expects of an IDE.
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Server name reported in the `initialize` result.
pub const SERVER_NAME: &str = "lattice";

const SUPPORTED_VERSIONS: &[ProtocolVersion] = &[ProtocolVersion::V_2024_11_05];

/// What the tools need: the read services, the write bus (inside
/// `reads.editor`), the `openDiff` bus and the review tracker. Built once at
/// spawn, shared, and stamped with each connection's id.
#[derive(Clone)]
pub struct ToolContext {
    /// The connection this context serves. `openDiff` tags its diff with it
    /// and the close tools tear down only its diffs, so one agent session can
    /// never close another's. `0` for the shared template and for tests.
    pub conn_id: u64,
    /// Read-tool services; the write bus rides inside `reads.editor`.
    pub reads: reads::ReadContext,
    /// The host-drained `openDiff` bus. `None` until boot wires it, and
    /// `openDiff` then answers with a graceful `isError: true`.
    pub diff: Option<ProgrammaticDiffBus>,
    /// The shared pending-review tracker: `openDiff` holds a guard on it
    /// while it waits, which drives the modeline's review badge.
    pub review: crate::mcp::status::ReviewHandle,
}

/// The catalogue as the contract writes it.
fn catalogue_json() -> Vec<Value> {
    let path = |key: &str| {
        json!({
            "type": "object",
            "properties": { key: { "type": "string" } },
            "required": [key],
        })
    };
    let empty = || json!({ "type": "object", "properties": {} });
    let tool = |name: &str, description: &str, schema: Value| json!({ "name": name, "description": description, "inputSchema": schema });
    vec![
        tool(
            "getCurrentSelection",
            "Return the active buffer's current selection.",
            empty(),
        ),
        tool("getOpenEditors", "List the open editor buffers.", empty()),
        tool(
            "getWorkspaceFolders",
            "List the open workspace folders.",
            empty(),
        ),
        tool(
            "getDiagnostics",
            "Return diagnostics for the workspace or a given file.",
            json!({ "type": "object", "properties": { "uri": { "type": "string" } } }),
        ),
        tool(
            "checkDocumentDirty",
            "Report whether a document has unsaved changes.",
            path("filePath"),
        ),
        tool(
            "openFile",
            "Open a file in the editor, optionally selecting a range.",
            path("filePath"),
        ),
        tool("saveDocument", "Save a document to disk.", path("filePath")),
        tool("close_tab", "Close an open tab by name.", path("tab_name")),
        tool(
            "closeAllDiffTabs",
            "Close all diff tabs opened by this session.",
            empty(),
        ),
        tool(
            "openDiff",
            "Open an interactive diff the user Keeps or Rejects; blocks until resolved.",
            json!({
                "type": "object",
                "properties": {
                    "old_file_path": { "type": "string" },
                    "new_file_path": { "type": "string" },
                    "new_file_contents": { "type": "string" },
                    "tab_name": { "type": "string" }
                }
            }),
        ),
    ]
}

/// The catalogue in rmcp's types, mapped once. A descriptor that does not
/// map is a bug in [`catalogue_json`]; it is logged and left out rather than
/// taking the server down, and a unit test keeps the count honest.
static CATALOGUE: LazyLock<Arc<Vec<Tool>>> = LazyLock::new(|| {
    Arc::new(
        catalogue_json()
            .into_iter()
            .filter_map(|t| match serde_json::from_value::<Tool>(t) {
                Ok(tool) => Some(tool),
                Err(e) => {
                    tracing::warn!(error = %e, "mcp: a catalogue entry does not map to a tool");
                    None
                }
            })
            .collect(),
    )
});

/// The tools the Claude Code connection offers.
pub fn catalogue() -> Arc<Vec<Tool>> {
    CATALOGUE.clone()
}

/// Lattice's MCP server for one connection.
#[derive(Clone)]
pub struct LatticeServer {
    ctx: ToolContext,
}

impl LatticeServer {
    pub fn new(ctx: ToolContext) -> Self {
        Self { ctx }
    }

    /// Run tool `name`. An unknown name is `METHOD_NOT_FOUND`, as before.
    pub async fn call(&self, name: &str, arguments: &Value) -> Result<CallToolResult, ErrorData> {
        let ctx = &self.ctx;
        let editor = &ctx.reads.editor;
        // `openDiff` builds its own envelope: its FILE_SAVED / DIFF_REJECTED
        // markers are the contract.
        if name == "openDiff" {
            return envelope(
                diff::open_diff(ctx.diff.as_ref(), arguments, ctx.conn_id, &ctx.review).await,
            );
        }
        let data = match name {
            "getCurrentSelection" => reads::get_current_selection(&ctx.reads),
            "getOpenEditors" => reads::get_open_editors(&ctx.reads),
            "getWorkspaceFolders" => reads::get_workspace_folders(&ctx.reads),
            "getDiagnostics" => reads::get_diagnostics(&ctx.reads, arguments),
            "checkDocumentDirty" => reads::check_document_dirty(&ctx.reads, arguments),
            "openFile" => writes::open_file(editor, arguments).await,
            "saveDocument" => writes::save_document(editor, arguments).await,
            "close_tab" => writes::close_tab(editor, arguments, ctx.conn_id).await,
            "closeAllDiffTabs" => writes::close_all_diff_tabs(editor, ctx.conn_id).await,
            _ => {
                return Err(ErrorData::new(
                    ErrorCode::METHOD_NOT_FOUND,
                    format!("unknown tool: {name}"),
                    None,
                ));
            }
        };
        // Reads and writes return structured data, sent as one text block.
        envelope(json!({
            "content": [{ "type": "text", "text": data.to_string() }],
            "isError": false,
        }))
    }
}

/// A `CallToolResult` from the contract's JSON envelope.
fn envelope(v: Value) -> Result<CallToolResult, ErrorData> {
    serde_json::from_value(v).map_err(|e| {
        ErrorData::new(
            ErrorCode::INTERNAL_ERROR,
            format!("tool result does not map: {e}"),
            None,
        )
    })
}

/// The `initialize` result: the version, a `tools` capability with
/// `listChanged`, and lattice's name and version.
fn server_config() -> ServerConfig {
    let v = json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": { "tools": { "listChanged": true } },
        "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
    });
    serde_json::from_value::<InitializeResult>(v).unwrap_or_else(|e| {
        // Unreachable for the literal above; a unit test pins it.
        tracing::warn!(error = %e, "mcp: initialize result does not map");
        InitializeResult::default()
    })
}

impl ServerHandler for LatticeServer {
    fn get_info(&self) -> ServerConfig {
        server_config()
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(SUPPORTED_VERSIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        serde_json::from_value(json!({ "tools": &*catalogue() })).map_err(|e| {
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("tool list does not map: {e}"),
                None,
            )
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let arguments = request.arguments.map(Value::Object).unwrap_or(json!({}));
        self.call(&request.name, &arguments)
            .await
            .map(CallToolResponse::from)
    }

    /// Advertise the tool list once the client says it is ready, as the IDE
    /// contract does.
    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        if let Err(e) = context.peer.notify_tool_list_changed().await {
            tracing::debug!(error = %e, "mcp: could not send tools/list_changed");
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn test_ctx() -> ToolContext {
        ToolContext {
            conn_id: 0,
            reads: reads::ReadContext {
                editor: lattice_agent::EditorAccess::new(
                    Arc::new(std::sync::Mutex::new(
                        lattice_agent::EditorStateCache::default(),
                    )),
                    None,
                    vec!["/work".to_string()],
                    None,
                ),
                diagnostics: None,
            },
            diff: None,
            review: crate::mcp::status::ReviewState::new(Arc::new(tokio::sync::Notify::new())),
        }
    }

    /// The catalogue rmcp serialises is the contract's JSON, entry for entry:
    /// names, descriptions and schemas, nothing added.
    #[test]
    fn the_catalogue_is_the_contract_unchanged() {
        let served: Vec<Value> = catalogue()
            .iter()
            .map(|t| serde_json::to_value(t).unwrap())
            .collect();
        assert_eq!(served, catalogue_json());
        let names: Vec<&str> = served.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(
            names,
            [
                "getCurrentSelection",
                "getOpenEditors",
                "getWorkspaceFolders",
                "getDiagnostics",
                "checkDocumentDirty",
                "openFile",
                "saveDocument",
                "close_tab",
                "closeAllDiffTabs",
                "openDiff",
            ]
        );
    }

    #[test]
    fn initialize_advertises_the_ide_version_and_listed_tools() {
        let v = serde_json::to_value(server_config()).unwrap();
        assert_eq!(v["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert_eq!(v["capabilities"]["tools"]["listChanged"], json!(true));
        assert_eq!(v["serverInfo"]["name"], SERVER_NAME);
    }

    #[tokio::test]
    async fn a_read_tool_answers_in_one_text_block() {
        let r = LatticeServer::new(test_ctx())
            .call("getWorkspaceFolders", &json!({}))
            .await
            .unwrap();
        let v = serde_json::to_value(r).unwrap();
        assert_eq!(v["isError"], json!(false));
        assert!(v["content"][0]["text"].as_str().unwrap().contains("/work"));
    }

    #[tokio::test]
    async fn an_unknown_tool_is_method_not_found_and_open_diff_without_a_bus_is_an_error_result() {
        let s = LatticeServer::new(test_ctx());
        let e = s.call("nope", &json!({})).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::METHOD_NOT_FOUND);
        let r = s.call("openDiff", &json!({})).await.unwrap();
        assert_eq!(r.is_error, Some(true));
    }
}
