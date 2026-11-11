//! `llm-tools-mode`: `*llm-tools*`, the tool sources beyond lattice's own
//! (LL.24, AT.5d; designs `llm-layer.md` §6, `llm-assistant-tools.md` §5).
//!
//! Two sections. **MCP servers**: each configured server with its state and
//! tools. A server that fails to start never blocks a request; this is where
//! the failure is seen, and where a server is started again: `r` on a server
//! (or one of its tools) restarts it, `dd` stops it, as `dd` stops a request
//! in `*llm-requests*`. **Plugins**: each plugin that offers tools, with the
//! row each tool sits in, or why it is not offered.
//!
//! **The list keeps itself current**: the mode subscribes to
//! [`McpServersChanged`] and [`PluginToolsChanged`] while the buffer is open
//! and repaints, then wakes the editor, so a server finishing its start or a
//! plugin loading shows without a keystroke. Nothing in it shows an `env` or
//! `headers` value.
//!
//! Read-only by the two declarations, as `*llm-requests*` is.

use std::sync::{Arc, OnceLock};

use lattice_grammar::effect::{EchoLevel, Effect};
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerContribution, BufferStoreHandle, CapabilitySet,
    Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};
use lattice_runtime::{Document, EventBus};

use super::requests::{BusSubscription, replace_text};
use crate::conversation::ToolKind;
use crate::llm::sink::LlmInlineHandle;
use crate::llm::tools::Permission;
use crate::llm::tools::mcp::{McpServersChanged, McpServersHandle, ServerState, ServerStatus};
use crate::llm::tools::plugin::{PluginToolsChanged, PluginToolsHandle, PluginToolsStatus, Reach};

/// The listing's buffer name.
pub const TOOLS_BUFFER: &str = "*llm-tools*";

/// `r`: restart the server on the caret's row.
pub const ACTION_RESTART: &str = "action:llm-tools-restart";

/// `dd`: stop the server on the caret's row.
pub const ACTION_STOP: &str = "action:llm-tools-stop";

pub struct LlmToolsMode {
    servers: McpServersHandle,
    plugins: PluginToolsHandle,
    bus: Arc<EventBus>,
    inline: LlmInlineHandle,
}

impl LlmToolsMode {
    pub fn new(
        servers: McpServersHandle,
        plugins: PluginToolsHandle,
        bus: Arc<EventBus>,
        inline: LlmInlineHandle,
    ) -> Self {
        Self {
            servers,
            plugins,
            bus,
            inline,
        }
    }

    pub fn mode_id() -> ModeId {
        ModeId::new("llm-tools-mode")
    }
}

/// Stops the refresher when the buffer closes or the mode is replaced.
pub struct LlmToolsGuard {
    task: tokio::task::AbortHandle,
    _subs: [BusSubscription; 2],
}

impl Drop for LlmToolsGuard {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Mode for LlmToolsMode {
    type Guard = Option<LlmToolsGuard>;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }

    fn options(&self) -> OptionOverrideSet {
        super::read_only_options()
    }

    /// Read-only by both declarations (see [`super::read_only_implied`]).
    fn implies(&self) -> &[ModeId] {
        super::read_only_implied()
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn keymap(&self) -> Keymap {
        Keymap::from_entries(keymap_entries())
    }

    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![
            ActionHandlerContribution {
                action_name: ACTION_RESTART,
                handler: row_handler(self.servers.clone(), Row::Restart),
            },
            ActionHandlerContribution {
                action_name: ACTION_STOP,
                handler: row_handler(self.servers.clone(), Row::Stop),
            },
        ]
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(None);
            };
            let Some(handle) = store.handle_for(buffer) else {
                return Ok(None);
            };
            // The layer's runtime, not the editor thread's (see
            // `LlmInline::runtime`).
            let runtime = self.inline.runtime().clone();
            // Subscribed before the first paint, so a change between the two
            // is not missed.
            let (mcp_tx, mut mcp_rx) = tokio::sync::mpsc::unbounded_channel::<McpServersChanged>();
            let (plugin_tx, mut plugin_rx) =
                tokio::sync::mpsc::unbounded_channel::<PluginToolsChanged>();
            let subs = [
                BusSubscription::new(
                    self.bus.clone(),
                    self.bus.subscribe_typed::<McpServersChanged>(mcp_tx),
                ),
                BusSubscription::new(
                    self.bus.clone(),
                    self.bus.subscribe_typed::<PluginToolsChanged>(plugin_tx),
                ),
            ];
            let servers = self.servers.clone();
            let plugins = self.plugins.clone();
            let inline = self.inline.clone();
            paint(&handle, &servers, &plugins, &inline).await;
            let task = runtime.spawn(async move {
                loop {
                    tokio::select! {
                        got = mcp_rx.recv() => if got.is_none() { break },
                        got = plugin_rx.recv() => if got.is_none() { break },
                    }
                    while mcp_rx.try_recv().is_ok() || plugin_rx.try_recv().is_ok() {}
                    paint(&handle, &servers, &plugins, &inline).await;
                }
            });
            Ok(Some(LlmToolsGuard {
                task: task.abort_handle(),
                _subs: subs,
            }))
        })
    }
}

