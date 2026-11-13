//! AT.14: the MCP servers an ACP session is opened with.
//!
//! ACP lets the client hand the agent MCP servers in `session/new`, and every
//! agent must accept a stdio one. Lattice hands it its own bridge,
//! `lattice --mcp`, pointed at this editor, while `llm.serve_tools` is on, so
//! an agent lattice starts reaches the plugins' tools with no setup.
//!
//! What to hand over is decided where the tools are served; ACP only asks
//! when a session opens. The slot between them is filled at boot by whoever
//! serves, and empty (no servers) when nothing does.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use agent_client_protocol::schema::v1::{EnvVariable, McpServer, McpServerStdio};

/// One stdio MCP server for the agent to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdioMcpServer {
    pub name: String,
    pub command: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl StdioMcpServer {
    pub(crate) fn to_acp(&self) -> McpServer {
        McpServer::Stdio(
            McpServerStdio::new(&self.name, &self.command)
                .args(self.args.clone())
                .env(
                    self.env
                        .iter()
                        .map(|(k, v)| EnvVariable::new(k, v))
                        .collect(),
                ),
        )
    }
}

/// The servers to hand a session, as they are when it opens.
pub type McpServersFn = Arc<dyn Fn() -> Vec<StdioMcpServer> + Send + Sync>;

/// Registered by the ACP install; filled by whoever serves tools.
#[derive(Clone, Default)]
pub struct SessionMcpServers(Arc<RwLock<Option<McpServersFn>>>);

impl SessionMcpServers {
    pub fn set(&self, servers: McpServersFn) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = Some(servers);
    }

    /// The servers for a session opening now.
    pub fn now(&self) -> Vec<StdioMcpServer> {
        let servers = self.0.read().unwrap_or_else(|e| e.into_inner()).clone();
        servers.map(|f| f()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn empty_until_filled_then_asked_each_time() {
        let slot = SessionMcpServers::default();
        assert!(slot.now().is_empty());
        let on = Arc::new(std::sync::atomic::AtomicBool::new(false));
        slot.set({
            let on = on.clone();
            Arc::new(move || {
                if on.load(std::sync::atomic::Ordering::SeqCst) {
                    vec![StdioMcpServer {
                        name: "lattice".into(),
                        command: "/bin/lattice".into(),
                        args: vec!["--mcp".into()],
                        env: vec![("LATTICE_MCP_LOCK".into(), "/l.json".into())],
                    }]
                } else {
                    Vec::new()
                }
            })
        });
        assert!(slot.now().is_empty());
        on.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(slot.now().len(), 1);
    }

    /// The wire shape ACP's `session/new` carries.
    #[test]
    fn a_server_is_sent_as_acp_writes_a_stdio_server() {
        let server = StdioMcpServer {
            name: "lattice".into(),
            command: "/bin/lattice".into(),
            args: vec!["--mcp".into()],
            env: vec![("LATTICE_MCP_LOCK".into(), "/l.json".into())],
        };
        let v = serde_json::to_value(server.to_acp()).unwrap();
        assert_eq!(v["name"], "lattice");
        assert_eq!(v["command"], "/bin/lattice");
        assert_eq!(v["args"], serde_json::json!(["--mcp"]));
        assert_eq!(
            v["env"],
            serde_json::json!([{"name": "LATTICE_MCP_LOCK", "value": "/l.json"}])
        );
    }
}
