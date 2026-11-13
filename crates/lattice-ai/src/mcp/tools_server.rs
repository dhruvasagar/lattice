//! AT.14: plugins' tools served to agents outside the editor, over MCP.
//!
//! An agent the user runs beside lattice (Claude Code, Codex, an ACP agent
//! lattice started) reaches org's tools, or any plugin's, through this
//! server: it lists every tool the loaded plugins offer, under the same wire
//! names a chat sees (`org__agenda`), and runs each call through the broker
//! exactly as a chat's would run, with what lattice decides about the caller:
//!
//! - **Untrusted.** A proposal always opens a diff review in the editor, and
//!   the call waits for the user's decision; `<C-t>` belongs to a chat and
//!   no outside agent has it.
//! - **Nothing can ask.** A tool in the `Ask` row (a plugin that can reach
//!   the network or a process) is refused: the question would have nowhere
//!   to appear that the agent's user is looking at. Org's tools never ask.
//! - **The user's rules hold.** `llm.deny` refuses and `llm.allow` spares, as
//!   for a chat.
//!
//! Only plugins' tools are served. Lattice's own (`read_file`, `edit_file`,
//! `run_command`) and `web.*` duplicate what every agent already has, and
//! serving them would hand an outside process lattice's command runner.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler};
use serde_json::{Value, json};

use crate::llm::tools::plugin::{PluginToolsHandle, PluginToolsStatus};
use crate::llm::tools::rules::ToolRules;
use crate::llm::tools::{CallContext, Offer, ToolBrokerHandle, ToolError};

/// Where an outside agent's review tags start: above an IDE connection's ids
/// (from 1) and an ACP session's, below a chat's (`1 << 62`), so closing one
/// connection's reviews never closes another's.
pub const ORIGIN_BASE: u64 = 1 << 61;

static NEXT_ORIGIN: AtomicU64 = AtomicU64::new(1);

/// A fresh review tag for one connection.
pub fn next_origin() -> u64 {
    ORIGIN_BASE + NEXT_ORIGIN.fetch_add(1, Ordering::Relaxed)
}

/// `llm.allow` and `llm.deny` as they are now. `None` is no rules.
pub type RulesFn = Arc<dyn Fn() -> Option<Arc<ToolRules>> + Send + Sync>;

/// Told an agent what the server is, at `initialize`.
const INSTRUCTIONS: &str = "Tools from the plugins loaded in a running lattice editor, \
named `<plugin>__<tool>`. A tool that proposes a change opens a diff review in the editor \
and waits for the user to accept or reject it; its result says which. A tool that would \
need the user's approval before running is refused here.";

/// What every connection shares.
struct Shared {
    broker: ToolBrokerHandle,
    plugins: PluginToolsHandle,
    rules: RulesFn,
}

/// The server for one connection.
#[derive(Clone)]
pub struct PluginToolsServer {
    shared: Arc<Shared>,
    /// This connection's review tag ([`next_origin`]).
    origin: u64,
}

impl PluginToolsServer {
    pub fn new(
        broker: ToolBrokerHandle,
        plugins: PluginToolsHandle,
        rules: RulesFn,
        origin: u64,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                broker,
                plugins,
                rules,
            }),
            origin,
        }
    }

    /// The same tools for another connection, tagged `origin`.
    pub fn for_connection(&self, origin: u64) -> Self {
        Self {
            shared: self.shared.clone(),
            origin,
        }
    }

    pub fn origin(&self) -> u64 {
        self.origin
    }

    /// `<plugin>.*` for every plugin whose tools are offered now: read per
    /// call, so a plugin loaded after the agent connected is there.
    fn selection(&self) -> BTreeSet<String> {
        self.shared
            .plugins
            .statuses()
            .into_iter()
            .filter(|(_, status)| matches!(status, PluginToolsStatus::Offered { .. }))
            .map(|(name, _)| format!("{name}.*"))
            .collect()
    }

    async fn offer(&self) -> Offer {
        self.shared.broker.offer(&self.selection()).await
    }

    /// The tools, under their wire names. One that does not map to MCP's
    /// shape is logged and left out.
    pub async fn tools(&self) -> Vec<Tool> {
        self.offer()
            .await
            .descriptors()
            .into_iter()
            .filter_map(|d| {
                let v = json!({
                    "name": d.name,
                    "description": d.description,
                    "inputSchema": d.input_schema,
                });
                serde_json::from_value::<Tool>(v)
                    .inspect_err(|e| {
                        tracing::debug!(target: "lattice::llm", "llm: tool `{}` not served: {e}", d.name)
                    })
                    .ok()
            })
            .collect()
    }

    /// Run `name` for this connection. A name no plugin offers is a protocol
    /// error; every other failure is a result the agent reads.
    pub async fn call(&self, name: &str, arguments: Value) -> Result<CallToolResult, ErrorData> {
        let ctx = CallContext {
            origin: self.origin,
            trusted: false,
            ask: None,
            rules: (self.shared.rules)(),
            ..CallContext::default()
        };
        match self.offer().await.call(name, arguments, ctx).await {
            Ok(out) => Ok(CallToolResult::success(vec![ContentBlock::text(out.text)])),
            Err(ToolError::UnknownTool(name)) => Err(ErrorData::invalid_params(
                format!("no tool named `{name}`"),
                None,
            )),
            Err(e) => Ok(CallToolResult::error(vec![ContentBlock::text(
                e.to_string(),
            )])),
        }
    }
}