fn keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! {
                mode: Normal, chord: "r",
                doc: "llm-tools: start the MCP server on this line again",
                cmd: "action:llm-tools-restart"
            },
            keymap_entry! {
                mode: Normal, chord: "dd",
                doc: "llm-tools: stop the MCP server on this line",
                cmd: "action:llm-tools-stop"
            },
        ]
    })
}

async fn paint(
    handle: &Arc<dyn Document>,
    servers: &McpServersHandle,
    plugins: &PluginToolsHandle,
    inline: &LlmInlineHandle,
) {
    let plugins: Vec<_> = plugins.statuses().into_iter().collect();
    if replace_text(handle, render(&servers.statuses(), &plugins)).await {
        inline.wake();
    }
}

/// The listing's text. Pure, for tests.
pub fn render(servers: &[ServerStatus], plugins: &[(String, PluginToolsStatus)]) -> String {
    let mut out = String::from(
        "Tool sources beyond lattice's own. On an MCP server, r starts it again and dd stops it.\n\n",
    );
    render_servers(&mut out, servers);
    out.push('\n');
    render_plugins(&mut out, plugins);
    out
}

fn render_servers(out: &mut String, servers: &[ServerStatus]) {
    out.push_str("MCP servers\n");
    if servers.is_empty() {
        out.push_str(
            "\nNone configured. Add an [llm.mcp.<name>] table to lattice.toml \
             (see :help llm-tools-mode), then select its tools with llm.tools = [\"<name>.*\"].\n",
        );
        return;
    }
    for s in servers {
        out.push('\n');
        out.push_str(&format!("{}  {}  {}\n", s.name, s.transport, s.state));
        if s.tools.is_empty() {
            if s.state == ServerState::Running {
                out.push_str("    (no tools)\n");
            }
            continue;
        }
        let width = s.tools.iter().map(|t| t.name.len()).max().unwrap_or(0);
        for t in &s.tools {
            let note = match (&t.skipped, t.allowed, t.read_only_claim) {
                (Some(why), _, _) => format!("not offered: {why}"),
                (None, true, true) => "allowed; read-only, by its own account".to_string(),
                (None, true, false) => "allowed".to_string(),
                (None, false, true) => "asks; read-only, by its own account".to_string(),
                (None, false, false) => "asks".to_string(),
            };
            let name = &t.name;
            let line = if t.description.is_empty() {
                format!("    {name:<width$}  [{note}]")
            } else {
                format!("    {name:<width$}  [{note}]  {}", t.description)
            };
            out.push_str(line.trim_end());
            out.push('\n');
        }
    }
}

fn render_plugins(out: &mut String, plugins: &[(String, PluginToolsStatus)]) {
    out.push_str("Plugins\n");
    if plugins.is_empty() {
        out.push_str("\nNo plugin offers tools.\n");
        return;
    }
    for (name, status) in plugins {
        out.push('\n');
        match status {
            PluginToolsStatus::Refused(why) => {
                out.push_str(&format!("{name}  plugin  not offered: {why}\n"));
            }
            PluginToolsStatus::Offered {
                tools,
                reach,
                problems,
                profiles,
            } => {
                out.push_str(&format!("{name}  plugin  offered"));
                if *reach == Reach::Outward {
                    out.push_str("; it can reach the network or run programs, so every tool asks");
                }
                out.push('\n');
                let width = tools.iter().map(|t| t.name.len()).max().unwrap_or(0);
                for t in tools {
                    let note = match (t.permission, t.kind) {
                        (Permission::Ask, ToolKind::Edit) => "asks, then reviewed",
                        (Permission::Ask, _) => "asks",
                        (Permission::Review, _) => "reviewed",
                        (Permission::Run, _) => "runs",
                    };
                    let line = format!("    {:<width$}  [{note}]  {}", t.name, t.description);
                    out.push_str(line.trim_end());
                    out.push('\n');
                }
                for (profile, description) in profiles {
                    let line = format!("    profile {profile}  {description}");
                    out.push_str(line.trim_end());
                    out.push('\n');
                }
                for p in problems {
                    out.push_str(&format!("    not offered: {p}\n"));
                }
            }
        }
    }
}

