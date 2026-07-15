//! PL8.C.2 — the `:plugin-load` / `:plugin-unload` / `:plugin-reload` ex-command
//! surface, **owned by the loader** (option A, confirmed with Dhruva).
//!
//! The loader self-registers these into the runtime-mutable `CommandRegistry` at
//! [`install`](crate::install) time; each `apply` closure captures the
//! [`PluginLoader`] handle and does the work in the loader crate — **zero host
//! code** (no host `Effect` variant, no `Editor::` method, no `expand_alias`
//! entry: plain command names resolve directly via `id_by_name`, exactly like
//! plugin-contributed ex-commands). The mode-ownership acid test holds
//! maximally.
//!
//! Sync vs async: `unload` is synchronous (teardown + `JoinHandle::abort` don't
//! await), so its `apply` does the work and echoes the result immediately.
//! `load` / `reload` are async (compile / instantiate / spawn), so their `apply`
//! kicks the work onto the loader's runtime and echoes "loading…"; completion /
//! failure surfaces via `tracing::info!` / `warn!` (→ `*messages*`), the
//! one-shot user-actionable event class.

use std::sync::Arc;

use lattice_grammar::{
    ArgDefault, ArgKind, ArgSpec, Args, CommandRegistry, EchoLevel, Effect, ExCommandContext,
    ExCommandSpec, GrammarResult, LatencyClass, SurfaceForm,
};

use crate::PluginLoader;

/// Register all three commands into `registry` (called under the loader's
/// load→clone→register→store RCU in [`PluginLoader::register_ex_commands`]).
pub(crate) fn register_all(registry: &mut CommandRegistry, loader: &Arc<PluginLoader>) {
    registry.register_ex_command(
        "plugin-load",
        "Load a plugin from a directory (`:plugin-load <path>`). The directory \
         must hold a `plugin.toml` manifest and exactly one `.wasm` component; \
         its declared seams are drained into the editor's native registries. \
         Loads asynchronously — completion is reported in `*messages*`.",
        load_spec(Arc::clone(loader)),
    );
    registry.register_ex_command(
        "plugin-unload",
        "Unload a loaded plugin (`:plugin-unload <id|name>`), reversing every \
         registry contribution it made (grammar / picker / modes / options / \
         event subscriptions) and stopping its actor tasks.",
        unload_spec(Arc::clone(loader)),
    );
    registry.register_ex_command(
        "plugin-reload",
        "Reload a loaded plugin (`:plugin-reload <id|name>`) — unload it, then \
         re-instantiate from its on-disk source with a fresh, untripped \
         quarantine. Reloads asynchronously — completion is reported in \
         `*messages*`.",
        reload_spec(Arc::clone(loader)),
    );
}

/// Parse the rest of the command line as a single trimmed string argument
/// (`<path>` for load, `<id|name>` for unload/reload). Empty → `Args::None` so
/// the `apply` can echo a usage hint.
fn parse_target(line: &str, _bang: bool) -> GrammarResult<Args> {
    let trimmed = line.trim();
    Ok(if trimmed.is_empty() {
        Args::None
    } else {
        Args::String(trimmed.to_string())
    })
}

/// The single string argument the user typed, if any.
fn arg_string(ctx: &ExCommandContext) -> Option<String> {
    match &ctx.args {
        Args::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn echo(level: EchoLevel, text: impl Into<String>) -> Effect {
    Effect::Echo {
        level,
        text: text.into(),
    }
}

/// One `ArgSpec` for the single positional string arg (drives the missing-arg
/// prompt + palette form). Completion is a follow-up (a `gen:plugins` generator
/// over the loaded set for unload/reload; a path completer for load).
fn string_arg(name: &'static str, doc: &'static str, prompt: &'static str) -> Vec<ArgSpec> {
    vec![ArgSpec {
        name,
        kind: ArgKind::String,
        doc,
        prompt,
        default: ArgDefault::None,
        completion: None,
    }]
}

fn load_spec(loader: Arc<PluginLoader>) -> ExCommandSpec {
    ExCommandSpec {
        // Reflex: the `apply` returns immediately (it spawns the async load); it
        // does no blocking work on the dispatch path.
        latency_class: LatencyClass::Reflex,
        accepts_bang: false,
        accepts_range: false,
        parse_args: Arc::new(parse_target),
        apply: Arc::new(move |ctx: &ExCommandContext| {
            let Some(path) = arg_string(ctx) else {
                return Ok(echo(EchoLevel::Warn, "usage: :plugin-load <path>"));
            };
            loader.spawn_load_path(std::path::PathBuf::from(&path));
            Ok(echo(EchoLevel::Info, format!("loading plugin from {path}…")))
        }),
        args_schema: string_arg(
            "path",
            "Directory holding the plugin's `plugin.toml` + `.wasm` component.",
            "path:",
        ),
        surface_form: SurfaceForm::Keyword,
    }
}

fn unload_spec(loader: Arc<PluginLoader>) -> ExCommandSpec {
    ExCommandSpec {
        latency_class: LatencyClass::Reflex,
        accepts_bang: false,
        accepts_range: false,
        parse_args: Arc::new(parse_target),
        apply: Arc::new(move |ctx: &ExCommandContext| {
            let Some(target) = arg_string(ctx) else {
                return Ok(echo(EchoLevel::Warn, "usage: :plugin-unload <id|name>"));
            };
            // Synchronous — do the work and report the outcome now.
            match loader.unload(&target) {
                Some(report) => Ok(echo(
                    EchoLevel::Info,
                    format!(
                        "unloaded `{target}` ({} contribution(s) reversed)",
                        report_total(&report)
                    ),
                )),
                None => Ok(echo(
                    EchoLevel::Warn,
                    format!("no loaded plugin `{target}`"),
                )),
            }
        }),
        args_schema: string_arg(
            "target",
            "Loaded plugin's manifest id or numeric plugin id.",
            "plugin:",
        ),
        surface_form: SurfaceForm::Keyword,
    }
}

fn reload_spec(loader: Arc<PluginLoader>) -> ExCommandSpec {
    ExCommandSpec {
        latency_class: LatencyClass::Reflex,
        accepts_bang: false,
        accepts_range: false,
        parse_args: Arc::new(parse_target),
        apply: Arc::new(move |ctx: &ExCommandContext| {
            let Some(target) = arg_string(ctx) else {
                return Ok(echo(EchoLevel::Warn, "usage: :plugin-reload <id|name>"));
            };
            loader.spawn_reload(target.clone());
            Ok(echo(EchoLevel::Info, format!("reloading `{target}`…")))
        }),
        args_schema: string_arg(
            "target",
            "Loaded plugin's manifest id or numeric plugin id.",
            "plugin:",
        ),
        surface_form: SurfaceForm::Keyword,
    }
}

/// Total contributions an unload reversed, across every surface — the number in
/// the echo line.
fn report_total(report: &lattice_plugin_host::TeardownReport) -> usize {
    report.commands
        + report.pickers
        + report.modes
        + report.config_options
        + report.events_defined
        + report.subscriptions
}
