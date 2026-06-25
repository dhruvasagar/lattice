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

    // I5.1: `:claude` — launch the agent CLI wired to this editor. Starts the
    // IDE server (pre-bind → port), then emits `Effect::SpawnTerminal` so the
    // host spawns `claude` in a terminal buffer with `CLAUDE_CODE_SSE_PORT` +
    // `ENABLE_IDE_INTEGRATION` injected (so the agent connects back) and
    // `claude-code-mode` activated. Mode-ownership-compliant: the binding +
    // the body both live here; the host action is requested via the Effect
    // vocabulary (the host boundary), not a bespoke channel.
    let claude_server = server.clone();
    registry.register_ex_command(
        "claude",
        "Launch the `claude` agent CLI in a terminal buffer wired to this \
         editor's IDE server: starts the server, injects CLAUDE_CODE_SSE_PORT + \
         ENABLE_IDE_INTEGRATION, and activates claude-code-mode on the terminal.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(move |_ctx| {
                let Some(port) = claude_server.start() else {
                    return Ok(Effect::Echo {
                        level: EchoLevel::Error,
                        text: "claude: failed to start the IDE server".to_string(),
                    });
                };
                Ok(Effect::SpawnTerminal {
                    cmd_line: Some("claude".to_string()),
                    env: vec![
                        ("CLAUDE_CODE_SSE_PORT".to_string(), port.to_string()),
                        ("ENABLE_IDE_INTEGRATION".to_string(), "true".to_string()),
                    ],
                    activate_minor: Some("claude-code-mode".to_string()),
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::server::{self, ServerConfig};
    use lattice_grammar::registry::ExCommandContext;
    use lattice_grammar::{CancellationToken, Count, Register};
    use lattice_runtime::EventBus;
    use std::sync::Arc;

    /// The `:claude` apply ignores its context, so any valid one works.
    fn empty_ctx() -> ExCommandContext {
        ExCommandContext {
            bang: false,
            args: Args::None,
            range: None,
            register: Register::default(),
            count: Count::default(),
            cancel: CancellationToken::new(),
        }
    }

    /// I5.1: `:claude` starts the IDE server and emits an `Effect::SpawnTerminal`
    /// that launches `claude` with `CLAUDE_CODE_SSE_PORT` (the bound port) +
    /// `ENABLE_IDE_INTEGRATION=true` injected and `claude-code-mode` activated.
    #[tokio::test]
    async fn claude_command_starts_server_and_emits_spawn_terminal() {
        let mut registry = CommandRegistry::new();
        let handle = server::spawn(
            ServerConfig {
                workspace_folders: vec![],
                lock_dir: std::env::temp_dir(),
            },
            Arc::new(EventBus::new()),
            &tokio::runtime::Handle::current(),
        );
        register_claude_code_ex_commands(&mut registry, handle.clone());

        let id = registry.id_by_name("claude").expect("`:claude` is registered");
        let spec = registry.ex_command_spec(id).expect("spec present");
        let effect = (spec.apply)(&empty_ctx()).expect("apply ok");

        match effect {
            Effect::SpawnTerminal {
                cmd_line,
                env,
                activate_minor,
            } => {
                assert_eq!(cmd_line.as_deref(), Some("claude"));
                let port = env
                    .iter()
                    .find(|(k, _)| k == "CLAUDE_CODE_SSE_PORT")
                    .expect("CLAUDE_CODE_SSE_PORT injected");
                assert!(port.1.parse::<u16>().is_ok(), "port is numeric: {}", port.1);
                assert!(
                    env.iter()
                        .any(|(k, v)| k == "ENABLE_IDE_INTEGRATION" && v == "true"),
                    "ENABLE_IDE_INTEGRATION=true injected"
                );
                assert_eq!(activate_minor.as_deref(), Some("claude-code-mode"));
            }
            other => panic!("expected SpawnTerminal, got {other:?}"),
        }

        // `:claude` started the server: it's now running on the bound port.
        let snap = handle.snapshot();
        assert!(snap.running, "server running after :claude");
        assert!(snap.port.is_some(), "bound port recorded");
        handle.stop();
    }
}
