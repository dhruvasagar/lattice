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
        meta_sink: boot.service::<PluginMetaSinkHandle>().map(|h| (*h).clone()),
    };
    if services.picker_registry.is_none() {
        // The host always registers the picker registry; its absence means a
        // boot-order regression. Degrade to no plugin support, logged.
        tracing::warn!("picker registry service missing; the editor runs without plugin support");
        return;
    }

    let loader: PluginLoaderHandle =
        Arc::new(PluginLoader::with_services(host, services));
    boot.register_service::<PluginLoaderHandle>(loader.clone());

    // Discover + load on-disk plugins OFF the boot thread: a plugin cold-start
    // must not delay boot, and the load path is async (instantiate/activate/
    // spawn-seam). Contributions appear a frame or two after boot — the
    // eventual-consistency the UX contract permits for non-edited content. A
    // missing plugins dir (the common case) is a benign empty scan.
    if let Some(dir) = crate::default_plugins_dir() {
        boot.runtime_handle().spawn(async move {
            let n = loader.discover_and_load(&dir, TrustTier::UserInstalled).await;
            if n > 0 {
                tracing::info!(count = n, dir = %dir.display(), "plugins loaded from disk");
            }
        });
    }
}
