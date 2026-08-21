//! PL8.A/B — the crate-owned `install(boot)` entry point.
//!
//! Wiring the loader into the editor is **one line** in the host's Phase-B
//! install list (`lattice_plugin_loader::install(&mut boot)`) and zero host
//! internals — the mode-ownership acid test: no `Editor::` method, no host
//! `Action` variant. `install` stands the runtime up, captures the editor
//! environment (runtime handle, event bus, the runtime-mutable picker registry,
//! the provenance sink) from the generic `SubsystemBoot` seams, registers the
//! [`PluginLoaderHandle`] service, and spawns on-disk discovery **off the boot
//! thread** so no plugin cold-start delays boot.

use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_mode::{PluginMetaSinkHandle, SubsystemBoot};
use lattice_picker::PickerRegistryHandle;
use lattice_plugin_host::{
    PluginHost, PluginTracePushed, PluginTracer, PluginTracerHandle, TraceLevel, TrustTier,
};
use lattice_protocol::{Event, EventKind};
use lattice_runtime::{EventFilter, SubscriptionTarget};

use std::sync::atomic::{AtomicBool, Ordering};

use crate::{LoaderServices, PluginLoader, PluginLoaderHandle};

/// Process-global opt-out of boot-time plugin auto-discovery, set by
/// [`disable_autoload`]. Checked (together with the
/// `LATTICE_DISABLE_PLUGIN_AUTOLOAD` env var) in [`install`].
static AUTOLOAD_DISABLED: AtomicBool = AtomicBool::new(false);

/// Suppress boot-time plugin auto-discovery for this process — the loader
/// service and its ex-commands are still installed, so manual `:plugin-load`
/// works, but [`install`] does not scan the core-plugins / `init.rs` / on-disk
/// plugin directories. For tests and embedders that must not pick up a
/// developer's real `~/.config/lattice`. Idempotent; equivalent to setting the
/// `LATTICE_DISABLE_PLUGIN_AUTOLOAD` env var. Safe (no `unsafe` env mutation).
pub fn disable_autoload() {
    AUTOLOAD_DISABLED.store(true, Ordering::Relaxed);
}

fn autoload_disabled() -> bool {
    AUTOLOAD_DISABLED.load(Ordering::Relaxed)
        || std::env::var_os("LATTICE_DISABLE_PLUGIN_AUTOLOAD").is_some()
}

