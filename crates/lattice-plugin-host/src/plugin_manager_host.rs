//! PM.7: the host side of the `require` seam.
//!
//! Design: [`plugin-manager.md`](../../../docs/dev/architecture/plugin-manager.md)
//! §3, §6. A guest (in practice the user's `init.rs`) calls
//! `plugin-manager.require(spec)` from its `register-plugins` export; the
//! `Host` impl records the spec into this Store's [`RequireContributions`],
//! and [`PluginHost::spawn_plugin_manager_plugin`] drains them after the
//! export returns.
//!
//! The record-then-drain split is the `register-mode` / `register-grammar`
//! precedent, and here it is load-bearing rather than merely consistent: a
//! `require` that resolved inline would put a git clone and a cargo build
//! inside a guest call on the boot path. The host drains and runs the pipeline
//! off-thread instead, so the editor draws its first frame without waiting on
//! a network it may not even have.
//!
//! This module does **not** resolve, build or load anything. It converts a WIT
//! spec into a host [`RequiredPlugin`] and hands it back; the pipeline that
//! consumes it lives in `lattice-plugin-loader`, which is where `resolve` and
//! `build_plugin` already live and where the loader's registries are reachable.
//! Keeping the boundary this thin is what lets the pipeline be tested without
//! standing up a wasm guest at all.

use crate::{
    Component, PluginBudget, PluginHost, PluginHostError, PluginManifest, TrustTier, arm_store,
    classify_trap,
};

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        world: "plugin-manager-plugin",
        path: "../../wit",
        // `register-plugins` shares the async linker with WASI + logging, so
        // the export is async; `require` itself is a sync host func (it only
        // records into `PluginState`) — the `modes` / `config` shape.
        exports: { default: async },
    });
}

/// Where a required plugin comes from. The host-side mirror of the WIT
/// `plugin-source` variant.
///
/// Deliberately re-declared here rather than shared with
/// `lattice_plugin_loader::PluginSource`: the loader must not depend on the
/// plugin host (the dependency runs the other way — `loader → host`), and a
/// WIT-facing type that changed shape because a loader refactor touched it
/// would be a public API breaking on an internal edit. The conversion is one
/// `match` at the call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequiredSource {
    Local(String),
    Git { url: String, rev: Option<String> },
    Prebuilt { url: String },
}

/// One plugin a guest declared via `require`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredPlugin {
    pub name: String,
    pub source: RequiredSource,
    /// The mode to enable once the plugin loads. The host carries this as an
    /// opaque string and never interprets it — the mode is the plugin's own
    /// surface (`feedback_mode_owns_its_surface`).
    pub enable_mode: Option<String>,
    pub pinned: bool,
}

/// The per-plugin accumulator the `plugin_manager::Host` impl records into
/// during `register-plugins`. Drained after the export returns.
#[derive(Default)]
pub(crate) struct RequireContributions {
    recorded: Vec<RequiredPlugin>,
}

impl RequireContributions {
    pub fn record(&mut self, spec: RequiredPlugin) {
        self.recorded.push(spec);
    }

    pub fn take(&mut self) -> Vec<RequiredPlugin> {
        std::mem::take(&mut self.recorded)
    }
}

/// Is `name` a single safe path component?
///
/// A required plugin's name becomes a directory under the cache root and the
/// user's plugin root, so an unchecked name is a path-traversal write with the
/// editor's full authority — `../../.ssh` is a plausible entry in a config
/// file someone copy-pasted. Same gate the untrusted `manifest.id` already
/// gets before it keys the writable data mount; this is the second untrusted
/// string to reach a path, so it gets the same treatment rather than a
/// bespoke one.
pub fn is_safe_plugin_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name != "."
        && name != ".."
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl PluginHost {
    /// Instantiate `component` as a plugin-manager guest, call its
    /// `register-plugins` export, and return the specs it declared.
    ///
    /// Returns the host-issued id alongside the specs — the `spawn_mode_plugin`
    /// shape — so the loader can record the guest as loaded even when it
    /// declared nothing.
    ///
    /// The specs are *declarations*, not loaded plugins: the caller runs
    /// resolve → build → load off-thread (§5).
    pub async fn spawn_plugin_manager_plugin(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        budget: PluginBudget,
        trust: TrustTier,
    ) -> Result<(crate::PluginId, Vec<RequiredPlugin>), PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, trust);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "plugin-manager guest loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(wasi, outcome.grant, budget, Some(&manifest.id))?;
        let bindings =
            bindings::PluginManagerPlugin::instantiate_async(&mut store, component, &self.linker)
                .await
                .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        let plugin_id = self.alloc_id();
        // PO.5: route this guest's `logging` calls into the tracer before the
        // export runs, so an `init.rs` can narrate what it is requiring.
        store.data_mut().log_ctx = self.log_ctx_for(plugin_id);

        arm_store(&mut store, budget)?;
        bindings
            .call_register_plugins(&mut store)
            .await
            .map_err(|source| PluginHostError::Trap {
                func: "register-plugins",
                kind: classify_trap(&source),
                source: source.into(),
            })?;
        Ok((plugin_id, store.data_mut().require_contributions.take()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_names_are_accepted() {
        for name in ["auto-pair", "vim_surround", "a", "plugin9"] {
            assert!(is_safe_plugin_name(name), "{name} should be accepted");
        }
    }

    #[test]
    fn traversal_and_separators_are_rejected() {
        // The failure this prevents is a write outside the cache root with
        // the editor's own authority.
        for name in [
            "..",
            ".",
            "../evil",
            "a/b",
            "a\\b",
            "/abs",
            ".hidden",
            "",
            "with space",
            "semi;colon",
        ] {
            assert!(!is_safe_plugin_name(name), "{name} must be rejected");
        }
    }

    #[test]
    fn an_absurdly_long_name_is_rejected() {
        assert!(!is_safe_plugin_name(&"a".repeat(129)));
        assert!(is_safe_plugin_name(&"a".repeat(128)));
    }

    #[test]
    fn contributions_record_and_drain_once() {
        let mut c = RequireContributions::default();
        c.record(RequiredPlugin {
            name: "demo".into(),
            source: RequiredSource::Local("/tmp/demo".into()),
            enable_mode: None,
            pinned: false,
        });
        assert_eq!(c.take().len(), 1);
        assert!(
            c.take().is_empty(),
            "a drained accumulator must not replay — a second drain would \
             resolve and build every plugin twice"
        );
    }
}
