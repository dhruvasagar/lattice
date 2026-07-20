//! The `config` guest→host option-declaration seam (PH7.10).
//!
//! A config plugin implements the `config-plugin` world: it **imports** the
//! `config` API (`register-option` / `get-option`) and **exports**
//! `register-options` (the host calls it once to drive declaration). This module
//! holds the `bindgen!` for that world plus the host-side registration logic —
//! the type mapping from the WIT `option-type` to a native `OptionType` impl,
//! factored here so it is unit-testable without a `Store` (the `host_services`
//! precedent).
//!
//! **The canonical API is the WIT** (`config.wit`) — any component-model language
//! calls `register-option` directly. A plugin option lands in the SAME
//! [`ConfigRegistry`](lattice_config::ConfigRegistry) core options use, built as a
//! concrete `Option<bool|i64|String>` via the public `OptionType` parse/format
//! contract, so `:set` / `:describe-option` / `gen:options` completion /
//! `OptionChanged` treat it uniformly with NO host kind-branch.
//!
//! Registration flow (the `register-events` precedent): the host sets the
//! `ConfigRegistry` handle on `PluginState`, calls the guest's `register-options`
//! export, and the guest calls the imported `register-option` — which registers
//! directly into the handle (no drain step; options are declared synchronously,
//! unlike event subscriptions which need bus wiring).

use std::sync::Arc;

use lattice_config::option::Option as ConfigOption;
use lattice_config::{ConfigRegistry, OptionType};

use crate::{
    Component, PluginBudget, PluginHost, PluginHostError, PluginManifest, TrustTier, arm_store,
    classify_trap,
};

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        world: "config-plugin",
        path: "../../wit",
        // `register-options` is wired into the SAME async linker as WASI + the
        // `config` host funcs (`lib.rs`), so the export is async (the `events`
        // world precedent: async export, sync `register-option`/`get-option` host
        // funcs). Registration is off any hot path, so async is free.
        exports: { default: async },
    });
}

/// The native value type a plugin option maps to — the host-side mirror of the
/// WIT `option-type` enum, kept as a plain enum (no bindgen type) so the
/// registration logic is unit-testable without the generated bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginOptionKind {
    Boolean,
    Integer,
    String,
}

/// The `register-option` host-service body (PH7.10). Builds a concrete
/// `Option<bool|i64|String>` from the plugin's declaration and registers it into
/// the SAME `ConfigRegistry` core options use. Returns `false` (registering
/// nothing) if `default` doesn't parse for `kind` OR `name` collides with an
/// existing option — a plugin must not silently shadow another option.
///
/// The name-collision check runs BEFORE any string leak (below), so a rejected
/// registration allocates nothing.
pub fn register_plugin_option(
    registry: &ConfigRegistry,
    name: &str,
    kind: PluginOptionKind,
    default: &str,
    doc: &str,
) -> bool {
    if registry.lookup(name).is_some() {
        return false;
    }
    match kind {
        PluginOptionKind::Boolean => build_and_register::<bool>(registry, name, default, doc),
        PluginOptionKind::Integer => build_and_register::<i64>(registry, name, default, doc),
        PluginOptionKind::String => build_and_register::<String>(registry, name, default, doc),
    }
}

/// Parse `default` for the concrete `T`, then register `Option<T>`. The default
/// is parsed FIRST so a malformed default never leaks the name/doc strings.
///
/// A plugin's `name`/`doc` arrive as owned `String`s over WIT. PL8.F made
/// `ConfigRegistry`'s option `name`/`doc` `Cow<'static, str>`, so these pass
/// straight through as `Cow::Owned` — no `Box::leak`. The strings free with the
/// entry on `ConfigRegistry::unregister` (PH7.12b.1b), so repeated
/// `:plugin-reload` / `:reload-config` no longer grows the interned-string
/// footprint (the leak PH7.12b.2 decision C deferred until the reload consumer
/// existed to exercise it — now closed).
fn build_and_register<T: OptionType>(
    registry: &ConfigRegistry,
    name: &str,
    default: &str,
    doc: &str,
) -> bool {
    let Ok(value) = T::parse(default) else {
        return false;
    };
    // PL8.F: the plugin's runtime `name`/`doc` become `Cow::Owned` on the native
    // option — no `Box::leak`. They free with the entry on
    // `ConfigRegistry::unregister`, so repeated `:plugin-reload` / `:reload-config`
    // no longer grows the interned-string footprint.
    registry
        .try_register(ConfigOption::<T>::new(name.to_owned(), value, doc.to_owned()))
        .is_ok()
}

