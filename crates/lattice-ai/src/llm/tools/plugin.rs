//! A plugin's tools as a [`ToolSource`] (AT.5, design
//! `llm-assistant-tools.md` §5).
//!
//! A plugin exporting `llm-tools` is wrapped by the plugin loader as a
//! [`ToolPlugin`] and registered here, so the broker offers its tools beside
//! lattice's, `web`'s and each MCP server's. The WIT and the actor live in the
//! plugin host; this module owns what is lattice's to decide:
//!
//! - **Which row a tool sits in comes from the capability grant, never from the
//!   plugin.** A plugin that can reach neither the network nor a process can
//!   only answer from what it reads, so its `read` tools run like
//!   `lattice.read`. One that can reach out could send a call's arguments
//!   anywhere, so every one of its tools asks, `propose` ones included: a
//!   review gates the edit, not the call that computed it.
//! - **A call cannot write.** It returns text and proposals. Each proposal is
//!   applied through `lattice.edit_file` itself, so it is fenced to the
//!   project, refused if stale, reviewed (or trusted with `<C-t>`), and held to
//!   `llm.deny`'s `edit_file(...)` rules like any edit the model asks for.
//! - **A plugin may not take a name already in use**: `lattice`, `web`, a
//!   configured MCP server, or another plugin. It is not offered, and the
//!   listing says why.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::broker::{LATTICE, NATIVE, Permission, ToolBrokerHandle};
use super::rules::Verdict;
use super::{BoxFuture, CallContext, SourceTool, ToolError, ToolOutput, ToolSource};
use crate::conversation::ToolKind;
use crate::llm::ToolDescriptor;

/// What a plugin tool does, as the plugin declares it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginToolKind {
    /// Answers from what it reads.
    Read,
    /// Answers with edits for the user to review.
    Propose,
}

/// One tool as a plugin lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginTool {
    pub name: String,
    pub description: String,
    /// JSON Schema for the input, as text.
    pub input_schema: String,
    pub kind: PluginToolKind,
}

/// An exact replacement, with `lattice.edit_file`'s meaning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedEdit {
    pub path: String,
    pub old_string: String,
    pub new_string: String,
}

/// What a call returns: text for the model, and edits to review.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginToolResult {
    pub text: String,
    pub proposals: Vec<ProposedEdit>,
}

/// A loaded plugin's `llm-tools` export, as the loader wraps it. Calls run on
/// the plugin's own actor; dropping the future abandons the call.
pub trait ToolPlugin: Send + Sync {
    /// The tools it offers. Read once, at registration.
    fn tools(&self) -> BoxFuture<'_, Result<Vec<PluginTool>, String>>;

    /// Run `name` with the model's input as JSON text.
    fn call(&self, name: &str, args: String) -> BoxFuture<'_, Result<PluginToolResult, String>>;
}

/// What the plugin's capability grant lets it reach, read by the loader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// No `net:*` and no `proc:spawn`.
    Contained,
    /// Either: it can send what it is given somewhere else.
    Outward,
}

/// A plugin's tools as one of the broker's sources.
pub struct PluginToolSource {
    plugin: Arc<dyn ToolPlugin>,
    tools: Vec<SourceTool>,
    reach: Reach,
    /// lattice's own tools, through whose `edit_file` a proposal applies.
    lattice: Arc<dyn ToolSource>,
}

