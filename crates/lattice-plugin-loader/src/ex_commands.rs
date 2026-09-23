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

use crate::{BulkOp, PluginLoader};

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
    registry.register_ex_command(
        "plugin-update",
        "Update a plugin (`:plugin-update <id|name>`) — bring its source up to \
         date with upstream, rebuild it, and reload. An unpinned git source \
         moves to the tracked head; a `Local` one is always current, so this is \
         a rebuild; a prebuilt one is re-downloaded. A plugin pinned to a \
         revision declines and says so — the pin is the answer already. Updates \
         asynchronously — completion is reported in `*messages*`.",
        update_spec(Arc::clone(loader)),
    );
    registry.register_ex_command(
        "plugin-rebuild-all",
        "Rebuild every loaded plugin from the source it already has \
         (`:plugin-rebuild-all`), then reload each. Plugins with no buildable \
         source (bundled, prebuilt) are skipped, not failed. Runs one at a \
         time — `cargo` already uses the whole machine — and one plugin's \
         failure never stops the rest. Reported in `*messages*`.",
        bulk_spec(Arc::clone(loader), BulkOp::Rebuild),
    );
    registry.register_ex_command(
        "plugin-reload-all",
        "Reload every loaded plugin (`:plugin-reload-all`) from the artifact \
         already on disk — no build, no network. Use after editing something \
         every plugin reads. One plugin's failure never stops the rest; \
         reported in `*messages*`.",
        bulk_spec(Arc::clone(loader), BulkOp::Reload),
    );
    registry.register_ex_command(
        "plugin-update-all",
        "Update every loaded plugin (`:plugin-update-all`) — bring each source \
         up to date, rebuild, reload. Pinned plugins are skipped and say so, \
         since a pin is the answer already. Spelled out rather than left as a \
         bare `:plugin-update`, which means one named plugin: a command that \
         rebuilds your whole editor should not be reachable by forgetting an \
         argument. Reported in `*messages*`.",
        bulk_spec(Arc::clone(loader), BulkOp::Update),
    );
    registry.register_ex_command(
        "plugin-clean",
        "List staged plugin directories that nothing loads any more \
         (`:plugin-clean`); `:plugin-clean!` removes them. A plugin that \
         FAILED to load is never listed — it is still one you asked for — and \
         neither is a directory without a `.source` marker, because provenance \
         is what makes the removal recoverable.",
        clean_spec(Arc::clone(loader)),
    );
    registry.register_ex_command(
        "reload-config",
        "Reload the user's `init.rs` configuration (`:reload-config`) — unload the \
         `init` plugin and re-instantiate it from `<config>/lattice/init/` with a \
         fresh, untripped quarantine, so edited keymaps / commands / options take \
         effect without restarting. Reloads asynchronously (reported in \
         `*messages*`). A no-op if no `init` config is loaded.",
        reload_config_spec(Arc::clone(loader)),
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
/// prompt, the palette form, and `<Tab>` in the `:` line).
///
/// `completion` names a generator registered by the host: `gen:plugins` over
/// the loaded set for unload / reload / update, and `gen:files` for load, whose
/// argument is a directory rather than a plugin. This was a deferred comment
/// here until the loaded-plugin registry existed; it does now, and
/// `ex_string_args_have_completion.rs` is what keeps the next one from being
/// deferred silently.
fn string_arg(
    name: &'static str,
    doc: &'static str,
    prompt: &'static str,
    completion: &'static str,
) -> Vec<ArgSpec> {
    vec![ArgSpec {
        name: name.into(),
        kind: ArgKind::String,
        doc: doc.into(),
        prompt: prompt.into(),
        default: ArgDefault::None,
        completion: Some(completion.into()),
        picker: None,
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
            // `~` expands, as it does for a `PluginSource::Local` in `init.rs`
            // and for every other path a user types at the `:` line. Without
            // it `:plugin-load ~/.config/lattice/plugins/org` looks up a
            // directory literally named `~`, and the error names the manifest
            // rather than the expansion — which reads like the plugin is
            // broken.
            let path = lattice_core::home::expand_tilde(&path);
            loader.spawn_load_path(std::path::PathBuf::from(&path));
            Ok(echo(
                EchoLevel::Info,
                format!("loading plugin from {path}…"),
            ))
        }),
        args_schema: string_arg(
            "path",
            "Directory holding the plugin's `plugin.toml` + `.wasm` component.",
            "path:",
            "gen:files",
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
            "gen:plugins",
        ),
        surface_form: SurfaceForm::Keyword,
    }
}

