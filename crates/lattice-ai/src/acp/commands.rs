//! ACP lifecycle ex-commands: `:opencode` / `:ai-prompt` / `:ai-stop`.
//!
//! Per `feedback_mode_owns_its_surface`, BOTH the binding (the command name)
//! AND the handler body live here: each `apply` closure captures the
//! [`AiClientHandle`](crate::acp::handle::AiClientHandle) and drives it directly
//! (a non-blocking `cmd_tx` send), returning an `Effect::Echo` for user
//! feedback. The host's only role is calling [`register_ai_ex_commands`] once at
//! boot (from `crate::acp::install`).
//!
//! The transport-neutral `:ai-log` command is NOT here -- it lives in the
//! crate-root `commands` module (`register_ai_log_command`), because the log
//! substrate is shared by every transport (AG‑3).

use std::sync::Arc;

use lattice_agent::{parse_no_args, parse_rest_as_text};
use lattice_grammar::args::Args;
use lattice_grammar::command::LatencyClass;
use lattice_grammar::effect::{EchoLevel, Effect};
use lattice_grammar::registry::{CommandRegistry, ExCommandSpec, SurfaceForm};

use crate::acp::handle::AiClientHandle;
use crate::acp::providers::ProviderConfig;

/// Register `:opencode` / `:ai-prompt` / `:ai-stop` against `registry`,
/// wiring each to `handle`. Called once from ACP boot (`crate::acp::install`).
pub fn register_ai_ex_commands(registry: &mut CommandRegistry, handle: AiClientHandle) {
    let start = handle.clone();
    registry.register_ex_command(
        // The primary opencode integration: lattice drives `opencode acp`
        // headlessly and renders the conversation itself, because opencode's
        // native TUI (opentui) needs modern-terminal image/capability features
        // lattice's emulator doesn't provide, so the terminal path can't render
        // it. That terminal spawn is kept as `:opencode-term` (see
        // `crate::opencode`) for agents whose TUIs degrade gracefully.
        "opencode",
        "Launch the opencode agent over ACP and open the *ai:opencode* \
         conversation buffer wired to this editor.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(move |_ctx| {
                start.start(ProviderConfig::opencode());
                // Start AND open the conversation buffer (the user's decision):
                // a generic open of the `*ai:opencode*` synthetic buffer under
                // the `ai-conversation` mode. The mode's `on_activate` seeds +
                // live-tails from the ConversationStore.
                Ok(Effect::OpenSyntheticBuffer {
                    name: crate::acp::conversation_mode::conversation_buffer_name(),
                    mode_id: crate::acp::conversation_mode::AiConversationMode::mode_id()
                        .as_str()
                        .to_string(),
                    content: None,
                    cursor: None,
                    activate_minor: None,
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
            parse_args: Arc::new(parse_rest_as_text),
            apply: Arc::new(move |ctx| match &ctx.args {
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(move |_ctx| {
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

    // PU-B: open the permission menu for the oldest pending request. Data-only
    // — the host ensures the `*ai-permission*` popup buffer under
    // `ai-permission-mode` and its `on_activate` projects the request. Steal
    // focus (the agent is blocked awaiting the decision); `Esc` defers.
    registry.register_ex_command(
        "ai-permission",
        "Open the permission menu for the oldest pending agent request.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_ctx| {
                Ok(Effect::OpenPopup {
                    name: crate::acp::permission_mode::PERMISSION_BUFFER_NAME.to_string(),
                    mode_id: crate::acp::permission_mode::AiPermissionMode::mode_id()
                        .as_str()
                        .to_string(),
                    placement: lattice_core::ui::popup::PopupPlacement::Centered,
                    focus: lattice_core::ui::popup::PopupFocus::Steal,
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
    use crate::acp::handle::{AiCmd, AiState};
    use arc_swap::ArcSwap;
    use lattice_grammar::registry::ExCommandContext;
    use lattice_grammar::{CancellationToken, Count, Register};
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    fn test_handle() -> (AiClientHandle, tokio::sync::mpsc::UnboundedReceiver<AiCmd>) {
        let (cmd_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (
            AiClientHandle {
                cmd_tx,
                state: Arc::new(ArcSwap::from_pointee(AiState::default())),
                queue_len: Arc::new(AtomicUsize::new(0)),
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
            buffer_id: lattice_core::BufferId::default(),
            // OC.10 added these four so a PLUGIN ex-command could name the
            // buffer its `Effect::ApplyEdit` targets. Every command in this
            // module is transport-level — it starts an agent, opens a log,
            // toggles a server — and reads none of them, so they carry their
            // empty forms rather than a fabricated cursor into a buffer this
            // test never builds.
            cursor: Default::default(),
            buffer: Default::default(),
            path: None,
            syntax: None,
            cancel: CancellationToken::new(),
        }
    }

    #[test]
    fn opencode_starts_and_opens_the_conversation_buffer() {
        let (handle, mut rx) = test_handle();
        let mut registry = CommandRegistry::new();
        register_ai_ex_commands(&mut registry, handle);

        let id = registry
            .id_by_name("opencode")
            .expect("`:opencode` is registered");
        let spec = registry.ex_command_spec(id).expect("spec present");
        let effect = (spec.apply)(&ctx_with(Args::None)).expect("apply ok");

        // `:opencode` both starts the agent AND opens the `*ai:opencode*`
        // conversation buffer under the `ai-conversation` mode.
        match effect {
            Effect::OpenSyntheticBuffer { name, mode_id, .. } => {
                assert_eq!(name, "*ai:opencode*");
                assert_eq!(mode_id, "ai-conversation-mode");
            }
            other => panic!("expected OpenSyntheticBuffer, got {other:?}"),
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