impl PluginToolSource {
    /// The plugin's listed tools as descriptors. A tool whose name or schema
    /// cannot be offered is left out and named among the problems.
    fn describe_all(listed: Vec<PluginTool>) -> (Vec<SourceTool>, Vec<String>) {
        let mut tools = Vec::new();
        let mut problems = Vec::new();
        for t in listed {
            if t.name.is_empty()
                || t.name.contains("__")
                || !t
                    .name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                problems.push(format!(
                    "`{}`: a tool name is letters, digits, `_` and `-`, without `__`",
                    t.name
                ));
                continue;
            }
            let schema = match serde_json::from_str::<Value>(&t.input_schema) {
                Ok(v @ Value::Object(_)) => v,
                _ => {
                    problems.push(format!(
                        "`{}`: its input schema is not a JSON object",
                        t.name
                    ));
                    continue;
                }
            };
            tools.push(SourceTool {
                descriptor: ToolDescriptor {
                    name: t.name,
                    description: t.description,
                    input_schema: schema,
                },
                kind: match t.kind {
                    PluginToolKind::Read => ToolKind::Read,
                    PluginToolKind::Propose => ToolKind::Edit,
                },
            });
        }
        (tools, problems)
    }

    /// Apply one proposal as `lattice.edit_file` would: `llm.deny` first,
    /// since calling the source directly passes the broker's own check.
    async fn apply(&self, edit: &ProposedEdit, ctx: &CallContext) -> String {
        let input = json!({
            "path": edit.path,
            "old_string": edit.old_string,
            "new_string": edit.new_string,
        });
        if let Some(rules) = &ctx.rules
            && let Verdict::Deny(rule) = rules.decide(LATTICE, "edit_file", &input)
        {
            return format!("`{}`: refused by `{rule}`; nothing changed", edit.path);
        }
        match self.lattice.call("edit_file", input, ctx.clone()).await {
            Ok(out) => out.text,
            Err(e) => format!("`{}`: {e}", edit.path),
        }
    }
}

impl ToolSource for PluginToolSource {
    fn list(&self) -> BoxFuture<'_, Result<Vec<SourceTool>, ToolError>> {
        let tools = self.tools.clone();
        Box::pin(async move { Ok(tools) })
    }

    fn call(
        &self,
        name: &str,
        input: Value,
        ctx: CallContext,
    ) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
        let name = name.to_string();
        Box::pin(async move {
            if !self.tools.iter().any(|t| t.descriptor.name == name) {
                return Err(ToolError::UnknownTool(name));
            }
            let result = self
                .plugin
                .call(&name, input.to_string())
                .await
                .map_err(ToolError::Failed)?;
            let mut text = result.text;
            // One at a time: each is its own review, against the file as it
            // is once the one before it settled.
            for edit in &result.proposals {
                let outcome = self.apply(edit, &ctx).await;
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&outcome);
            }
            Ok(ToolOutput { text })
        })
    }

    fn permission(&self, name: &str) -> Option<Permission> {
        let tool = self.tools.iter().find(|t| t.descriptor.name == name)?;
        Some(row(self.reach, tool.kind))
    }
}

/// The row a plugin tool sits in: what the plugin can reach, then what the
/// tool does with its result.
fn row(reach: Reach, kind: ToolKind) -> Permission {
    match (reach, kind) {
        (Reach::Outward, _) => Permission::Ask,
        (Reach::Contained, ToolKind::Edit) => Permission::Review,
        (Reach::Contained, _) => Permission::Run,
    }
}

/// One offered plugin tool, as `*llm-tools*` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedPluginTool {
    pub name: String,
    pub description: String,
    pub kind: ToolKind,
    pub permission: Permission,
}

/// Fired when a plugin's tools are offered, refused or withdrawn, so
/// `*llm-tools*` repaints.
#[derive(Debug, Clone)]
pub struct PluginToolsChanged;

lattice_protocol::register_event!(
    PluginToolsChanged,
    "llm.plugin-tools-changed",
    "Fired when a plugin's tools for the LLM assistant are offered, refused or withdrawn.",
    "lattice-ai",
);

/// Where a plugin's tools stand, for the listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginToolsStatus {
    /// Offered, with the tools that could not be.
    Offered {
        tools: Vec<ListedPluginTool>,
        reach: Reach,
        problems: Vec<String>,
    },
    /// Not offered, and why.
    Refused(String),
}

/// The plugins whose tools the broker offers. The loader registers each
/// plugin exporting `llm-tools` and unregisters it on unload.
pub struct PluginTools {
    broker: ToolBrokerHandle,
    lattice: Arc<dyn ToolSource>,
    /// Names already taken by the user's config (`[llm.mcp.*]`), read when a
    /// plugin registers.
    configured: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    status: Mutex<BTreeMap<String, PluginToolsStatus>>,
    /// Called after the statuses change; publishes [`PluginToolsChanged`].
    changed: Arc<dyn Fn() + Send + Sync>,
}