fn update_spec(loader: Arc<PluginLoader>) -> ExCommandSpec {
    ExCommandSpec {
        latency_class: LatencyClass::Reflex,
        accepts_bang: false,
        accepts_range: false,
        parse_args: Arc::new(parse_target),
        apply: Arc::new(move |ctx: &ExCommandContext| {
            let Some(target) = arg_string(ctx) else {
                return Ok(echo(EchoLevel::Warn, "usage: :plugin-update <id|name>"));
            };
            loader.spawn_update(target.clone());
            Ok(echo(EchoLevel::Info, format!("updating `{target}`…")))
        }),
        args_schema: string_arg(
            "target",
            "Loaded plugin's manifest id or numeric plugin id.",
            "plugin:",
            "gen:plugins",
        ),
        surface_form: SurfaceForm::Keyword,
    }
}

/// The word the acknowledgement uses while a bulk run is in flight
/// (`updating all plugins…`).
///
/// The past-tense peer lives on [`BulkOp`] itself, because the loader needs it
/// for the summary it logs when the run finishes; this one is only ever said
/// here, on the dispatch path, so it stays here.
fn progressive(op: BulkOp) -> &'static str {
    match op {
        BulkOp::Rebuild => "rebuilding",
        BulkOp::Reload => "reloading",
        BulkOp::Update => "updating",
    }
}

/// One spec builder over [`BulkOp`] rather than three near-identical
/// builders: the three differ only in the loader method awaited and the word
/// the echo uses, and three copies of the spawn-and-echo scaffolding is three
/// places for them to drift apart.
fn bulk_spec(loader: Arc<PluginLoader>, op: BulkOp) -> ExCommandSpec {
    ExCommandSpec {
        latency_class: LatencyClass::Reflex,
        accepts_bang: false,
        accepts_range: false,
        parse_args: Arc::new(|_line: &str, _bang: bool| Ok(Args::None)),
        apply: Arc::new(move |_ctx: &ExCommandContext| {
            loader.spawn_bulk(op);
            Ok(echo(
                EchoLevel::Info,
                format!("{} all plugins…", progressive(op)),
            ))
        }),
        args_schema: Vec::new(),
        surface_form: SurfaceForm::Keyword,
    }
}

fn clean_spec(loader: Arc<PluginLoader>) -> ExCommandSpec {
    ExCommandSpec {
        latency_class: LatencyClass::Reflex,
        // The bang is the confirmation. `:plugin-clean` shows you what would
        // go; `:plugin-clean!` removes it. Vim's own convention for "yes, I
        // mean it", and it costs no new mechanism.
        accepts_bang: true,
        accepts_range: false,
        parse_args: Arc::new(|_line: &str, _bang: bool| Ok(Args::None)),
        apply: Arc::new(move |ctx: &ExCommandContext| {
            let removable = loader.removable_plugin_dirs();
            if removable.is_empty() {
                return Ok(echo(EchoLevel::Info, "nothing to clean"));
            }
            let names: Vec<String> = removable.into_iter().map(|(n, _)| n).collect();
            // `ctx.bang`, not a parsed arg: the dispatcher already carries
            // the bang, and re-encoding it into `Args` would be a second
            // answer to a question the context has answered.
            if !ctx.bang {
                return Ok(echo(
                    EchoLevel::Info,
                    format!(
                        "{} removable: {} — `:plugin-clean!` to remove",
                        names.len(),
                        names.join(", ")
                    ),
                ));
            }
            let report = loader.clean(&names);
            for (name, why) in report.failures() {
                tracing::warn!(plugin = %name, error = %why, "plugin clean failed");
            }
            Ok(echo(EchoLevel::Info, report.summary("removed")))
        }),
        args_schema: Vec::new(),
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
            "gen:plugins",
        ),
        surface_form: SurfaceForm::Keyword,
    }
}

use crate::INIT_PLUGIN_ID;

fn reload_config_spec(loader: Arc<PluginLoader>) -> ExCommandSpec {
    ExCommandSpec {
        latency_class: LatencyClass::Reflex,
        accepts_bang: false,
        accepts_range: false,
        // `:reload-config` takes no argument — ignore any trailing text.
        parse_args: Arc::new(|_line: &str, _bang: bool| Ok(Args::None)),
        apply: Arc::new(move |_ctx: &ExCommandContext| {
            loader.spawn_reload(INIT_PLUGIN_ID.to_string());
            Ok(echo(EchoLevel::Info, "reloading user config (init.rs)…"))
        }),
        args_schema: Vec::new(),
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
        + report.keymap_bindings
}