/// The server a row belongs to: the nearest unindented line at or above
/// `line`, by its first word. `None` above the first server.
pub fn server_on(lines: &[&str], line: usize) -> Option<String> {
    lines
        .iter()
        .take(line + 1)
        .rev()
        .find(|l| !l.is_empty() && !l.starts_with(' '))
        .and_then(|l| l.split_whitespace().next())
        .map(str::to_string)
}

#[derive(Clone, Copy)]
enum Row {
    Restart,
    Stop,
}

fn row_handler(servers: McpServersHandle, row: Row) -> ActionHandler {
    Arc::new(move |ctx: &ActionContext<'_>| -> Option<Effect> {
        let store = ctx.services.get::<BufferStoreHandle>()?;
        let handle = store.handle_for(lattice_core::BufferId(ctx.buffer_id.0 as u32))?;
        let text = handle.snapshot().buffer.as_string();
        let lines: Vec<&str> = text.lines().collect();
        let server = server_on(&lines, ctx.cursor.line as usize)
            .and_then(|name| servers.get(&name).cloned());
        let Some(server) = server else {
            return Some(say(EchoLevel::Info, "llm: no MCP server on this line"));
        };
        let name = server.name().to_string();
        Some(match row {
            Row::Restart => {
                server.restart();
                say(
                    EchoLevel::Info,
                    &format!("llm: starting MCP server `{name}`"),
                )
            }
            Row::Stop => {
                server.stop();
                say(
                    EchoLevel::Info,
                    &format!("llm: stopped MCP server `{name}`"),
                )
            }
        })
    })
}

fn say(level: EchoLevel, text: &str) -> Effect {
    Effect::Echo {
        level,
        text: text.to_string(),
    }
}

/// `:llm-tools`: open the listing, focused.
pub fn open() -> Effect {
    Effect::OpenSyntheticBuffer {
        name: TOOLS_BUFFER.to_string(),
        mode_id: LlmToolsMode::mode_id().as_str().to_string(),
        content: None,
        cursor: None,
        activate_minor: None,
    }
}

