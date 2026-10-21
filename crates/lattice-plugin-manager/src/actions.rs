//! PL8.H.3 — the `plugins-mode` action handlers: reload / unload / describe /
//! refresh the plugin under the cursor.
//!
//! Each is a mode-owned [`ActionHandler`] closure (the `repl-mode` precedent),
//! bound to its `action:plugins-*` command by the host's
//! `register_mode_action_handlers` walk and gated to `plugins-mode`-active
//! buffers by the per-keystroke filter. The `action:*` commands are registered
//! (dead-body) at [`install`](crate::install) so their names resolve for the
//! keymap binding; the mode's handler closure intercepts before the grammar
//! Action gate, so those dead bodies never run.
//!
//! The cursor line maps to a plugin through the render
//! ([`RenderedStatus::target_at`](crate::render::RenderedStatus::target_at)):
//! a row of the table, or an entry in the "build failed" / "failed to load"
//! sections under it. Every handler reads the loader + buffer
//! store from the [`ActionContext`] service registry, so nothing is captured at
//! registration — a missing service degrades the action to a no-op, never a
//! panic. Reload runs off the actor thread (async unload + re-instantiate);
//! unload is a synchronous teardown; both re-render from the fresh status
//! afterward.

use std::sync::Arc;

use lattice_grammar::registry::ActionSpec;
use lattice_grammar::{CommandRegistry, EchoLevel, Effect};
use lattice_mode::{ActionContext, ActionHandler, BufferStoreHandle};
use lattice_plugin_host::{PluginTracerHandle, TrustTier};
use lattice_plugin_loader::{BulkOp, PluginLoaderHandle};

/// The `action:plugins-*` command names. Used for the handler bindings +
/// dead-body registration; the keymap `cmd:` literals in `mode.rs` must match
/// these (pinned by `keymap_cmds_have_registered_handlers`).
pub const RELOAD: &str = "action:plugins-reload";
pub const UNLOAD: &str = "action:plugins-unload";
pub const DESCRIBE: &str = "action:plugins-describe";
pub const REFRESH: &str = "action:plugins-refresh";
pub const TRACE: &str = "action:plugins-trace";
pub const TRACE_LEVEL: &str = "action:plugins-trace-level";
/// PM.8b: force a fresh build of the plugin under the cursor.
pub const REBUILD: &str = "action:plugins-rebuild";
/// The row verb that goes and gets something newer first.
pub const UPDATE: &str = "action:plugins-update";
/// The all-scope peers of `r` / `b` / `u`. Uppercase is the view's existing
/// idiom for "the other scope of this verb" (`t` / `T` already read that way).
pub const RELOAD_ALL: &str = "action:plugins-reload-all";
pub const REBUILD_ALL: &str = "action:plugins-rebuild-all";
pub const UPDATE_ALL: &str = "action:plugins-update-all";
/// `X` — remove staged directories nothing loads any more. Confirmed before it
/// runs, because it is the only verb here that deletes anything.
pub const CLEAN: &str = "action:plugins-clean";
/// The yes-half of [`CLEAN`]'s confirmation. Never bound to a chord: it is
/// reached only through `Effect::Confirm`, carrying the names the prompt named.
pub const CLEAN_CONFIRMED: &str = "action:plugins-clean-confirmed";