/// Shared by the llm layer and the plugin loader.
pub type PluginToolsHandle = Arc<PluginTools>;

impl PluginTools {
    pub fn new(
        broker: ToolBrokerHandle,
        lattice: Arc<dyn ToolSource>,
        configured: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Self {
        Self {
            broker,
            lattice,
            configured,
            status: Mutex::new(BTreeMap::new()),
            changed: Arc::new(|| {}),
        }
    }

    /// Call `changed` whenever the statuses change, so a listing can repaint.
    pub fn on_change(mut self, changed: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.changed = changed;
        self
    }

    /// Offer `plugin`'s tools as `name.*`. Refused when the name is not
    /// usable or already taken, or the plugin cannot list; the refusal is
    /// listed unless the name is another plugin's, whose entry it would hide.
    pub async fn register(
        &self,
        name: &str,
        plugin: Arc<dyn ToolPlugin>,
        reach: Reach,
    ) -> Result<(), String> {
        let refuse = |why: String, record: bool| {
            tracing::warn!(target: "lattice::llm", "llm: tools from plugin `{name}` not offered: {why}");
            if record {
                self.status()
                    .insert(name.to_string(), PluginToolsStatus::Refused(why.clone()));
                (self.changed)();
            }
            Err(why)
        };
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return refuse(
                "a tool source's name is letters, digits and `-`".into(),
                true,
            );
        }
        if NATIVE.contains(&name) {
            return refuse(format!("`{name}` is lattice's own tools"), true);
        }
        if (self.configured)().iter().any(|n| n == name) {
            return refuse(format!("`{name}` is an MCP server in your config"), true);
        }
        if matches!(
            self.status().get(name),
            Some(PluginToolsStatus::Offered { .. })
        ) {
            return refuse(format!("another plugin offers tools as `{name}`"), false);
        }
        let listed = match plugin.tools().await {
            Ok(listed) => listed,
            Err(e) => return refuse(format!("it could not list its tools: {e}"), true),
        };
        let (tools, problems) = PluginToolSource::describe_all(listed);
        for p in &problems {
            tracing::warn!(target: "lattice::llm", "llm: plugin `{name}` tool left out: {p}");
        }
        let listed = tools
            .iter()
            .map(|t| ListedPluginTool {
                name: t.descriptor.name.clone(),
                description: t.descriptor.description.clone(),
                kind: t.kind,
                permission: row(reach, t.kind),
            })
            .collect();
        self.broker.add_source(
            name,
            Arc::new(PluginToolSource {
                plugin,
                tools,
                reach,
                lattice: self.lattice.clone(),
            }),
        );
        self.status().insert(
            name.to_string(),
            PluginToolsStatus::Offered {
                tools: listed,
                reach,
                problems,
            },
        );
        (self.changed)();
        Ok(())
    }

    /// Stop offering `name`'s tools: the plugin unloaded. A name this
    /// registry never offered (a refused one, or an MCP server's) is left
    /// alone in the broker. True when tools were withdrawn.
    pub fn unregister(&self, name: &str) -> bool {
        let offered = matches!(
            self.status().remove(name),
            Some(PluginToolsStatus::Offered { .. })
        );
        if offered {
            self.broker.remove_source(name);
            (self.changed)();
        }
        offered
    }

    /// Every plugin that registered, and where it stands.
    pub fn statuses(&self) -> BTreeMap<String, PluginToolsStatus> {
        self.status().clone()
    }