impl PluginHost {
    /// Instantiate a `config-plugin` component under its capability grant, run its
    /// `register-options` export to declare options into `registry`, and return
    /// the names it registered. Grant / data-dir / WASI are identical to
    /// [`instantiate_plugin`](PluginHost::instantiate_plugin) (shared
    /// `build_plugin_wasi` + `new_store`).
    ///
    /// The registry handle is wired onto the `Store` BEFORE `register-options`
    /// runs, so the guest's imported `register-option` / `get-option` reach it.
    /// Options are declared synchronously (no drain / actor, unlike events); the
    /// returned names are the drain of the plugin's `config_contributions` (the
    /// PH7.12 teardown seam will unregister them).
    pub async fn spawn_config_plugin(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        budget: PluginBudget,
        registry: &Arc<ConfigRegistry>,
    ) -> Result<(crate::PluginId, Vec<String>), PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, tier);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "config plugin loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(wasi, outcome.grant, budget)?;
        let bindings =
            bindings::ConfigPlugin::instantiate_async(&mut store, component, &self.linker)
                .await
                .map_err(|e| PluginHostError::Instantiate(e.into()))?;

        // A host-issued id so a config-only plugin keys `:list-plugins` /
        // provenance uniformly with the seam plugins that mint one (picker /
        // event). The config contributions register into `ConfigRegistry` by
        // name, not by `SourceLayer::Plugin(id)`, so this id backs the loader's
        // loaded-record, not per-option provenance. Allocated BEFORE
        // `register-options` so the guest can also narrate from there (PO.5).
        let id = self.alloc_id();

        // Wire the registry BEFORE `register-options` runs so the guest's imported
        // `register-option` / `get-option` reach it.
        store.data_mut().config_registry = Some(Arc::clone(registry));
        // PO.5: route this plugin's `logging` calls into the tracer (Layer 2).
        store.data_mut().log_ctx = self.log_ctx_for(id);

        arm_store(&mut store, budget)?;
        bindings
            .call_register_options(&mut store)
            .await
            .map_err(|source| PluginHostError::Trap {
                func: "register-options",
                kind: classify_trap(&source),
                source: source.into(),
            })?;

        let names = store.data_mut().config_contributions.drain(..).collect();
        Ok((id, names))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    /// A fresh, empty registry (no linkme core options) — the plugin options are
    /// the only entries, so assertions are hermetic.
    fn registry() -> ConfigRegistry {
        ConfigRegistry::default()
    }

    #[test]
    fn registers_each_type_and_reads_back_formatted() {
        let r = registry();
        assert!(register_plugin_option(
            &r,
            "plugin.flag",
            PluginOptionKind::Boolean,
            "true",
            "a flag"
        ));
        assert!(register_plugin_option(
            &r,
            "plugin.count",
            PluginOptionKind::Integer,
            "3",
            "a count"
        ));
        assert!(register_plugin_option(
            &r,
            "plugin.label",
            PluginOptionKind::String,
            "hi",
            "a label"
        ));

        // Each lands in the registry, formatted through its native OptionType.
        assert_eq!(r.lookup("plugin.flag").unwrap().get_formatted(), "true");
        assert_eq!(r.lookup("plugin.count").unwrap().get_formatted(), "3");
        assert_eq!(r.lookup("plugin.label").unwrap().get_formatted(), "hi");
        // The mapped native type is reflected in the erased type_label.
        assert_eq!(r.lookup("plugin.flag").unwrap().type_label(), "boolean");
        assert_eq!(r.lookup("plugin.count").unwrap().type_label(), "integer");
        assert_eq!(r.lookup("plugin.label").unwrap().type_label(), "string");
    }

    #[test]
    fn bad_default_is_rejected_and_registers_nothing() {
        let r = registry();
        assert!(!register_plugin_option(
            &r,
            "plugin.count",
            PluginOptionKind::Integer,
            "not-a-number",
            "count"
        ));
        assert!(
            r.lookup("plugin.count").is_none(),
            "a rejected default registers nothing"
        );
    }

    #[test]
    fn duplicate_name_is_rejected_keeping_the_original() {
        let r = registry();
        assert!(register_plugin_option(
            &r,
            "plugin.x",
            PluginOptionKind::Boolean,
            "true",
            "first"
        ));
        // A second registration under the same name is refused (no silent shadow).
        assert!(!register_plugin_option(
            &r,
            "plugin.x",
            PluginOptionKind::String,
            "hi",
            "second"
        ));
        assert_eq!(
            r.lookup("plugin.x").unwrap().type_label(),
            "boolean",
            "the original option is untouched"
        );
    }

    #[test]
    fn set_and_get_round_trip_through_the_registry() {
        let r = registry();
        register_plugin_option(&r, "plugin.count", PluginOptionKind::Integer, "3", "count");
        // A plugin option is a first-class registry entry: `:set` works uniformly.
        r.parse_and_set_command("plugin.count=7")
            .expect(":set works");
        assert_eq!(r.lookup("plugin.count").unwrap().get_formatted(), "7");
    }
}