/// Register the four `action:plugins-*` commands (dead-body — the mode's handler
/// closures do the work) so the keymap's `cmd:` names resolve at boot. The
/// `register_repl_mode_actions` precedent.
pub fn register_actions(commands: &mut CommandRegistry) {
    for (name, doc) in [
        (
            RELOAD,
            "plugins: reload the plugin under the cursor (mode-owned).",
        ),
        (
            UNLOAD,
            "plugins: unload the plugin under the cursor (mode-owned).",
        ),
        (
            DESCRIBE,
            "plugins: describe the plugin under the cursor (mode-owned).",
        ),
        (REFRESH, "plugins: refresh the plugin list (mode-owned)."),
        (
            TRACE,
            "plugins: open the boundary trace for the plugin under the cursor (mode-owned).",
        ),
        (
            TRACE_LEVEL,
            "plugins: cycle the trace verbosity of the plugin under the cursor (mode-owned).",
        ),
        (
            REBUILD,
            "plugins: force a fresh build of the plugin under the cursor (mode-owned).",
        ),
        (
            UPDATE,
            "plugins: update the plugin under the cursor — fetch, rebuild, reload (mode-owned).",
        ),
        (
            RELOAD_ALL,
            "plugins: reload every loaded plugin (mode-owned).",
        ),
        (
            REBUILD_ALL,
            "plugins: rebuild every loaded plugin from source (mode-owned).",
        ),
        (
            UPDATE_ALL,
            "plugins: update every loaded plugin (mode-owned).",
        ),
        (
            CLEAN,
            "plugins: remove staged plugin directories nothing loads any more (mode-owned).",
        ),
        (
            CLEAN_CONFIRMED,
            "plugins: the confirmed half of clean — not bound to a chord (mode-owned).",
        ),
    ] {
        commands.register_action(
            name,
            doc,
            ActionSpec {
                apply: Arc::new(|_| Ok(Effect::None)),
                args_schema: vec![],
            },
        );
    }
}

/// The name of the plugin the cursor's line is about, or `None` on a line
/// that is about none (the title, a section heading, a hint) — the action
/// then no-ops.
///
/// Asked of the render, not computed from the cursor: the view is a table of
/// loaded plugins followed by the entries for the ones whose build or load
/// failed, and only the render knows which lines belong to which. A plugin
/// that failed to load has no row in the table, so indexing the loaded list —
/// what this did before — made every chord a no-op on exactly the entries a
/// user most wants to press `b` on.
fn plugin_name_at(ctx: &ActionContext<'_>) -> Option<String> {
    let loader = ctx.services.get::<PluginLoaderHandle>()?;
    crate::mode::render_now(&loader)
        .target_at(ctx.cursor.line as usize)
        .map(str::to_string)
}

/// The `(host-issued id, manifest name)` of the LOADED plugin under the
/// cursor. `None` for a plugin that failed to load as well as for no plugin:
/// the id keys the tracer, and a plugin that never loaded was never issued
/// one.
fn plugin_row_at(ctx: &ActionContext<'_>) -> Option<(u32, String)> {
    let name = plugin_name_at(ctx)?;
    let loader = ctx.services.get::<PluginLoaderHandle>()?;
    loader
        .plugin_status()
        .into_iter()
        .find(|s| s.name == name)
        .map(|s| (s.id, s.name))
}

/// Re-render the manager buffer from the current status (off the actor thread).
fn refresh(ctx: &ActionContext<'_>) {
    let (Some(loader), Some(store)) = (
        ctx.services.get::<PluginLoaderHandle>(),
        ctx.services.get::<BufferStoreHandle>(),
    ) else {
        return;
    };
    let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
    let highlights = ctx
        .services
        .get::<lattice_mode::PendingSyntheticHighlights>();
    crate::mode::spawn_rerender(&store, buffer_id, loader, highlights);
}

/// `r` — reload the plugin under the cursor (async: unload + re-instantiate from
/// disk with a fresh, untripped quarantine), then re-render.
pub fn reload_handler() -> ActionHandler {
    Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
        let name = plugin_name_at(ctx)?;
        let loader = ctx.services.get::<PluginLoaderHandle>()?;
        let store = ctx.services.get::<BufferStoreHandle>()?;
        let highlights = ctx
            .services
            .get::<lattice_mode::PendingSyntheticHighlights>();
        let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let name_c = name.clone();
            runtime.spawn(async move {
                let _ = loader.reload(&name_c, TrustTier::UserInstalled).await;
                if let Some(handle) = store.handle_for(buffer_id) {
                    crate::mode::rerender(&handle, &loader, highlights.as_deref(), buffer_id).await;
                }
            });
        }
        Some(Effect::Echo {
            level: EchoLevel::Info,
            text: format!("reloading `{name}`…"),
        })
    })
}

