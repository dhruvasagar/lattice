//! The `ToolBroker`: every tool source, namespaced, under one trust table.
//!
//! **Names.** A tool is `server.tool` wherever a person writes it (`llm.tools`,
//! a profile) and `server__tool` on the wire, because providers allow only
//! `[A-Za-z0-9_-]` in a tool name. Every source is namespaced the same way,
//! lattice's included, so a server added later cannot collide with it.
//!
//! **Selection.** `llm.tools` is a list of selectors: `server.*` (all of a
//! server's tools), `server.name` (one), or `server.read` / `server.edit` /
//! `server.exec` (a server's tools of that kind, when no tool has that exact
//! name). Kinds come from the source and are believed only for lattice's own
//! tools, so `github.read` selects nothing unless GitHub's server has a tool
//! named `read`.
//!
//! **An offer is fixed for its request.** [`ToolBroker::offer`] resolves the
//! selection once, before the first exchange; the loop calls through the
//! [`Offer`] and nothing else. A tool result is only text the model reads, so
//! no result can add a tool or loosen a permission: the structure has no path
//! by which it could.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, RwLock};

use serde_json::Value;

use super::{SourceTool, ToolError, ToolOutput, ToolSource};
use crate::conversation::ToolKind;
use crate::llm::ToolDescriptor;

/// Lattice's own namespace; the only source whose tool kinds are trusted.
pub const LATTICE: &str = "lattice";

/// Wire-name separator between server and tool.
const SEP: &str = "__";

/// What happens when the model calls a tool (design §6's table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    /// Runs without asking: lattice's read tools.
    Run,
    /// Goes to diff review: lattice's edit tools (LL.17).
    Review,
    /// Asks first: exec, and any tool from another server (LL.18).
    Ask,
}

/// One tool in an offer.
#[derive(Debug, Clone, PartialEq)]
pub struct OfferedTool {
    /// The name the model sees: `lattice__read_file`.
    pub wire: String,
    /// The server: `lattice`.
    pub server: String,
    /// The source-local name: `read_file`.
    pub name: String,
    pub kind: ToolKind,
    pub permission: Permission,
    pub descriptor: ToolDescriptor,
}

/// The tools one request may call, fixed when it starts.
#[derive(Clone, Default)]
pub struct Offer {
    tools: Vec<OfferedTool>,
    sources: HashMap<String, Arc<dyn ToolSource>>,
}

impl std::fmt::Debug for Offer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Offer").field("tools", &self.tools).finish()
    }
}

impl Offer {
    /// No tools: the request goes without any.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn tools(&self) -> &[OfferedTool] {
        &self.tools
    }

    /// The descriptors sent with the request, under their wire names.
    pub fn descriptors(&self) -> Vec<ToolDescriptor> {
        self.tools
            .iter()
            .map(|t| ToolDescriptor {
                name: t.wire.clone(),
                ..t.descriptor.clone()
            })
            .collect()
    }

    /// The offered tool the model named, if this request offers it.
    pub fn find(&self, wire: &str) -> Option<&OfferedTool> {
        self.tools.iter().find(|t| t.wire == wire)
    }

    /// Run the tool the model named. A name this request was not offered is
    /// refused by name, whatever any earlier result said; a tool that needs
    /// approval is not run here, since review and asking arrive with the
    /// slices that build them (LL.17, LL.18).
    pub async fn call(&self, wire: &str, input: Value) -> Result<ToolOutput, ToolError> {
        let tool = self
            .find(wire)
            .ok_or_else(|| ToolError::UnknownTool(wire.to_string()))?;
        if tool.permission != Permission::Run {
            return Err(ToolError::Refused(format!(
                "{}.{} needs approval, which this version cannot ask for yet",
                tool.server, tool.name
            )));
        }
        let source = self
            .sources
            .get(&tool.server)
            .ok_or_else(|| ToolError::UnknownTool(wire.to_string()))?;
        source.call(&tool.name, input).await
    }
}

/// Every tool source, namespaced.
#[derive(Default)]
pub struct ToolBroker {
    sources: RwLock<Vec<(String, Arc<dyn ToolSource>)>>,
}

/// Shared by the llm layer and anything that adds a source.
pub type ToolBrokerHandle = Arc<ToolBroker>;

impl ToolBroker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `source` under `server`, replacing one already there. A server
    /// name holds letters, digits and `-` only, so wire names split cleanly.
    pub fn add_source(&self, server: &str, source: Arc<dyn ToolSource>) {
        debug_assert!(
            !server.is_empty()
                && server
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "server names are [A-Za-z0-9-]: {server}"
        );
        let mut g = self.sources.write().unwrap_or_else(|e| e.into_inner());
        g.retain(|(s, _)| s != server);
        g.push((server.to_string(), source));
    }

    /// The tools `selection` picks, as one request's offer. A source that
    /// cannot list its tools is left out and logged, never failing the
    /// request: a broken server must not stop a chat.
    pub async fn offer(&self, selection: &BTreeSet<String>) -> Offer {
        if selection.is_empty() {
            return Offer::default();
        }
        let sources = self
            .sources
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut offer = Offer::default();
        for (server, source) in sources {
            let wanted: Vec<&str> = selection
                .iter()
                .filter_map(|s| s.strip_prefix(server.as_str())?.strip_prefix('.'))
                .collect();
            if wanted.is_empty() {
                continue;
            }
            let listed = match source.list().await {
                Ok(listed) => listed,
                Err(e) => {
                    tracing::warn!(target: "lattice::llm", "llm: tools from {server} are unavailable: {e}");
                    continue;
                }
            };
            let mut any = false;
            for tool in &listed {
                if selected(&wanted, tool, &listed, &server) {
                    any = true;
                    offer.tools.push(offered(&server, tool));
                }
            }
            if any {
                offer.sources.insert(server, source);
            }
        }
        offer
    }
}

