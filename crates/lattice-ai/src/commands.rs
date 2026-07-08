//! Ex-commands owned by the AI (ACP agent) subsystem.
//!
//! `:opencode` / `:ai-prompt` / `:ai-stop` control the AI agent's lifecycle.
//! Per `feedback_mode_owns_its_surface`, BOTH the binding (the command name)
//! AND the handler body live in this crate: each `apply` closure captures
//! the [`AiClientHandle`] and drives it directly (a non-blocking `cmd_tx`
//! send), returning an `Effect::Echo` for user feedback. The host's only
//! role is calling [`register_ai_ex_commands`] once at boot.
//!
//! `:ai-log` (opening the per-process log buffer) is intentionally NOT
//! registered here -- it needs host buffer-opening support and is handled
//! by a later task.

use lattice_grammar::args::Args;
use lattice_grammar::command::LatencyClass;
use lattice_grammar::effect::{EchoLevel, Effect};
use lattice_grammar::error::{CommandError, GrammarResult};
use lattice_grammar::registry::{CommandRegistry, ExCommandSpec, SurfaceForm};

use crate::handle::AiClientHandle;
use crate::providers::ProviderConfig;

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

/// Take the rest of the line verbatim (trimmed) as a single string arg.
fn parse_rest_as_text(rest: &str, _bang: bool) -> GrammarResult<Args> {
    Ok(Args::String(rest.trim().to_string()))
}

/// Register `:opencode` / `:ai-prompt` / `:ai-stop` against `registry`,
/// wiring each to `handle`. Called once from editor boot.
pub fn register_ai_ex_commands(registry: &mut CommandRegistry, handle: AiClientHandle) {
    let start = handle.clone();
    registry.register_ex_command(
        "opencode",
        "Launch the opencode agent over ACP and open a session wired to this \
         editor.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(move |_ctx| {
                start.start(ProviderConfig::opencode());
                Ok(Effect::Echo {
                    level: EchoLevel::Info,
                    text: "opencode: starting agent".to_string(),
                })
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );

    let prompt = handle.clone();
    registry.register_ex_command(
        "ai-prompt",
        "Send the rest of the line to the running AI agent as a prompt.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_rest_as_text),
            apply: Box::new(move |ctx| match &ctx.args {
                Args::String(t) if !t.is_empty() => {
                    prompt.prompt(t.clone());
                    Ok(Effect::Echo {
                        level: EchoLevel::Info,
                        text: "ai-prompt: sent".to_string(),
                    })
                }
                _ => Ok(Effect::Echo {
                    level: EchoLevel::Error,
                    text: "ai-prompt: empty prompt".to_string(),
                }),
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );

    let stop = handle;
    registry.register_ex_command(
        "ai-stop",
        "Stop the running AI agent and close its session.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(move |_ctx| {
                stop.stop();
                Ok(Effect::Echo {
                    level: EchoLevel::Info,
                    text: "ai: stopping agent".to_string(),
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
    use crate::handle::{AiCmd, AiState};
    use arc_swap::ArcSwap;
    use lattice_grammar::registry::ExCommandContext;
    use lattice_grammar::{CancellationToken, Count, Register};
    use std::sync::Arc;

    fn test_handle() -> (AiClientHandle, tokio::sync::mpsc::UnboundedReceiver<AiCmd>) {
        let (cmd_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (
            AiClientHandle {
                cmd_tx,
                state: Arc::new(ArcSwap::from_pointee(AiState::default())),
            },
            rx,
        )
    }

    fn ctx_with(args: Args) -> ExCommandContext {
        ExCommandContext {
            bang: false,
            args,
            range: None,
            register: Register::default(),
            count: Count::default(),
            cancel: CancellationToken::new(),
        }
    }

    #[test]
    fn opencode_registers_and_starts() {
        let (handle, mut rx) = test_handle();
        let mut registry = CommandRegistry::new();
        register_ai_ex_commands(&mut registry, handle);

        let id = registry
            .id_by_name("opencode")
            .expect("`:opencode` is registered");
        let spec = registry.ex_command_spec(id).expect("spec present");
        let effect = (spec.apply)(&ctx_with(Args::None)).expect("apply ok");

        match effect {
            Effect::Echo { level, .. } => assert_eq!(level, EchoLevel::Info),
            other => panic!("expected an Echo, got {other:?}"),
        }
        match rx.try_recv().expect("AiCmd sent") {
            AiCmd::Start(_) => {}
            _ => panic!("expected AiCmd::Start"),
        }
    }

    #[test]
    fn ai_prompt_sends_text() {
        let (handle, mut rx) = test_handle();
        let mut registry = CommandRegistry::new();
        register_ai_ex_commands(&mut registry, handle);

        let id = registry
            .id_by_name("ai-prompt")
            .expect("`:ai-prompt` is registered");
        let spec = registry.ex_command_spec(id).expect("spec present");
        let effect = (spec.apply)(&ctx_with(Args::String("hi".to_string()))).expect("apply ok");

        match effect {
            Effect::Echo { level, .. } => assert_eq!(level, EchoLevel::Info),
            other => panic!("expected an Echo, got {other:?}"),
        }
        match rx.try_recv().expect("AiCmd sent") {
            AiCmd::Prompt(t) => assert_eq!(t, "hi"),
            _ => panic!("expected AiCmd::Prompt"),
        }
    }

    #[test]
    fn ai_prompt_rejects_empty() {
        let (handle, _rx) = test_handle();
        let mut registry = CommandRegistry::new();
        register_ai_ex_commands(&mut registry, handle);

        let id = registry
            .id_by_name("ai-prompt")
            .expect("`:ai-prompt` is registered");
        let spec = registry.ex_command_spec(id).expect("spec present");
        let effect = (spec.apply)(&ctx_with(Args::String(String::new()))).expect("apply ok");

        match effect {
            Effect::Echo { level, .. } => assert_eq!(level, EchoLevel::Error),
            other => panic!("expected an Echo, got {other:?}"),
        }
    }

    #[test]
    fn ai_stop_stops() {
        let (handle, mut rx) = test_handle();
        let mut registry = CommandRegistry::new();
        register_ai_ex_commands(&mut registry, handle);

        let id = registry
            .id_by_name("ai-stop")
            .expect("`:ai-stop` is registered");
        let spec = registry.ex_command_spec(id).expect("spec present");
        let effect = (spec.apply)(&ctx_with(Args::None)).expect("apply ok");

        match effect {
            Effect::Echo { level, .. } => assert_eq!(level, EchoLevel::Info),
            other => panic!("expected an Echo, got {other:?}"),
        }
        match rx.try_recv().expect("AiCmd sent") {
            AiCmd::Stop => {}
            _ => panic!("expected AiCmd::Stop"),
        }
    }
}