/// `b` — force a fresh build of the plugin under the cursor, then reload.
///
/// Distinct from `r` (reload), which re-instantiates whatever artifact is on
/// disk. `b` rebuilds that artifact from source first — the thing you want
/// after editing a local plugin, and the thing `r` cannot do.
///
/// A row whose source is not buildable (bundled, prebuilt, unknown) echoes why
/// rather than starting a build that cannot work.
pub fn rebuild_handler() -> ActionHandler {
    Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
        let name = plugin_name_at(ctx)?;
        let loader = ctx.services.get::<PluginLoaderHandle>()?;
        let store = ctx.services.get::<BufferStoreHandle>()?;
        let highlights = ctx
            .services
            .get::<lattice_mode::PendingSyntheticHighlights>();
        let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return Some(Effect::Echo {
                level: EchoLevel::Error,
                text: "no runtime available to build on".to_string(),
            });
        };
        let name_c = name.clone();
        runtime.spawn(async move {
            let result = loader.rebuild(&name_c).await;
            // Re-render either way: on success the row's BUILD flips back to
            // `cached`, on failure it reads `build-failed` — both are the
            // answer the user pressed `b` to get.
            if let Some(handle) = store.handle_for(buffer_id) {
                crate::mode::rerender(&handle, &loader, highlights.as_deref(), buffer_id).await;
            }
            match result {
                Ok(()) => tracing::info!(plugin = %name_c, "plugin rebuilt"),
                // CG.10: refused, not failed — a build of it is running.
                Err(error) if error.contains(lattice_plugin_loader::ALREADY_BUILDING) => {
                    tracing::info!(plugin = %name_c, "{error}")
                }
                Err(error) => tracing::warn!(plugin = %name_c, %error, "plugin rebuild failed"),
            }
        });
        // The row flips to `building…` on the next render; echo so the
        // keypress is acknowledged immediately even before that lands.
        Some(Effect::Echo {
            level: EchoLevel::Info,
            text: format!("rebuilding `{name}`…"),
        })
    })
}

/// `u` — update the plugin under the cursor: bring its source up to date,
/// rebuild, reload.
///
/// The difference from `b` (rebuild) is where the source comes from, not what
/// happens to it: rebuild compiles what is already on disk, update fetches
/// first. A pinned plugin declines and says so — the loader's arm table owns
/// that decision, not this handler.
pub fn update_handler() -> ActionHandler {
    Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
        let name = plugin_name_at(ctx)?;
        let loader = ctx.services.get::<PluginLoaderHandle>()?;
        let store = ctx.services.get::<BufferStoreHandle>()?;
        let highlights = ctx
            .services
            .get::<lattice_mode::PendingSyntheticHighlights>();
        let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return Some(Effect::Echo {
                level: EchoLevel::Error,
                text: "no runtime available to update on".to_string(),
            });
        };
        let name_c = name.clone();
        runtime.spawn(async move {
            let result = loader.update(&name_c).await;
            // Re-render either way: success flips BUILD back to `cached`,
            // failure reads `build-failed`, and a decline leaves the row as it
            // was — all three are the answer the user pressed `u` to get.
            if let Some(handle) = store.handle_for(buffer_id) {
                crate::mode::rerender(&handle, &loader, highlights.as_deref(), buffer_id).await;
            }
            match result {
                Ok(()) => tracing::info!(plugin = %name_c, "plugin updated"),
                Err(error) if error.contains(lattice_plugin_loader::ALREADY_BUILDING) => {
                    tracing::info!(plugin = %name_c, "{error}")
                }
                Err(error) => tracing::warn!(plugin = %name_c, %error, "plugin update failed"),
            }
        });
        Some(Effect::Echo {
            level: EchoLevel::Info,
            text: format!("updating `{name}`…"),
        })
    })
}

