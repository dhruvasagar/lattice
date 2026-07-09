//! MCP envelope payloads carried inside the JSON-RPC frames (the
//! `lattice_protocol::jsonrpc` types are the envelope; these are the
//! method-specific bodies).
//!
//! The wire contract must match VS Code's so the stock `claude` CLI talks
//! to lattice unchanged: MCP protocol version `2024-11-05`, a `tools`
//! capability with `listChanged`, and the tool catalog enumerated by
//! `tools/list`.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// MCP protocol version lattice advertises. Must match the VS Code
/// contract the `claude` CLI expects.
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Server name reported in the `initialize` result.
pub const SERVER_NAME: &str = "lattice";

/// One MCP tool descriptor as enumerated by `tools/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    /// Tool name the agent invokes via `tools/call`.
    pub name: String,
    /// Human-readable description shown by the agent.
    pub description: String,
    /// JSON Schema for the tool's arguments.
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

impl ToolDescriptor {
    fn new(name: &str, description: &str, input_schema: Value) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            input_schema,
        }
    }
}

/// An object schema with no required properties — the placeholder used by
/// the parameterless read tools until I2 fills in real schemas.
fn empty_object_schema() -> Value {
    json!({ "type": "object", "properties": {} })
}

/// The full tool catalog lattice advertises. The tools are **stubbed** in
/// I1 (`tools/call` returns "not implemented"); reads land in I2, writes
/// in I3, `openDiff` in I4. Enumerating them now lets a real `claude` CLI
/// validate the catalog against the VS Code contract during the I0–I2
/// walking-skeleton phase.
pub fn tool_catalog() -> Vec<ToolDescriptor> {
    vec![
        // Reads (I2).
        ToolDescriptor::new(
            "getCurrentSelection",
            "Return the active buffer's current selection.",
            empty_object_schema(),
        ),
        ToolDescriptor::new(
            "getOpenEditors",
            "List the open editor buffers.",
            empty_object_schema(),
        ),
        ToolDescriptor::new(
            "getWorkspaceFolders",
            "List the open workspace folders.",
            empty_object_schema(),
        ),
        ToolDescriptor::new(
            "getDiagnostics",
            "Return diagnostics for the workspace or a given file.",
            json!({
                "type": "object",
                "properties": { "uri": { "type": "string" } }
            }),
        ),
        ToolDescriptor::new(
            "checkDocumentDirty",
            "Report whether a document has unsaved changes.",
            json!({
                "type": "object",
                "properties": { "filePath": { "type": "string" } },
                "required": ["filePath"]
            }),
        ),
        // Writes (I3).
        ToolDescriptor::new(
            "openFile",
            "Open a file in the editor, optionally selecting a range.",
            json!({
                "type": "object",
                "properties": { "filePath": { "type": "string" } },
                "required": ["filePath"]
            }),
        ),
        ToolDescriptor::new(
            "saveDocument",
            "Save a document to disk.",
            json!({
                "type": "object",
                "properties": { "filePath": { "type": "string" } },
                "required": ["filePath"]
            }),
        ),
        ToolDescriptor::new(
            "close_tab",
            "Close an open tab by name.",
            json!({
                "type": "object",
                "properties": { "tab_name": { "type": "string" } },
                "required": ["tab_name"]
            }),
        ),
        // D-fix.6: close every diff this session opened (used when the agent
        // abandons a proposed edit). Scoped to the calling connection.
        ToolDescriptor::new(
            "closeAllDiffTabs",
            "Close all diff tabs opened by this session.",
            json!({ "type": "object", "properties": {} }),
        ),
        // Blocking (I4).
        ToolDescriptor::new(
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

/// The `initialize` result body advertised to the agent.
pub fn initialize_result() -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": { "tools": { "listChanged": true } },
        "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") }
    })
}

/// The `tools/list` result body (the enumerated catalog).
pub fn tools_list_result() -> Value {
    json!({ "tools": tool_catalog() })
}

/// The `prompts/list` result body — empty; lattice exposes no MCP prompts.
pub fn prompts_list_result() -> Value {
    json!({ "prompts": [] })
}