    fn status(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, PluginToolsStatus>> {
        self.status.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::llm::tools::ToolBroker;

    /// A plugin answering from a script: `notes` reads, `change` proposes.
    struct Fixture {
        calls: AtomicUsize,
        proposals: Vec<ProposedEdit>,
        fail: bool,
    }

    impl Fixture {
        fn new(proposals: Vec<ProposedEdit>) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                proposals,
                fail: false,
            })
        }
    }

    fn tool(name: &str, schema: &str, kind: PluginToolKind) -> PluginTool {
        PluginTool {
            name: name.into(),
            description: name.into(),
            input_schema: schema.into(),
            kind,
        }
    }

    impl ToolPlugin for Fixture {
        fn tools(&self) -> BoxFuture<'_, Result<Vec<PluginTool>, String>> {
            let object = r#"{"type":"object"}"#;
            Box::pin(async move {
                Ok(vec![
                    tool("notes", object, PluginToolKind::Read),
                    tool("change", object, PluginToolKind::Propose),
                    tool("bad__name", object, PluginToolKind::Read),
                    tool("noschema", "not json", PluginToolKind::Read),
                ])
            })
        }

        fn call(
            &self,
            name: &str,
            args: String,
        ) -> BoxFuture<'_, Result<PluginToolResult, String>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let name = name.to_string();
            Box::pin(async move {
                if self.fail {
                    return Err("trapped: unreachable".into());
                }
                Ok(PluginToolResult {
                    text: format!("{name} got {args}"),
                    proposals: if name == "change" {
                        self.proposals.clone()
                    } else {
                        Vec::new()
                    },
                })
            })
        }
    }

    /// Stands in for lattice's tools: records each `edit_file` it is asked.
    #[derive(Default)]
    struct Lattice {
        edits: Mutex<Vec<Value>>,
    }

    impl ToolSource for Lattice {
        fn list(&self) -> BoxFuture<'_, Result<Vec<SourceTool>, ToolError>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn call(
            &self,
            name: &str,
            input: Value,
            _ctx: CallContext,
        ) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
            assert_eq!(name, "edit_file");
            let path = input["path"].as_str().unwrap().to_string();
            self.edits.lock().unwrap().push(input);
            Box::pin(async move {
                if path == "stale.org" {
                    Err(ToolError::Failed(format!(
                        "`{path}` changed; read it again"
                    )))
                } else {
                    Ok(ToolOutput {
                        text: format!("wrote `{path}`"),
                    })
                }
            })
        }
    }

    fn registry(lattice: Arc<Lattice>) -> (PluginTools, ToolBrokerHandle) {
        let broker: ToolBrokerHandle = Arc::new(ToolBroker::new());
        let tools = PluginTools::new(
            broker.clone(),
            lattice,
            Arc::new(|| vec!["github".to_string()]),
        );
        (tools, broker)
    }

    fn select(s: &[&str]) -> BTreeSet<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    fn edit(path: &str) -> ProposedEdit {
        ProposedEdit {
            path: path.into(),
            old_string: "TODO".into(),
            new_string: "DONE".into(),
        }
    }

    /// The row comes from what the plugin can reach: a contained plugin's
    /// read runs and its proposal reviews; one that can reach out asks for
    /// both, since the call itself sees the arguments.
    #[tokio::test]
    async fn the_row_comes_from_the_grant_not_the_plugin() {
        let (tools, broker) = registry(Arc::default());
        tools
            .register("org", Fixture::new(vec![]), Reach::Contained)
            .await
            .unwrap();
        let offer = broker.offer(&select(&["org.*"])).await;
        let rows: Vec<_> = offer
            .tools()
            .iter()
            .map(|t| (t.wire.as_str(), t.permission, t.kind))
            .collect();
        assert_eq!(
            rows,
            [
                ("org__notes", Permission::Run, ToolKind::Read),
                ("org__change", Permission::Review, ToolKind::Edit),
            ]
        );
        // The tools that could not be offered are named.
        let Some(PluginToolsStatus::Offered { problems, .. }) = tools.statuses().remove("org")
        else {
            panic!("not offered");
        };
        assert_eq!(problems.len(), 2, "{problems:?}");

        let (tools, broker) = registry(Arc::default());
        tools
            .register("org", Fixture::new(vec![]), Reach::Outward)
            .await
            .unwrap();
        let offer = broker.offer(&select(&["org.*"])).await;
        assert_eq!(offer.tools().len(), 2);
        assert!(
            offer
                .tools()
                .iter()
                .all(|t| t.permission == Permission::Ask)
        );
    }

    /// A proposal goes through `lattice.edit_file`; a stale one is that
    /// proposal's error and the rest still apply; `llm.deny` refuses one
    /// before it reaches review.
    #[tokio::test]
    async fn proposals_apply_through_edit_file_and_deny_rules() {
        let lattice = Arc::new(Lattice::default());
        let (tools, broker) = registry(lattice.clone());
        let plugin = Fixture::new(vec![edit("a.org"), edit("stale.org"), edit("secret.org")]);
        tools
            .register("org", plugin, Reach::Contained)
            .await
            .unwrap();
        let offer = broker.offer(&select(&["org.change"])).await;
        let (rules, problems) = super::super::rules::ToolRules::new(
            std::path::Path::new("/p"),
            &[],
            &["edit_file(secret.org)".to_string()],
            &[],
        );
        assert!(problems.is_empty(), "{problems:?}");
        let ctx = CallContext {
            rules: Some(Arc::new(rules)),
            ..CallContext::default()
        };
        let out = offer
            .call("org__change", json!({"ref": "x"}), ctx)
            .await
            .unwrap()
            .text;
        assert!(out.starts_with("change got {\"ref\":\"x\"}"), "{out}");
        assert!(out.contains("wrote `a.org`"), "{out}");
        assert!(out.contains("`stale.org`: `stale.org` changed"), "{out}");
        assert!(
            out.contains("`secret.org`: refused by `edit_file(secret.org)`"),
            "{out}"
        );
        let edits = lattice.edits.lock().unwrap();
        let paths: Vec<_> = edits.iter().map(|e| e["path"].clone()).collect();
        assert_eq!(paths, [json!("a.org"), json!("stale.org")]);
        assert_eq!(edits[0]["old_string"], "TODO");
        assert_eq!(edits[0]["new_string"], "DONE");
    }

    /// A trap is that call's error, and a name it does not list is unknown
    /// without reaching the plugin.
    #[tokio::test]
    async fn a_trap_is_the_calls_error() {
        let (tools, broker) = registry(Arc::default());
        let plugin = Arc::new(Fixture {
            calls: AtomicUsize::new(0),
            proposals: vec![],
            fail: true,
        });
        tools
            .register("org", plugin.clone(), Reach::Contained)
            .await
            .unwrap();
        let offer = broker.offer(&select(&["org.*"])).await;
        let err = offer
            .call("org__notes", json!({}), CallContext::default())
            .await
            .unwrap_err();
        assert_eq!(err, ToolError::Failed("trapped: unreachable".into()));
        // Not offered (its name was refused), so it never reaches the plugin.
        let err = offer
            .call("org__bad__name", json!({}), CallContext::default())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::UnknownTool(_)));
        assert_eq!(plugin.calls.load(Ordering::SeqCst), 1);
    }

    /// A name in use is not taken over: lattice's, web's, a configured MCP
    /// server's, another plugin's. Each refusal is listed (but not over the
    /// owner's entry); unloading a refused plugin leaves the owner in place.
    #[tokio::test]
    async fn a_plugin_may_not_take_a_name_in_use() {
        let (tools, broker) = registry(Arc::default());
        for name in ["lattice", "web", "github", "bad_name", ""] {
            let err = tools
                .register(name, Fixture::new(vec![]), Reach::Contained)
                .await
                .unwrap_err();
            assert!(!err.is_empty());
        }
        tools
            .register("org", Fixture::new(vec![]), Reach::Contained)
            .await
            .unwrap();
        let err = tools
            .register("org", Fixture::new(vec![]), Reach::Outward)
            .await
            .unwrap_err();
        assert!(err.contains("another plugin"), "{err}");
        // The first plugin keeps its row and its entry.
        let offer = broker.offer(&select(&["org.notes"])).await;
        assert_eq!(offer.tools()[0].permission, Permission::Run);
        let statuses = tools.statuses();
        assert!(matches!(
            &statuses["org"],
            PluginToolsStatus::Offered {
                reach: Reach::Contained,
                ..
            }
        ));
        assert!(matches!(
            &statuses["lattice"],
            PluginToolsStatus::Refused(why) if why.contains("lattice's own")
        ));
        assert!(matches!(
            &statuses["github"],
            PluginToolsStatus::Refused(why) if why.contains("MCP server")
        ));

        tools.unregister("github");
        tools.unregister("org");
        assert!(broker.offer(&select(&["org.*"])).await.tools().is_empty());
    }
}