/// `R` / `B` / `U` — the all-scope peers, one handler body over [`BulkOp`].
///
/// Re-renders BETWEEN legs, not only at the end, and that is the feature: a
/// bulk rebuild is minutes of `cargo`, so a view that only updated when the
/// whole run finished would sit still for the entire time it mattered. Each
/// leg's start repaints the title with `updating 3/7 (org)…` and the row it is
/// working on flips to `building…` — the async-buffer rule's headerline
/// surface, in the buffer the user is already looking at.
fn bulk_handler(op: BulkOp, progressive: &'static str) -> ActionHandler {
    Arc::new(move |ctx: &ActionContext<'_>| -> Option<Effect> {
        let loader = ctx.services.get::<PluginLoaderHandle>()?;
        let store = ctx.services.get::<BufferStoreHandle>()?;
        let highlights = ctx
            .services
            .get::<lattice_mode::PendingSyntheticHighlights>();
        let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return Some(Effect::Echo {
                level: EchoLevel::Error,
                text: format!("no runtime available to {progressive} on"),
            });
        };
        runtime.spawn(async move {
            let repaint = |note: Option<String>| {
                // The note lives with the view, not in this closure: a leg's
                // own status events re-render too, and a render that did not
                // know about the run would wipe `updating 3/7 (org)…` off the
                // title the moment the leg started.
                crate::mode::set_bulk_progress(note);
                crate::mode::spawn_rerender(&store, buffer_id, loader.clone(), highlights.clone());
            };
            let report = loader
                .run_bulk(op, &|done, total, name| {
                    repaint(Some(format!(
                        "{progressive} {}/{total} ({name})…",
                        done + 1
                    )));
                })
                .await;
            // Clear the note on the way out: a title still claiming to be
            // updating after the run finished is the kind of stuck indicator
            // users stop trusting.
            repaint(None);
            for (name, why) in report.failures() {
                tracing::warn!(plugin = %name, error = %why, "bulk plugin operation failed");
            }
            tracing::info!(summary = %report.summary(past_tense(op)), "bulk plugin operation done");
        });
        Some(Effect::Echo {
            level: EchoLevel::Info,
            text: format!("{progressive} all plugins…"),
        })
    })
}

/// The word a finished bulk run counts with.
///
/// Duplicated from the loader's private peer rather than exported: it is one
/// word per variant, and widening the loader's public surface to share three
/// strings buys less than it costs.
fn past_tense(op: BulkOp) -> &'static str {
    match op {
        BulkOp::Rebuild => "rebuilt",
        BulkOp::Reload => "reloaded",
        BulkOp::Update => "updated",
    }
}

pub fn reload_all_handler() -> ActionHandler {
    bulk_handler(BulkOp::Reload, "reloading")
}

pub fn rebuild_all_handler() -> ActionHandler {
    bulk_handler(BulkOp::Rebuild, "rebuilding")
}

pub fn update_all_handler() -> ActionHandler {
    bulk_handler(BulkOp::Update, "updating")
}

/// `X` — ask before removing staged directories nothing loads any more.
///
/// The confirmation carries the NAMES, not a cursor position or a count, so
/// the thing the user agreed to is the thing that gets deleted (effect.rs,
/// IX.1). Between the prompt appearing and `y` being pressed a plugin can
/// finish loading; re-deriving the list afterwards would delete the one that
/// just arrived.
///
/// One `Args::String` rather than one argument per name: a confirm's payload
/// is truncated to the action's declared arity, so a variadic list has to
/// travel in a single slot.
pub fn clean_handler() -> ActionHandler {
    Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
        let loader = ctx.services.get::<PluginLoaderHandle>()?;
        let removable = loader.removable_plugin_dirs();
        if removable.is_empty() {
            return Some(Effect::Echo {
                level: EchoLevel::Info,
                text: "nothing to clean".to_string(),
            });
        }
        let names: Vec<String> = removable.into_iter().map(|(n, _)| n).collect();
        Some(Effect::Confirm {
            prompt: format!(
                "Remove {} staged plugin director{} ({})?",
                names.len(),
                if names.len() == 1 { "y" } else { "ies" },
                names.join(", ")
            ),
            yes_action: CLEAN_CONFIRMED.to_string(),
            args: lattice_grammar::Args::String(names.join(",")),
        })
    })
}

/// The yes-half of [`clean_handler`]. Reached only through the confirmation,
/// carrying the names it named.
pub fn clean_confirmed_handler() -> ActionHandler {
    Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
        let loader = ctx.services.get::<PluginLoaderHandle>()?;
        let lattice_grammar::Args::String(packed) = &ctx.args else {
            return Some(Effect::Echo {
                level: EchoLevel::Error,
                text: "clean: no plugins named in the confirmation".to_string(),
            });
        };
        let names: Vec<String> = packed
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let report = loader.clean(&names);
        for (name, why) in report.failures() {
            tracing::warn!(plugin = %name, error = %why, "plugin clean failed");
        }
        refresh(ctx);
        Some(Effect::Echo {
            level: EchoLevel::Info,
            text: report.summary("removed"),
        })
    })
}