/// Declare the chords' actions, `:llm-tools` and its old name `:llm-mcp`;
/// the bodies are the mode's.
pub fn register(registry: &mut lattice_grammar::CommandRegistry) {
    use lattice_grammar::command::LatencyClass;
    use lattice_grammar::registry::{ActionSpec, ExCommandSpec, SurfaceForm};
    for (name, doc) in [
        (
            ACTION_RESTART,
            "llm-tools: start the MCP server on this line again.",
        ),
        (ACTION_STOP, "llm-tools: stop the MCP server on this line."),
    ] {
        registry.register_action(
            name,
            doc,
            ActionSpec {
                apply: Arc::new(|_| Ok(Effect::None)),
                args_schema: vec![],
            },
        );
    }
    // `:llm-mcp` was the listing's name while it listed MCP servers only;
    // kept for a release so the muscle memory still opens it.
    for (name, doc) in [
        (
            "llm-tools",
            "List the tool sources beyond lattice's own: each MCP server under [llm.mcp] with \
             its state and tools, and each plugin that offers tools. `r` on a server starts it \
             again, `dd` stops it.",
        ),
        (
            "llm-mcp",
            "The old name of :llm-tools; opens the same listing.",
        ),
    ] {
        registry.register_ex_command(
            name,
            doc,
            ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                parse_args: Arc::new(lattice_agent::parse_no_args),
                apply: Arc::new(|_ctx| Ok(open())),
                args_schema: vec![],
                surface_form: SurfaceForm::Keyword,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::tools::mcp::ListedTool;
    use crate::llm::tools::plugin::ListedPluginTool;

    fn tool(name: &str, allowed: bool, ro: bool, skipped: Option<&str>) -> ListedTool {
        ListedTool {
            name: name.to_string(),
            description: format!("{name} things"),
            read_only_claim: ro,
            allowed,
            skipped: skipped.map(str::to_string),
        }
    }

    fn plugin_tool(name: &str, kind: ToolKind, permission: Permission) -> ListedPluginTool {
        ListedPluginTool {
            name: name.to_string(),
            description: format!("{name} things"),
            kind,
            permission,
        }
    }

    const TITLE: &str = "Tool sources beyond lattice's own. On an MCP server, r starts it again and dd stops it.\n\n";

    #[test]
    fn servers_list_with_their_state_and_each_tool_s_permission() {
        let text = render(
            &[
                ServerStatus {
                    name: "github".into(),
                    transport: "stdio: github-mcp-server".into(),
                    state: ServerState::Running,
                    tools: vec![
                        tool("get_issue", true, false, None),
                        tool("search", false, true, None),
                        tool("a.b", false, false, Some("bad name")),
                    ],
                },
                ServerStatus {
                    name: "docs".into(),
                    transport: "http: https://example.com/mcp".into(),
                    state: ServerState::Failed("command not found".into()),
                    tools: vec![],
                },
            ],
            &[],
        );
        assert_eq!(
            text,
            format!(
                "{TITLE}MCP servers\n\
                 \n\
                 github  stdio: github-mcp-server  running\n    \
                 get_issue  [allowed]  get_issue things\n    \
                 search     [asks; read-only, by its own account]  search things\n    \
                 a.b        [not offered: bad name]  a.b things\n\
                 \n\
                 docs  http: https://example.com/mcp  failed: command not found\n\
                 \n\
                 Plugins\n\
                 \n\
                 No plugin offers tools.\n"
            )
        );
    }

    /// Each plugin with the row each tool sits in, the tools it could not
    /// offer, and a plugin refused with its reason.
    #[test]
    fn plugins_list_with_each_tool_s_row_or_why_they_are_not_offered() {
        let text = render(
            &[],
            &[
                (
                    "jira".into(),
                    PluginToolsStatus::Offered {
                        tools: vec![
                            plugin_tool("issue", ToolKind::Read, Permission::Ask),
                            plugin_tool("close", ToolKind::Edit, Permission::Ask),
                        ],
                        reach: Reach::Outward,
                        problems: vec![],
                        profiles: vec![],
                    },
                ),
                (
                    "org".into(),
                    PluginToolsStatus::Offered {
                        tools: vec![
                            plugin_tool("agenda", ToolKind::Read, Permission::Run),
                            plugin_tool("change", ToolKind::Edit, Permission::Review),
                        ],
                        reach: Reach::Contained,
                        problems: vec!["`a.b`: bad name".into()],
                        profiles: vec![("org-plan".into(), "Plan a day or a week".into())],
                    },
                ),
                (
                    "web".into(),
                    PluginToolsStatus::Refused("`web` is lattice's own tools".into()),
                ),
            ],
        );
        let plugins = text.split("Plugins\n").nth(1).unwrap();
        assert_eq!(
            plugins,
            "\n\
             jira  plugin  offered; it can reach the network or run programs, so every tool asks\n    \
             issue  [asks]  issue things\n    \
             close  [asks, then reviewed]  close things\n\
             \n\
             org  plugin  offered\n    \
             agenda  [runs]  agenda things\n    \
             change  [reviewed]  change things\n    \
             profile org-plan  Plan a day or a week\n    \
             not offered: `a.b`: bad name\n\
             \n\
             web  plugin  not offered: `web` is lattice's own tools\n"
        );
    }

    #[test]
    fn no_servers_says_how_to_add_one() {
        assert!(render(&[], &[]).contains("[llm.mcp.<name>]"));
    }

    #[test]
    fn a_tool_row_belongs_to_the_server_above_it() {
        let text = render(
            &[ServerStatus {
                name: "github".into(),
                transport: "stdio: x".into(),
                state: ServerState::Running,
                tools: vec![tool("get_issue", false, false, None)],
            }],
            &[],
        );
        let lines: Vec<&str> = text.lines().collect();
        let server = lines.iter().position(|l| l.starts_with("github")).unwrap();
        assert_eq!(server_on(&lines, server).as_deref(), Some("github"));
        assert_eq!(server_on(&lines, server + 1).as_deref(), Some("github"));
        // A heading is not a configured server; the handler finds none.
        assert_eq!(server_on(&lines, 2).as_deref(), Some("MCP"));
    }
}