pub fn install(boot: &mut impl SubsystemBoot) {
    let host = match PluginHost::new() {
        Ok(host) => Arc::new(host),
        Err(err) => {
            tracing::warn!(
                error = %err,
                "plugin host unavailable; the editor runs without plugin support"
            );
            return;
        }
    };

    // PO.1 (plugin observability): stand the boundary tracer up — its publisher
    // bound to the runtime bus so every appended `PluginTraceRecord` streams as a
    // typed `PluginTracePushed` event (the `LspLogger` boot-wiring precedent). The
    // seams emit into it (PO.2/PO.3, via `LoaderServices.tracer`); the trace-buffer
    // views subscribe (PO.4). Off the hot path by contract — this only wires it.
    let tracer: PluginTracerHandle = Arc::new(PluginTracer::with_defaults());
    let trace_bus = boot.event_bus().clone();
    tracer.set_event_publisher(Box::new(move |record| {
        trace_bus.publish_typed(PluginTracePushed { record });
    }));
    // PO.5: hand the tracer to the host so each instantiate/spawn stamps the
    // plugin's `log_ctx` and the guest `logging` seam (Layer 2) routes into the
    // same ring as the boundary trace.
    host.set_tracer(tracer.clone());

    // Capture the editor environment from the generic boot seams. `service`
    // returns `Arc<Handle-alias>` (double-Arc); unwrap one layer to the handle.
    let services = LoaderServices {
        runtime: Some(boot.runtime_handle().clone()),
        bus: Some(boot.event_bus().clone()),
        picker_registry: boot.service::<PickerRegistryHandle>().map(|h| (*h).clone()),
        config_registry: boot.service::<Arc<ConfigRegistry>>().map(|h| (*h).clone()),
        command_registry: boot
            .service::<lattice_grammar::CommandRegistryHandle>()
            .map(|h| (*h).clone()),
        mode_registry: boot
            .service::<lattice_mode::ModeRegistryHandle>()
            .map(|h| (*h).clone()),
        keymap: boot
            .service::<lattice_keymap::KeymapHandle>()
            .map(|h| (*h).clone()),
        meta_sink: boot.service::<PluginMetaSinkHandle>().map(|h| (*h).clone()),
        decoration_registry: boot
            .service::<lattice_mode::GutterDecorationSourceRegistryHandle>()
            .map(|h| (*h).clone()),
        context_registry: boot
            .service::<lattice_mode::ContextSourceRegistryHandle>()
            .map(|h| (*h).clone()),
        theme_registry: boot
            .service::<lattice_theme::ThemeRegistryHandle>()
            .map(|h| (*h).clone()),
        tracer: Some(tracer.clone()),
    };
    if services.picker_registry.is_none() {
        // The host always registers the picker registry; its absence means a
        // boot-order regression. Degrade to no plugin support, logged.
        tracing::warn!("picker registry service missing; the editor runs without plugin support");
        return;
    }

    let loader: PluginLoaderHandle = Arc::new(PluginLoader::with_services(host, services));
    // Option A (PL8.C.2): the loader self-registers its `:plugin-load` /
    // `:plugin-unload` / `:plugin-reload` ex-commands into the runtime-mutable
    // command registry — zero host code, the full command surface owned by the
    // loader crate.
    loader.register_ex_commands();
    // PL8.H.1: track plugin health for the manager view — a `PluginCrashed`
    // subscription drained on the runtime flips a trapped plugin to quarantined.
    loader.subscribe_health();
    // PM.3: react to `<id>.enabled` toggles — activate/deactivate a core plugin's
    // default mode live (`:set auto-pair.enabled=false`).
    loader.subscribe_mode_gates();
    boot.register_service::<PluginLoaderHandle>(loader.clone());

    // PO.4.3: observe `:set plugin.trace-level=…` and push the new default into
    // the tracer live — PO.3's republish then reaches every un-overridden hot gate
    // on the next keystroke. Mechanism B (the dashboard `install_recompose_triggers`
    // precedent): the subsystem that OWNS the tracer subscribes its own
    // `OptionChanged` channel and name-filters, rather than adding a plugin arm to
    // the host's option cascade (mode-ownership — the App stays a thin host). The
    // event carries the new value as a string, so no `lattice-config` value-type
    // coupling — `TraceLevel::parse` bridges (the labels match `PluginTraceLevel`).
    let observer_tracer = tracer.clone();
    let (option_tx, mut option_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    boot.event_bus().subscribe(
        EventFilter::kind(EventKind::OptionChanged),
        SubscriptionTarget::Channel(option_tx),
    );
    boot.runtime_handle().spawn(async move {
        while let Some(event) = option_rx.recv().await {
            let Event::OptionChanged { name, new, .. } = event else {
                continue;
            };
            if name != "plugin.trace-level" {
                continue;
            }
            match TraceLevel::parse(&new) {
                Some(level) => observer_tracer.set_default_level(level),
                None => tracing::warn!(value = %new, "ignoring unparseable plugin.trace-level"),
            }
        }
    });

    // PO.1: register the tracer (built above) as a boot service so the
    // trace-buffer views (PO.4) resolve it; the seams already hold it via
    // `LoaderServices.tracer`.
    boot.register_service::<PluginTracerHandle>(tracer);

    // Headless / CI / test opt-out: `LATTICE_DISABLE_PLUGIN_AUTOLOAD` skips the
    // boot-time filesystem discovery below (core plugins + init.rs + on-disk
    // plugins), so a developer's real `~/.config/lattice` never leaks into a
    // test editor and CI can boot deterministically. The loader service and its
    // `:plugin-load` / `:plugin-unload` / `:plugin-reload` ex-commands are
    // already wired above, so manual loading still works — only auto-discovery
    // is suppressed.
    if autoload_disabled() {
        tracing::debug!("plugin auto-load disabled; skipping boot-time plugin discovery");
        return;
    }

    // CI.2: `init.rs` loads FIRST, then on-disk plugins — in ONE task, off the
    // boot thread. init.rs is config authority (`<config>/lattice/init/`, loaded
    // with a boot-capability `Bundled` tier); it must register its
    // `plugin-loaded` subscriptions BEFORE any plugin fires that event, so a
    // deferred `on-plugin-loaded` handler can't miss its plugin
    // (config-and-init.md §3). Both stay OFF the boot thread — a plugin
    // cold-start must not delay boot; contributions appear a frame or two after
    // boot (the eventual-consistency the UX contract permits). An absent init /
    // plugins dir (the common case) is a benign skip.
    let init_dir = crate::default_init_dir();
    let plugins_dir = crate::default_plugins_dir();
    // PM.1: the core-plugins root — prebuilt plugins that ship WITH lattice,
    // discovered from a runtime search path (plugin-manager.md §7). `None` when no
    // runtime root is present (a source checkout with no staged `runtime/`, etc.).
    let core_plugins_dir = crate::default_core_plugins_dir();
    // PL8.D.4: watch the init dir so a rebuilt `init.wasm` auto-reloads without a
    // manual `:reload-config`. A no-op if the dir doesn't exist.
    if let Some(ref dir) = init_dir {
        crate::watch::spawn_init_watcher(loader.clone(), dir.clone(), boot.runtime_handle());
    }
    if core_plugins_dir.is_some() || init_dir.is_some() || plugins_dir.is_some() {
        let loader = loader.clone();
        boot.runtime_handle().spawn(async move {
            // 0. CORE plugins first (prebuilt runtime root, `Bundled` tier) — PM.1.
            //    They enable via a config gate (PM.3), NOT init.rs, so loading them
            //    before init.rs is correct: a user init.rs `on-plugin-loaded`
            //    handler targets USER plugins (step 2), which still load after it.
            if let Some(core_dir) = core_plugins_dir {
                let n = loader
                    .discover_and_load(&core_dir, TrustTier::Bundled)
                    .await;
                if n > 0 {
                    tracing::info!(
                        count = n,
                        dir = %core_dir.display(),
                        "core plugins loaded from the runtime root"
                    );
                }
            }
            // 1. init.rs next — AWAITED, so its subscriptions are live before
            //    step 2 loads plugins that fire `plugin-loaded`.
            if let Some(init_dir) = init_dir {
                // PM.7b: build init.rs before loading it. It is a
                // `wasm32-wasip2` component like any other, so the PM.5
                // service builds it — one build primitive, two callers
                // (design §6). This is what removes the "run cargo by hand
                // first" step: an edited init.rs rebuilds on the next boot,
                // and an unchanged one is a pure load that never invokes a
                // toolchain.
                //
                // Failure is a skip. A user whose init.rs stopped compiling
                // must still get an editor — with their previous init.wasm if
                // one exists (`StaleKept`), and without config if not.
                build_init_if_needed(&init_dir).await;
                match loader.load_path(&init_dir, TrustTier::Bundled).await {
                    Ok(id) => tracing::info!(
                        id = id.0,
                        dir = %init_dir.display(),
                        "user init.rs config loaded"
                    ),
                    Err(err) => tracing::debug!(
                        dir = %init_dir.display(),
                        error = %err,
                        "no user init.rs loaded"
                    ),
                }
                // PM.7b: whatever init.rs `require`d is now queued. Resolve,
                // build and load it BEFORE step 2's on-disk scan, so a plugin
                // that was just installed into the user root is discovered by
                // that scan rather than waiting for the next boot.
                install_required_plugins(&loader).await;
            }
            // 2. Then the plugins the init.rs handlers react to.
            if let Some(dir) = plugins_dir {
                let n = loader
                    .discover_and_load(&dir, TrustTier::UserInstalled)
                    .await;
                if n > 0 {
                    tracing::info!(count = n, dir = %dir.display(), "plugins loaded from disk");
                }
            }
        });
    }
}

/// PM.7b: build the user's `init.rs` if its source changed.
///
/// A no-op when the directory holds no cargo project — the common case today
/// is a hand-built `init.wasm` dropped in place, and that must keep working.
/// The build stages into the same directory the loader then discovers, so
/// nothing downstream needs to know a build happened.
///
/// Runs on `spawn_blocking`: a cold component build is seconds to minutes and
/// this is inside the boot task, which shares the async runtime with the
/// editor (paramount goal #1 / #4).
async fn build_init_if_needed(init_dir: &std::path::Path) {
    if !init_dir.join("Cargo.toml").is_file() {
        tracing::debug!(
            dir = %init_dir.display(),
            "init dir is not a cargo project; loading any prebuilt init.wasm as-is"
        );
        return;
    }
    let dir = init_dir.to_path_buf();
    let outcome = match tokio::task::spawn_blocking(move || {
        let parent = dir.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        crate::build::build_plugin(
            &crate::build::CargoComponentBuilder,
            &dir,
            "init",
            &parent,
            false,
        )
    })
    .await
    {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(error = %e, "init.rs build task failed to run");
            return;
        }
    };
    match outcome.error() {
        Some(error) => tracing::warn!(
            dir = %init_dir.display(),
            %error,
            "init.rs build failed; using the previous build if there is one"
        ),
        None => tracing::debug!(dir = %init_dir.display(), "init.rs is current"),
    }
}