/// `x` — unload the plugin under the cursor (synchronous teardown), then
/// re-render to drop its row.
pub fn unload_handler() -> ActionHandler {
    Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
        let name = plugin_name_at(ctx)?;
        let loader = ctx.services.get::<PluginLoaderHandle>()?;
        let report = loader.unload(&name);
        refresh(ctx);
        Some(Effect::Echo {
            level: EchoLevel::Info,
            text: match report {
                Some(_) => format!("unloaded `{name}`"),
                None => format!("no loaded plugin `{name}`"),
            },
        })
    })
}

/// `K` / `<CR>` — open the plugin under the cursor's documentation
/// (`:describe-plugin`).
pub fn describe_handler() -> ActionHandler {
    Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
        let name = plugin_name_at(ctx)?;
        Some(Effect::DescribePlugin { name })
    })
}

/// `gr` — re-render the list from the current status (a plugin loaded / reloaded
/// out of band since the view opened shows up).
pub fn refresh_handler() -> ActionHandler {
    Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
        refresh(ctx);
        None
    })
}

/// `t` — open the per-plugin boundary-trace view (`*plugin-trace:<name>*`) for the
/// plugin under the cursor. Returns the generic `OpenSyntheticBuffer` effect; the
/// host ensures the buffer under `plugin-trace-mode`, whose `on_activate` resolves
/// the name back to the plugin id and filters the tracer ring (PO.4.2). The
/// buffer-name scheme is single-sourced via `lattice_plugin_trace` so the manager
/// (producer) and the mode (consumer) can't drift.
pub fn trace_handler() -> ActionHandler {
    Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
        let name = plugin_name_at(ctx)?;
        Some(Effect::OpenSyntheticBuffer {
            name: lattice_plugin_trace::per_plugin_buffer_name(&name),
            mode_id: lattice_plugin_trace::TRACE_MODE_ID.to_string(),
            content: None,
            cursor: None,
            activate_minor: None,
        })
    })
}

