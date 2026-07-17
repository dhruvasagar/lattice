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
use lattice_plugin_host::{PluginHost, TrustTier};

use crate::{LoaderServices, PluginLoader, PluginLoaderHandle};

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
    };
    if services.picker_registry.is_none() {
        // The host always registers the picker registry; its absence means a
        // boot-order regression. Degrade to no plugin support, logged.
        tracing::warn!("picker registry service missing; the editor runs without plugin support");
        return;
    }

    let loader: PluginLoaderHandle =
        Arc::new(PluginLoader::with_services(host, services));
    // Option A (PL8.C.2): the loader self-registers its `:plugin-load` /
    // `:plugin-unload` / `:plugin-reload` ex-commands into the runtime-mutable
    // command registry — zero host code, the full command surface owned by the
    // loader crate.
    loader.register_ex_commands();
    // PL8.H.1: track plugin health for the manager view — a `PluginCrashed`
    // subscription drained on the runtime flips a trapped plugin to quarantined.
    loader.subscribe_health();
    boot.register_service::<PluginLoaderHandle>(loader.clone());

    // Discover + load on-disk plugins OFF the boot thread: a plugin cold-start
    // must not delay boot, and the load path is async (instantiate/activate/
    // spawn-seam). Contributions appear a frame or two after boot — the
    // eventual-consistency the UX contract permits for non-edited content. A
    // missing plugins dir (the common case) is a benign empty scan.
    if let Some(dir) = crate::default_plugins_dir() {
        let loader = loader.clone();
        boot.runtime_handle().spawn(async move {
            let n = loader.discover_and_load(&dir, TrustTier::UserInstalled).await;
            if n > 0 {
                tracing::info!(count = n, dir = %dir.display(), "plugins loaded from disk");
            }
        });
    }

    // PL8.D.3: the user's `init.rs` — a single plugin dir at
    // `<config>/lattice/init/`, loaded with a boot-capability (`Bundled`) tier
    // (the user's own trusted config, not an external install). Loaded OFF the
    // boot thread, AFTER the native builtins register (this `install` is seated
    // late in boot), so user keymaps / commands / options layer on top of the
    // defaults. An absent init dir (the common case — no user config) is a benign
    // debug skip, never a warn.
    if let Some(init_dir) = crate::default_init_dir() {
        // PL8.D.4: watch the init dir so a rebuilt `init.wasm` auto-reloads
        // without a manual `:reload-config`. A no-op if the dir doesn't exist.
        crate::watch::spawn_init_watcher(loader.clone(), init_dir.clone(), boot.runtime_handle());
        boot.runtime_handle().spawn(async move {
            match loader.load_path(&init_dir, TrustTier::Bundled).await {
                Ok(id) => {
                    tracing::info!(id = id.0, dir = %init_dir.display(), "user init.rs config loaded")
                }
                Err(err) => {
                    tracing::debug!(dir = %init_dir.display(), error = %err, "no user init.rs loaded")
                }
            }
        });
    }
}
