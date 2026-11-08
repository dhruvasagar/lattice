//! `llm-mcp-mode`: `*llm-mcp*`, every configured MCP server with its state
//! and tools (LL.24, design `llm-layer.md` §6).
//!
//! A server that fails to start never blocks a request; this is where the
//! failure is seen, and where a server is started again. `r` on a server (or
//! one of its tools) restarts it, `dd` stops it, as `dd` stops a request in
//! `*llm-requests*`.
//!
//! **The list keeps itself current**: the mode subscribes to
//! [`McpServersChanged`] while the buffer is open and repaints from the
//! servers' status, then wakes the editor, so a server finishing its start
//! shows without a keystroke. Nothing in it shows an `env` or `headers`
//! value.
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

use super::requests::replace_text;
use crate::llm::sink::LlmInlineHandle;
use crate::llm::tools::mcp::{McpServersChanged, McpServersHandle, ServerState, ServerStatus};

/// The listing's buffer name.
pub const MCP_BUFFER: &str = "*llm-mcp*";

/// `r`: restart the server on the caret's row.
pub const ACTION_RESTART: &str = "action:llm-mcp-restart";

/// `dd`: stop the server on the caret's row.
pub const ACTION_STOP: &str = "action:llm-mcp-stop";

pub struct LlmMcpMode {
    servers: McpServersHandle,
    bus: Arc<EventBus>,
    inline: LlmInlineHandle,
}

impl LlmMcpMode {
    pub fn new(servers: McpServersHandle, bus: Arc<EventBus>, inline: LlmInlineHandle) -> Self {
        Self {
            servers,
            bus,
            inline,
        }
    }

    pub fn mode_id() -> ModeId {
        ModeId::new("llm-mcp-mode")
    }
}

/// Stops the refresher when the buffer closes or the mode is replaced.
pub struct LlmMcpGuard {
    task: tokio::task::AbortHandle,
}

impl Drop for LlmMcpGuard {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Mode for LlmMcpMode {
    type Guard = Option<LlmMcpGuard>;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }

    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
        }
    }

    /// The second declaration: `ReadOnly` gates typing only; `read-only-mode`
    /// carries the runner that refuses `x`, `p` and the other operators.
    fn implies(&self) -> &[ModeId] {
        static IMPLIED: OnceLock<Vec<ModeId>> = OnceLock::new();
        IMPLIED.get_or_init(|| vec![lattice_mode::modes::ReadOnlyMode::mode_id()])
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
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                return Ok(None);
            };
            // Subscribed before the first paint, so a change between the two
            // is not missed.
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<McpServersChanged>();
            self.bus.subscribe_typed::<McpServersChanged>(tx);
            let servers = self.servers.clone();
            let inline = self.inline.clone();
            paint(&handle, &servers, &inline).await;
            let task = runtime.spawn(async move {
                while rx.recv().await.is_some() {
                    while rx.try_recv().is_ok() {}
                    paint(&handle, &servers, &inline).await;
                }
            });
            Ok(Some(LlmMcpGuard {
                task: task.abort_handle(),
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
                doc: "llm-mcp: start the server on this line again",
                cmd: "action:llm-mcp-restart"
            },
            keymap_entry! {
                mode: Normal, chord: "dd",
                doc: "llm-mcp: stop the server on this line",
                cmd: "action:llm-mcp-stop"
            },
        ]
    })
}

async fn paint(handle: &Arc<dyn Document>, servers: &McpServersHandle, inline: &LlmInlineHandle) {
    if replace_text(handle, render(&servers.statuses())).await {
        inline.wake();
    }
}

/// The listing's text. Pure, for tests.
pub fn render(servers: &[ServerStatus]) -> String {
    if servers.is_empty() {
        return "No MCP servers are configured.\n\nAdd an [llm.mcp.<name>] table to lattice.toml \
                (see :help llm-mcp-mode), then select its tools with llm.tools = [\"<name>.*\"].\n"
            .to_string();
    }
    let mut out = String::from("MCP servers: r starts the server on the line again, dd stops it\n");
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
    out
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

/// `:llm-mcp`: open the listing, focused.
pub fn open() -> Effect {
    Effect::OpenSyntheticBuffer {
        name: MCP_BUFFER.to_string(),
        mode_id: LlmMcpMode::mode_id().as_str().to_string(),
        content: None,
        cursor: None,
        activate_minor: None,
    }
}

/// Declare the chords' actions and `:llm-mcp`; the bodies are the mode's.
pub fn register(registry: &mut lattice_grammar::CommandRegistry) {
    use lattice_grammar::command::LatencyClass;
    use lattice_grammar::registry::{ActionSpec, ExCommandSpec, SurfaceForm};
    for (name, doc) in [
        (
            ACTION_RESTART,
            "llm-mcp: start the server on this line again.",
        ),
        (ACTION_STOP, "llm-mcp: stop the server on this line."),
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
    registry.register_ex_command(
        "llm-mcp",
        "List the MCP servers configured under [llm.mcp], each with its state and tools. \
         `r` on a server starts it again, `dd` stops it.",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::tools::mcp::ListedTool;

    fn tool(name: &str, allowed: bool, ro: bool, skipped: Option<&str>) -> ListedTool {
        ListedTool {
            name: name.to_string(),
            description: format!("{name} things"),
            read_only_claim: ro,
            allowed,
            skipped: skipped.map(str::to_string),
        }
    }

    #[test]
    fn servers_list_with_their_state_and_each_tool_s_permission() {
        let text = render(&[
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
        ]);
        assert_eq!(
            text,
            "MCP servers: r starts the server on the line again, dd stops it\n\
             \n\
             github  stdio: github-mcp-server  running\n    \
             get_issue  [allowed]  get_issue things\n    \
             search     [asks; read-only, by its own account]  search things\n    \
             a.b        [not offered: bad name]  a.b things\n\
             \n\
             docs  http: https://example.com/mcp  failed: command not found\n"
        );
    }

    #[test]
    fn no_servers_says_how_to_add_one() {
        assert!(render(&[]).contains("[llm.mcp.<name>]"));
    }

    #[test]
    fn a_tool_row_belongs_to_the_server_above_it() {
        let text = render(&[ServerStatus {
            name: "github".into(),
            transport: "stdio: x".into(),
            state: ServerState::Running,
            tools: vec![tool("get_issue", false, false, None)],
        }]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(server_on(&lines, 2).as_deref(), Some("github"));
        assert_eq!(server_on(&lines, 3).as_deref(), Some("github"));
        // The title is not a configured server; the handler finds none.
        assert_eq!(server_on(&lines, 0).as_deref(), Some("MCP"));
    }
}
