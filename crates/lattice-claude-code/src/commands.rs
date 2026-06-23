//! Ex-commands owned by the Claude Code IDE peer.
//!
//! `:claude-code-start` / `:claude-code-stop` control the IDE server's
//! lifecycle. Per `feedback_mode_owns_its_surface`, BOTH the binding (the
//! command name) AND the handler body live in this crate: the `apply`
//! closure captures the [`ClaudeCodeServerHandle`] and drives it directly
//! (a non-blocking `cmd_tx` send), returning an `Effect::Echo` for user
//! feedback. The host's only role is calling
//! [`register_claude_code_ex_commands`] once at boot.
//!
//! The names are registered **bare** (no `ex:` namespace prefix), so they
//! resolve directly via `id_by_name` on the `:` line with no host
//! alias-table entry — the command surface is fully crate-owned. They are
//! `CommandKind::ExCommand`, so they enumerate in completion / `:apropos`
//! and obey the dashed + namespaced naming rule (like `lsp-format`).
//!
//! Why an `apply` closure that captures a handle rather than a mode
//! `ActionHandler`: the `:` line rejects `CommandKind::Action`
//! (`excommand.rs`), and an ex-command `apply` (`Fn(&ExCommandContext) ->
//! Effect`) gets no `services`, so it cannot reach the
//! `ActionHandlerRegistry`. Capturing the subsystem handle is the
//! mode-ownership-compliant route — it keeps the handler body in the
//! crate without a new host `Effect` variant. See design §2.

use lattice_grammar::args::Args;
use lattice_grammar::command::LatencyClass;
use lattice_grammar::effect::{EchoLevel, Effect};
use lattice_grammar::error::{CommandError, GrammarResult};
use lattice_grammar::registry::{CommandRegistry, ExCommandSpec, SurfaceForm};

use crate::server::ClaudeCodeServerHandle;

/// Reject any trailing characters; these commands take no arguments.
fn parse_no_args(rest: &str, _bang: bool) -> GrammarResult<Args> {
    if rest.trim().is_empty() {
        Ok(Args::None)
    } else {
        Err(CommandError::BadArgs(
            "trailing characters after command".into(),
        ))
    }
}

/// Register `:claude-code-start` / `:claude-code-stop` against `registry`,
/// wiring each to `server`. Called once from editor boot.
pub fn register_claude_code_ex_commands(
    registry: &mut CommandRegistry,
    server: ClaudeCodeServerHandle,
) {
    let start_server = server.clone();
    registry.register_ex_command(
        "claude-code-start",
        "Start the Claude Code IDE server (loopback WebSocket + discovery \
         lockfile) so an external `claude` CLI can attach.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(move |_ctx| {
                start_server.start();
                Ok(Effect::Echo {
                    level: EchoLevel::Info,
                    text: "claude-code: starting IDE server".to_string(),
                })
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );

    let stop_server = server;
    registry.register_ex_command(
        "claude-code-stop",
        "Stop the Claude Code IDE server and remove its discovery lockfile.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(move |_ctx| {
                stop_server.stop();
                Ok(Effect::Echo {
                    level: EchoLevel::Info,
                    text: "claude-code: stopping IDE server".to_string(),
                })
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
}