/// Does a selector in `wanted` (the part after `server.`) pick `tool`?
fn selected(wanted: &[&str], tool: &SourceTool, all: &[SourceTool], server: &str) -> bool {
    let name = tool.descriptor.name.as_str();
    wanted.iter().any(|w| {
        if *w == "*" || *w == name {
            return true;
        }
        // A kind group, unless a tool is literally called that.
        let named_so = all.iter().any(|t| t.descriptor.name == *w);
        !named_so && server == LATTICE && group_of(tool.kind) == Some(*w)
    })
}

fn group_of(kind: ToolKind) -> Option<&'static str> {
    match kind {
        ToolKind::Read | ToolKind::Search => Some("read"),
        ToolKind::Edit => Some("edit"),
        ToolKind::Execute => Some("exec"),
        ToolKind::Other => None,
    }
}

fn offered(server: &str, tool: &SourceTool) -> OfferedTool {
    let ours = server == LATTICE;
    // Kinds are believed for lattice's tools only; another server's tool is
    // shown as `Other` and always asks.
    let kind = if ours { tool.kind } else { ToolKind::Other };
    let permission = match (ours, kind) {
        (true, ToolKind::Read | ToolKind::Search) => Permission::Run,
        (true, ToolKind::Edit) => Permission::Review,
        _ => Permission::Ask,
    };
    OfferedTool {
        wire: format!("{server}{SEP}{}", tool.descriptor.name),
        server: server.to_string(),
        name: tool.descriptor.name.clone(),
        kind,
        permission,
        descriptor: tool.descriptor.clone(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::llm::tools::BoxFuture;

    struct Fixed(Vec<SourceTool>);

    impl ToolSource for Fixed {
        fn list(&self) -> BoxFuture<'_, Result<Vec<SourceTool>, ToolError>> {
            let tools = self.0.clone();
            Box::pin(async move { Ok(tools) })
        }
        fn call(&self, name: &str, _: Value) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
            let text = format!("ran {name}");
            Box::pin(async move { Ok(ToolOutput { text }) })
        }
    }

    fn tool(name: &str, kind: ToolKind) -> SourceTool {
        SourceTool {
            descriptor: ToolDescriptor {
                name: name.into(),
                description: String::new(),
                input_schema: serde_json::json!({"type": "object"}),
            },
            kind,
        }
    }

    fn broker() -> ToolBroker {
        let b = ToolBroker::new();
        b.add_source(
            LATTICE,
            Arc::new(Fixed(vec![
                tool("read_file", ToolKind::Read),
                tool("search", ToolKind::Search),
                tool("write_file", ToolKind::Edit),
            ])),
        );
        b.add_source(
            "github",
            Arc::new(Fixed(vec![
                tool("search_issues", ToolKind::Read),
                tool("create_issue", ToolKind::Edit),
            ])),
        );
        b
    }

    fn pick(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn wires(o: &Offer) -> Vec<&str> {
        o.tools().iter().map(|t| t.wire.as_str()).collect()
    }

    #[tokio::test]
    async fn selectors_pick_by_name_group_or_star_and_namespace_every_server() {
        let b = broker();
        let o = b.offer(&pick(&["lattice.read"])).await;
        assert_eq!(wires(&o), ["lattice__read_file", "lattice__search"]);
        assert!(o.tools().iter().all(|t| t.permission == Permission::Run));

        let o = b.offer(&pick(&["lattice.write_file", "github.*"])).await;
        assert_eq!(
            wires(&o),
            [
                "lattice__write_file",
                "github__search_issues",
                "github__create_issue"
            ]
        );
        let perms: Vec<Permission> = o.tools().iter().map(|t| t.permission).collect();
        assert_eq!(
            perms,
            [Permission::Review, Permission::Ask, Permission::Ask]
        );

        assert!(b.offer(&BTreeSet::new()).await.is_empty());
    }

    /// A server's own claim of `Read` is not believed: its tools ask, and a
    /// kind group does not select them.
    #[tokio::test]
    async fn another_servers_kinds_are_not_trusted() {
        let b = broker();
        assert!(b.offer(&pick(&["github.read"])).await.is_empty());
        let o = b.offer(&pick(&["github.search_issues"])).await;
        assert_eq!(o.tools()[0].kind, ToolKind::Other);
        assert_eq!(o.tools()[0].permission, Permission::Ask);
    }

    #[tokio::test]
    async fn only_offered_run_tools_are_called() {
        let b = broker();
        let o = b.offer(&pick(&["lattice.*"])).await;
        let ran = o.call("lattice__read_file", Value::Null).await.unwrap();
        assert_eq!(ran.text, "ran read_file");
        assert!(matches!(
            o.call("github__search_issues", Value::Null).await,
            Err(ToolError::UnknownTool(_))
        ));
        assert!(matches!(
            o.call("lattice__write_file", Value::Null).await,
            Err(ToolError::Refused(_))
        ));
    }
}
