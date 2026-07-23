//! CM.1: the `:compile` / `:recompile` / `:make` ex-commands.
//!
//! Each parses its (optional) argument into an
//! `Option<String>` cmdline and returns
//! `Effect::AppAction(AppEffect::CompileRun { cmdline })`; the
//! host's apply-effect arm creates the `*compilation*` buffer
//! (`Editor::ensure_named_synthetic_document`, host-side) and runs the
//! registered [`crate::CompilationServiceHandle`]. `:compile` and
//! `:recompile` are
//! the emacs-canonical names for this feature (not an LSP-coupled
//! subsystem, so the dashed-namespaced rule does not apply);
//! `:make` is the vim-canonical alias.

use std::sync::Arc;

use lattice_grammar::app_effect::AppEffect;
use lattice_grammar::args::{ArgKind, ArgSpec, Args};
use lattice_grammar::command::LatencyClass;
use lattice_grammar::effect::Effect;
use lattice_grammar::error::CommandError;
use lattice_grammar::registry::{ExCommandSpec, SurfaceForm};
use lattice_grammar::CommandRegistry;

/// Parse an already-trimmed argument string into an optional
/// cmdline (empty ⇒ `None`).
fn arg_to_cmdline(args: &Args) -> Option<String> {
    match args {
        Args::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Register `:compile` / `:recompile` / `:make`.
pub fn register_compilation_ex_commands(registry: &mut CommandRegistry) {
    // `:compile <cmd>` — run a shell command, streaming into
    // `*compilation*`. Requires a non-empty command.
    registry.register_ex_command(
        "compile",
        "Run a shell command and stream its output into the *compilation* buffer.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(|s: &str, _bang: bool| {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    return Err(CommandError::BadArgs(":compile requires a command".into()));
                }
                Ok(Args::String(trimmed.to_string()))
            }),
            apply: Arc::new(|ctx| {
                Ok(Effect::AppAction(AppEffect::CompileRun {
                    cmdline: arg_to_cmdline(&ctx.args),
                }))
            }),
            args_schema: vec![ArgSpec::required(
                "command",
                ArgKind::String,
                "shell command to run",
            )],
            surface_form: SurfaceForm::Keyword,
        },
    );

    // `:recompile` — re-run the last compilation command. No args.
    registry.register_ex_command(
        "recompile",
        "Re-run the last compilation command, clearing and re-streaming *compilation*.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(|s: &str, _bang: bool| {
                if s.trim().is_empty() {
                    Ok(Args::None)
                } else {
                    Err(CommandError::BadArgs(
                        ":recompile takes no arguments".into(),
                    ))
                }
            }),
            apply: Arc::new(|_ctx| Ok(Effect::AppAction(AppEffect::CompileRun { cmdline: None }))),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );

    // `:make [cmd]` — vim-canonical alias. Optional override runs a
    // one-off command; bare `:make` reuses the last command.
    registry.register_ex_command(
        "make",
        "Run the build command (optionally overridden) into the *compilation* buffer.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(|s: &str, _bang: bool| {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    Ok(Args::None)
                } else {
                    Ok(Args::String(trimmed.to_string()))
                }
            }),
            apply: Arc::new(|ctx| {
                Ok(Effect::AppAction(AppEffect::CompileRun {
                    cmdline: arg_to_cmdline(&ctx.args),
                }))
            }),
            args_schema: vec![ArgSpec::optional(
                "command",
                ArgKind::String,
                "build command override",
            )],
            surface_form: SurfaceForm::Keyword,
        },
    );

    // CM.3d (2026-07-22): kill the running compilation via `<C-c>`.
    // No args; the host arm calls CompilationService::kill().
    registry.register_ex_command(
        "compilation-kill",
        "Kill the running compilation child process.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(|_s: &str, _bang: bool| Ok(Args::None)),
            apply: Arc::new(|_| Ok(Effect::AppAction(AppEffect::CompilationKill))),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    fn ex_spec<'a>(registry: &'a CommandRegistry, name: &str) -> Option<&'a ExCommandSpec> {
        registry
            .id_by_name(name)
            .and_then(|id| registry.ex_command_spec(id))
    }

    #[test]
    fn registers_all_four_commands() {
        let mut registry = CommandRegistry::new();
        register_compilation_ex_commands(&mut registry);
        assert!(ex_spec(&registry, "compile").is_some());
        assert!(ex_spec(&registry, "recompile").is_some());
        assert!(ex_spec(&registry, "make").is_some());
        assert!(ex_spec(&registry, "compilation-kill").is_some());
    }

    #[test]
    fn compile_rejects_empty_and_accepts_command() {
        let mut registry = CommandRegistry::new();
        register_compilation_ex_commands(&mut registry);
        let spec = ex_spec(&registry, "compile").unwrap();
        assert!((spec.parse_args)("", false).is_err());
        let parsed = (spec.parse_args)("cargo build", false).unwrap();
        match parsed {
            Args::String(s) => assert_eq!(s, "cargo build"),
            other => panic!("expected String args, got {other:?}"),
        }
    }

    #[test]
    fn make_arg_is_optional() {
        let mut registry = CommandRegistry::new();
        register_compilation_ex_commands(&mut registry);
        let spec = ex_spec(&registry, "make").unwrap();
        assert!(matches!((spec.parse_args)("", false).unwrap(), Args::None));
        assert!(matches!(
            (spec.parse_args)("cargo test", false).unwrap(),
            Args::String(_)
        ));
    }
}