/// `T` — cycle the trace verbosity of the plugin under the cursor
/// (off→error→warn→info→debug→trace→off) via `tracer.set_plugin_level` (PO.3's
/// per-plugin gate; PO.4.3). The tracer republishes to that plugin's hot gate, so
/// the change is live on the next keystroke — no re-render needed (the level
/// isn't a status-table column). A missing tracer service no-ops.
pub fn trace_level_handler() -> ActionHandler {
    Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
        let (id, name) = plugin_row_at(ctx)?;
        let tracer = ctx.services.get::<PluginTracerHandle>()?;
        let next = tracer.plugin_level(id).cycle_next();
        tracer.set_plugin_level(id, next);
        Some(Effect::Echo {
            level: EchoLevel::Info,
            text: format!("plugin `{name}` trace level → {}", next.as_str()),
        })
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn rebuild_is_registered_and_distinct_from_reload() {
        // `r` re-instantiates what is on disk; `b` rebuilds that first. Two
        // commands, because collapsing them would make `r` occasionally take
        // minutes.
        let mut commands = CommandRegistry::new();
        register_actions(&mut commands);
        assert!(commands.id_by_name(REBUILD).is_some());
        assert_ne!(REBUILD, RELOAD);
    }

    #[test]
    fn register_actions_registers_the_action_commands() {
        let mut commands = CommandRegistry::new();
        register_actions(&mut commands);
        for name in [RELOAD, UNLOAD, DESCRIBE, REFRESH, TRACE, TRACE_LEVEL] {
            assert!(
                commands.id_by_name(name).is_some(),
                "`{name}` must be registered so the keymap `cmd:` resolves"
            );
        }
    }

    #[test]
    fn the_trace_handler_opens_the_per_plugin_trace_buffer() {
        // The `t` handler's buffer name must round-trip through the trace crate's
        // parser — the single-sourced naming contract the mode relies on.
        let name = lattice_plugin_trace::per_plugin_buffer_name("fuzzy-finder");
        assert_eq!(
            lattice_plugin_trace::parse_per_plugin_name(&name),
            Some("fuzzy-finder"),
        );
        assert_eq!(
            lattice_plugin_trace::TRACE_MODE_ID,
            "plugin-trace-mode",
            "the drill-in targets the mode the trace crate registers"
        );
    }

    /// The reported bug, at the seam it lived in: the cursor on a plugin that
    /// FAILED to load resolved to no plugin, so `r`, `b` and `u` did nothing
    /// on the one entry they were wanted for.
    ///
    /// Through a real loader and its own failure record — a hand-built
    /// `FailedLoad` would test the render, which has its own tests, and not
    /// the join between the two.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_cursor_on_a_failed_plugins_entry_is_on_that_plugin() {
        use lattice_mode::ServiceRegistry;
        use lattice_plugin_host::PluginHost;
        use lattice_plugin_loader::PluginLoader;
        use lattice_protocol::ids::BufferId;
        use lattice_protocol::position::Position;
        use lattice_runtime::EventBus;

        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("plugins").join("org");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("plugin.toml"), "id = \"org\"\n").unwrap();
        std::fs::write(dir.join("component.wasm"), b"not a component").unwrap();

        let host =
            PluginHost::with_dirs(base.path().join("cache"), base.path().join("data")).unwrap();
        let loader: PluginLoaderHandle = Arc::new(PluginLoader::new(Arc::new(host)));
        loader
            .discover_and_load(&base.path().join("plugins"), TrustTier::UserInstalled)
            .await;
        assert_eq!(loader.failed_loads().len(), 1, "org failed to load");

        let mut services = ServiceRegistry::new();
        services.register::<PluginLoaderHandle>(loader.clone());
        let events = EventBus::new();
        let rendered = crate::mode::render_now(&loader);
        let lines: Vec<&str> = rendered.text.lines().collect();
        let name_line = lines
            .iter()
            .position(|l| l.trim_start().starts_with("org  ("))
            .expect("org has an entry");
        let heading = lines
            .iter()
            .position(|l| l.starts_with("## Failed to load"))
            .unwrap();

        let at = |line: usize| ActionContext {
            buffer_id: BufferId::new(0),
            cursor: Position {
                line: line as u32,
                byte: 0,
            },
            selection: None,
            services: &services,
            events: &events,
            prompt_value: None,
            args: lattice_grammar::Args::None,
            buffer_locals: None,
        };

        assert_eq!(plugin_name_at(&at(name_line)).as_deref(), Some("org"));
        assert_eq!(
            plugin_name_at(&at(name_line + 1)).as_deref(),
            Some("org"),
            "and on the reason under it"
        );
        assert_eq!(plugin_name_at(&at(heading)), None, "a heading is no plugin");
        assert_eq!(
            plugin_row_at(&at(name_line)),
            None,
            "it has a name to rebuild by, but no host id: the trace chords \
             still have nothing to act on"
        );
        // Describe is name-keyed, so it follows the name.
        assert!(matches!(
            describe_handler()(&at(name_line)),
            Some(Effect::DescribePlugin { name }) if name == "org"
        ));
    }

    #[test]
    fn every_handler_no_ops_without_its_services() {
        use lattice_mode::ServiceRegistry;
        use lattice_protocol::ids::BufferId;
        use lattice_protocol::position::Position;
        use lattice_runtime::EventBus;
        // An empty registry: no loader, buffer store, or tracer. Every handler must
        // degrade to a benign no-op (never a panic) — the module's stated contract
        // that a missing service short-circuits at the `ctx.services.get()?`.
        let services = ServiceRegistry::new();
        let events = EventBus::new();
        let ctx = ActionContext {
            buffer_id: BufferId::new(0),
            cursor: Position { line: 5, byte: 0 },
            selection: None,
            services: &services,
            events: &events,
            prompt_value: None,
            args: lattice_grammar::Args::None,
            buffer_locals: None,
        };
        assert!(
            reload_handler()(&ctx).is_none(),
            "reload no-ops without a loader"
        );
        assert!(
            unload_handler()(&ctx).is_none(),
            "unload no-ops without a loader"
        );
        assert!(
            describe_handler()(&ctx).is_none(),
            "describe no-ops without a loader"
        );
        assert!(
            trace_handler()(&ctx).is_none(),
            "trace no-ops without a loader"
        );
        assert!(
            trace_level_handler()(&ctx).is_none(),
            "trace-level no-ops without a loader"
        );
        assert!(
            refresh_handler()(&ctx).is_none(),
            "refresh returns None regardless"
        );
    }
}
