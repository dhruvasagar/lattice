//! The transport-neutral `:ai-log` ex-command.
//!
//! `:ai-log` is always registered ([`register_ai_log_command`], no `#[cfg]`):
//! the log substrate lives in the `lattice-agent` port (AG‑3), so the command
//! is meaningful for whichever transport(s) are compiled in. It captures no
//! handle: opening the per-process `*ai:<provider>:<index>*` buffer needs host
//! buffer-open machinery, so it emits `Effect::OpenAiLog` and the host resolves
//! it (0 known sessions echo a hint, 1 opens directly, >1 raises a picker) --
//! exactly how `:lsp-server-log` is wired.
//!
//! The ACP lifecycle commands (`:opencode` / `:ai-prompt` / `:ai-stop`) live in
//! [`crate::acp::commands`], gated with the rest of the ACP transport.

use lattice_grammar::args::Args;
use lattice_grammar::command::LatencyClass;
use lattice_grammar::effect::Effect;
use lattice_grammar::registry::{CommandRegistry, ExCommandSpec, SurfaceForm};
use std::sync::Arc;

/// Register the transport-neutral `:ai-log [provider]` command. Captures NO
/// handle: the host reads the `AiLogger` service to enumerate sessions and
/// open the `*ai:<provider>:<index>*` buffer. The crate owns the BINDING + the
/// emission here (mode-ownership); the host owns only the generic buffer open.
/// Mirrors `:lsp-server-log`. Always available (both transports log through the
/// shared port substrate).
pub fn register_ai_log_command(registry: &mut CommandRegistry) {
    registry.register_ex_command(
        "ai-log",
        "Open the AI agent log buffer (picker when several sessions exist).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(lattice_agent::parse_rest_as_text),
            apply: Arc::new(move |ctx| {
                let session = match &ctx.args {
                    Args::String(t) if !t.is_empty() => Some(t.clone()),
                    _ => None,
                };
                Ok(Effect::OpenAiLog { session })
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
    use lattice_grammar::registry::ExCommandContext;
    use lattice_grammar::{CancellationToken, Count, Register};

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

    // `:ai-log` is transport-neutral -- registered without a handle and tested
    // in every feature combination (acp, mcp, both, neither).
    #[test]
    fn ai_log_no_arg_opens_without_prefilter() {
        let mut registry = CommandRegistry::new();
        register_ai_log_command(&mut registry);

        let id = registry
            .id_by_name("ai-log")
            .expect("`:ai-log` is registered");
        let spec = registry.ex_command_spec(id).expect("spec present");
        let effect = (spec.apply)(&ctx_with(Args::None)).expect("apply ok");

        match effect {
            Effect::OpenAiLog { session } => assert_eq!(session, None),
            other => panic!("expected OpenAiLog, got {other:?}"),
        }
    }

    #[test]
    fn ai_log_with_arg_carries_provider_prefilter() {
        let mut registry = CommandRegistry::new();
        register_ai_log_command(&mut registry);

        let id = registry
            .id_by_name("ai-log")
            .expect("`:ai-log` is registered");
        let spec = registry.ex_command_spec(id).expect("spec present");
        let effect =
            (spec.apply)(&ctx_with(Args::String("opencode".to_string()))).expect("apply ok");

        match effect {
            Effect::OpenAiLog { session } => assert_eq!(session.as_deref(), Some("opencode")),
            other => panic!("expected OpenAiLog, got {other:?}"),
        }
    }
}