/// PM.7b: resolve, build and load everything `init.rs` declared via `require`.
///
/// Each spec is independent: one broken source costs that plugin and nothing
/// else. The whole pipeline runs on `spawn_blocking` — it clones, downloads
/// and compiles — and only the final load returns to the async context.
async fn install_required_plugins(loader: &std::sync::Arc<crate::PluginLoader>) {
    let specs = loader.take_required();
    if specs.is_empty() {
        return;
    }
    let Some(user_root) = crate::default_plugins_dir() else {
        tracing::warn!("require: no config dir; cannot install declared plugins");
        return;
    };
    let cache_root = crate::default_source_cache_dir();
    tracing::info!(
        count = specs.len(),
        "installing plugins declared by init.rs"
    );

    let installs = match tokio::task::spawn_blocking({
        let user_root = user_root.clone();
        move || {
            crate::pipeline::install_all(
                &crate::resolve::SystemGit,
                &crate::resolve::HttpFetcher,
                &crate::build::CargoComponentBuilder,
                &specs,
                &cache_root,
                &user_root,
            )
        }
    })
    .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "require: install task failed to run");
            return;
        }
    };

    for install in installs {
        match install {
            crate::pipeline::Install::Ready { name, stale, .. } => {
                if let Some(error) = stale {
                    tracing::warn!(
                        plugin = %name,
                        %error,
                        "plugin is running a previous build (rebuild failed)"
                    );
                }
                // The artifact is staged in the user root; load it through the
                // ordinary discovery path so a `require`d plugin and a
                // hand-installed one take exactly the same route in.
                let dir = user_root.join(&name);
                match loader.load_path(&dir, TrustTier::UserInstalled).await {
                    Ok(id) => {
                        tracing::info!(plugin = %name, id = id.0, "required plugin loaded")
                    }
                    Err(err) => tracing::warn!(
                        plugin = %name,
                        error = %err,
                        "required plugin built but failed to load"
                    ),
                }
            }
            crate::pipeline::Install::Skipped { name, error } => tracing::warn!(
                plugin = %name,
                %error,
                "required plugin skipped"
            ),
        }
    }
}
