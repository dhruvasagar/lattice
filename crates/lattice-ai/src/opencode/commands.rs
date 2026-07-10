//! The `:opencode` ex-command — launch the opencode agent's native TUI in a
//! terminal buffer with `opencode-mode` activated on it.
//!
//! Per `feedback_mode_owns_its_surface`, both the binding (the command name)
//! and the body live here; the host action (spawning the terminal + activating
//! the minor) is requested through the [`Effect`] vocabulary — the host
//! boundary — not a bespoke channel, exactly like `:claude`.

use lattice_agent::parse_no_args;
use lattice_grammar::command::LatencyClass;
use lattice_grammar::effect::Effect;
use lattice_grammar::registry::{CommandRegistry, ExCommandSpec, SurfaceForm};

use crate::opencode::modes::OpencodeMode;

/// Register `:opencode` against `registry`. Called from [`super::install`].
pub fn register_opencode_ex_commands(registry: &mut CommandRegistry) {
    registry.register_ex_command(
        "opencode-term",
        "Launch the opencode agent's native TUI in a terminal buffer, with \
         opencode-mode activated. NOTE: opencode's TUI (opentui) needs modern- \
         terminal image/capability features lattice's emulator doesn't yet \
         provide, so it may not render here -- use `:opencode` (the ACP \
         conversation) instead. Kept for agents whose TUIs degrade gracefully.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_ctx| {
                Ok(Effect::SpawnTerminal {
                    cmd_line: Some("opencode".to_string()),
                    env: vec![],
                    activate_minor: Some(OpencodeMode::mode_id().as_str().to_string()),
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
    use lattice_grammar::args::Args;
    use lattice_grammar::registry::ExCommandContext;
    use lattice_grammar::{CancellationToken, Count, Register};

    fn ctx() -> ExCommandContext {
        ExCommandContext {
            bang: false,
            args: Args::None,
            range: None,
            register: Register::default(),
            count: Count::default(),
            cancel: CancellationToken::new(),
        }
    }

    /// `:opencode-term` spawns the `opencode` TUI in a terminal and activates
    /// `opencode-mode` on it (the terminal topology, like `:claude`).
    #[test]
    fn opencode_term_spawns_terminal_and_activates_mode() {
        let mut registry = CommandRegistry::new();
        register_opencode_ex_commands(&mut registry);
        let id = registry.id_by_name("opencode-term").expect("`:opencode-term` registered");
        let spec = registry.ex_command_spec(id).expect("spec present");
        match (spec.apply)(&ctx()).expect("apply ok") {
            Effect::SpawnTerminal { cmd_line, activate_minor, .. } => {
                assert_eq!(cmd_line.as_deref(), Some("opencode"));
                assert_eq!(activate_minor.as_deref(), Some("opencode-mode"));
            }
            other => panic!("expected SpawnTerminal, got {other:?}"),
        }
    }
}
