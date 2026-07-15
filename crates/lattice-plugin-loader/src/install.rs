//! PL8.A — the crate-owned `install(boot)` entry point.
//!
//! Wiring the loader into the editor is **one line** in the host's Phase-B
//! install list (`lattice_plugin_loader::install(&mut boot)`) and zero host
//! internals — the mode-ownership acid test: no `Editor::` method, no host
//! `Action` variant. `install` stands the runtime up, wraps it in a
//! [`PluginLoader`], and registers the [`PluginLoaderHandle`] service so the
//! user surface (PL8.C) and manager view (PL8.H) reach it generically. PL8.B
//! drives on-disk discovery + load from here, off the boot thread via
//! `boot.runtime_handle()`.

use std::sync::Arc;

use lattice_mode::SubsystemBoot;
use lattice_plugin_host::PluginHost;

use crate::{PluginLoader, PluginLoaderHandle};

/// Stand the plugin loader up at boot and register its handle as a service.
///
/// Graceful degradation (the four-artefact clause): if the wasmtime engine
/// cannot be built (an unsupported target, a bad cache dir), the editor degrades
/// to **no plugin support** — logged, never a failed boot. Every native
/// subsystem still installs; the editor runs fully, just without plugins.
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
    let loader: PluginLoaderHandle = Arc::new(PluginLoader::new(host));
    boot.register_service::<PluginLoaderHandle>(loader);
    // PL8.B: `boot.runtime_handle().spawn(discover_and_load(loader.clone()))`
    // scans the plugins dir and loads each discovered component off the boot
    // thread. PL8.A registers the service only — no real plugins load yet.
}
