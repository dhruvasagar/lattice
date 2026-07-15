//! `PluginMetaSink` — the generic seam the plugin loader writes plugin
//! provenance through (PL8.B).
//!
//! The host owns a `PluginMetaRegistry` (host-issued `PluginId` → manifest name
//! + doc) that backs provenance display (`SourceLayer::Plugin(id)` renders as
//! the manifest name) and the loaded-plugin introspection surfaces
//! (`:list-plugins` / `:describe-plugin`). The plugin loader
//! (`lattice-plugin-loader`) must populate it as each plugin loads — but the
//! loader cannot name the host's `PluginMetaRegistry` type without a dependency
//! cycle. So the host implements this trait over its registry and registers the
//! same instance as a [`PluginMetaSinkHandle`] service; the loader looks the
//! handle up ([`SubsystemBoot::service`](crate::SubsystemBoot::service)) and
//! writes through it. Mirrors the `PluginEventSink` type-erasure pattern that
//! keeps `lattice-runtime` free of a plugin-host dep.

use std::sync::Arc;

/// The host-owned plugin-metadata store, viewed as a write seam. Implemented by
/// the host's `PluginMetaRegistry`; consumed by the plugin loader. All methods
/// take `&self` (the store is interior-mutable behind a lock) so the shared
/// `Arc` handle suffices — no `&mut` plumbing across the boundary.
pub trait PluginMetaSink: Send + Sync {
    /// Record a loaded plugin's manifest name + doc against its host-issued
    /// numeric id, so `SourceLayer::Plugin(id)` provenance renders as the name
    /// and `:list-plugins` shows it. Called once per plugin at load.
    fn register_plugin(&self, id: u32, name: String, doc: String);

    /// Forget a plugin's metadata (unload / reload). Idempotent — a
    /// never-registered or already-removed id is a no-op.
    fn unregister_plugin(&self, id: u32);
}

/// The service alias the host registers and the loader looks up. Per the
/// `ServiceRegistry` Arc/TypeId rule, register and look up with this exact type.
pub type PluginMetaSinkHandle = Arc<dyn PluginMetaSink>;