impl ServerHandler for PluginToolsServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .build(),
        )
        .with_server_info(Implementation::new("lattice", env!("CARGO_PKG_VERSION")))
        .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools().await))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let arguments = request.arguments.map(Value::Object).unwrap_or(json!({}));
        // A cancelled call is dropped, which closes a review it holds open.
        tokio::select! {
            r = self.call(&request.name, arguments) => r.map(CallToolResponse::from),
            () = context.ct.cancelled() => Err(ErrorData::internal_error("cancelled", None)),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use std::sync::Mutex;

    use super::*;
    use crate::llm::tools::plugin::{
        PluginTool, PluginToolKind, PluginToolResult, PluginTools, ProposedEdit, Reach, ToolPlugin,
    };
    use crate::llm::tools::{BoxFuture, SourceTool, ToolBroker, ToolOutput, ToolSource};

    /// `agenda` reads; `change` proposes one edit.
    pub(crate) struct Org;

    impl ToolPlugin for Org {
        fn tools(&self) -> BoxFuture<'_, Result<Vec<PluginTool>, String>> {
            let tool = |name: &str, kind| PluginTool {
                name: name.into(),
                description: format!("the {name} tool"),
                input_schema: r#"{"type":"object"}"#.into(),
                kind,
            };
            Box::pin(async move {
                Ok(vec![
                    tool("agenda", PluginToolKind::Read),
                    tool("change", PluginToolKind::Propose),
                ])
            })
        }

        fn call(
            &self,
            name: &str,
            args: String,
        ) -> BoxFuture<'_, Result<PluginToolResult, String>> {
            let name = name.to_string();
            Box::pin(async move {
                Ok(PluginToolResult {
                    text: format!("{name} got {args}"),
                    proposals: if name == "change" {
                        vec![ProposedEdit {
                            path: "/org/todo.org".into(),
                            old_string: "TODO".into(),
                            new_string: "DONE".into(),
                        }]
                    } else {
                        Vec::new()
                    },
                })
            })
        }
    }

    /// Lattice's tools, as far as a test needs: `read_file` is listed, and
    /// each `edit_file` records the context it was called with.
    #[derive(Default)]
    struct Lattice {
        edits: Mutex<Vec<CallContext>>,
    }

    impl ToolSource for Lattice {
        fn list(&self) -> BoxFuture<'_, Result<Vec<SourceTool>, ToolError>> {
            Box::pin(async {
                Ok(vec![SourceTool {
                    descriptor: crate::llm::ToolDescriptor {
                        name: "read_file".into(),
                        description: "read".into(),
                        input_schema: json!({"type": "object"}),
                    },
                    kind: crate::conversation::ToolKind::Read,
                }])
            })
        }

        fn call(
            &self,
            _name: &str,
            _input: Value,
            ctx: CallContext,
        ) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
            self.edits.lock().unwrap().push(ctx);
            Box::pin(async {
                Ok(ToolOutput {
                    text: "the user accepted the edit".into(),
                })
            })
        }
    }

    struct Fixture {
        server: PluginToolsServer,
        plugins: PluginToolsHandle,
        lattice: Arc<Lattice>,
    }

    async fn fixture(reach: Reach) -> Fixture {
        let broker: ToolBrokerHandle = Arc::new(ToolBroker::new());
        let lattice = Arc::new(Lattice::default());
        broker.add_source("lattice", lattice.clone());
        let plugins: PluginToolsHandle = Arc::new(PluginTools::new(
            broker.clone(),
            lattice.clone(),
            Arc::new(Vec::new),
        ));
        plugins.register("org", Arc::new(Org), reach).await.unwrap();
        let server = PluginToolsServer::new(broker, plugins.clone(), Arc::new(|| None), 0)
            .for_connection(next_origin());
        Fixture {
            server,
            plugins,
            lattice,
        }
    }

    fn text(r: &CallToolResult) -> String {
        serde_json::to_value(r).unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// The plugins' tools, under a chat's wire names; lattice's own are not
    /// served.
    #[tokio::test]
    async fn only_plugin_tools_are_served_under_their_wire_names() {
        let f = fixture(Reach::Contained).await;
        let names: Vec<String> = f
            .server
            .tools()
            .await
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        assert_eq!(names, ["org__agenda", "org__change"]);
    }

    /// A plugin that unloads is gone from the next listing, and one that
    /// loads afterwards is in it.
    #[tokio::test]
    async fn the_listing_follows_plugins_loading_and_unloading() {
        let f = fixture(Reach::Contained).await;
        f.plugins.unregister("org");
        assert!(f.server.tools().await.is_empty());
        f.plugins
            .register("org", Arc::new(Org), Reach::Contained)
            .await
            .unwrap();
        assert_eq!(f.server.tools().await.len(), 2);
    }

    /// A proposal goes to review untrusted, tagged with this connection, and
    /// with nothing to ask through: an outside agent cannot skip the review.
    #[tokio::test]
    async fn a_proposal_is_reviewed_under_the_connections_tag() {
        let f = fixture(Reach::Contained).await;
        let r = f.server.call("org__change", json!({})).await.unwrap();
        assert_eq!(r.is_error, Some(false));
        assert!(
            text(&r).contains("the user accepted the edit"),
            "{}",
            text(&r)
        );
        let edits = f.lattice.edits.lock().unwrap();
        let [ctx] = edits.as_slice() else {
            panic!("one edit, got {}", edits.len());
        };
        assert!(!ctx.trusted);
        assert!(ctx.ask.is_none());
        assert_eq!(ctx.origin, f.server.origin());
        assert!(ctx.origin > ORIGIN_BASE && ctx.origin < crate::llm::chat::ORIGIN_BASE);
    }

    /// A read answers with the plugin's text.
    #[tokio::test]
    async fn a_read_answers_with_the_plugins_text() {
        let f = fixture(Reach::Contained).await;
        let r = f
            .server
            .call("org__agenda", json!({"span": "today"}))
            .await
            .unwrap();
        assert_eq!(text(&r), r#"agenda got {"span":"today"}"#);
    }

    /// A tool that must ask is refused, as a result the agent reads, and the
    /// plugin is never called.
    #[tokio::test]
    async fn a_tool_that_must_ask_is_refused() {
        let f = fixture(Reach::Outward).await;
        let r = f.server.call("org__agenda", json!({})).await.unwrap();
        assert_eq!(r.is_error, Some(true));
        assert!(text(&r).contains("nothing here can ask"), "{}", text(&r));
    }

    /// Lattice's own tools are not callable here even by name, and an
    /// unknown name is the protocol's invalid-params error.
    #[tokio::test]
    async fn lattices_own_tools_cannot_be_called() {
        let f = fixture(Reach::Contained).await;
        let e = f
            .server
            .call("lattice__read_file", json!({}))
            .await
            .unwrap_err();
        assert_eq!(e.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(f.lattice.edits.lock().unwrap().is_empty());
    }

    /// Each connection gets its own tag.
    #[test]
    fn origins_are_distinct_and_in_their_own_range() {
        let (a, b) = (next_origin(), next_origin());
        assert_ne!(a, b);
        assert!(a > ORIGIN_BASE && b > ORIGIN_BASE);
    }
}
