//! The `signs` guest→host sign-declaration seam (SG.3a).
//!
//! A sign-contributing plugin implements the `sign-plugin` world: it
//! **imports** the `signs` API (`define-sign`) and **exports** `register-signs`,
//! which the host calls once to drive declaration. This module holds the
//! `bindgen!` for that world plus the host-side conversion from the WIT
//! `sign-spec` to a native [`SignDefinition`](lattice_mode::SignDefinition),
//! factored out so it is unit-testable without a `Store` (the `theme_host`
//! precedent).
//!
//! **The canonical API is the WIT** (`signs.wit`) — any component-model
//! language calls `define-sign` directly. A plugin's sign lands in the SAME
//! registry native producers use, so it is styled through the ordinary theme
//! registry, contends for the mark cell by the same `priority` rule, and needs
//! NO host kind-branch.
//!
//! See `docs/dev/architecture/gutter-signs.md`.

use lattice_mode::{SignDefinition, SignRegistry, SignRegistryHandle};

use crate::{
    Component, PluginBudget, PluginHost, PluginHostError, PluginManifest, TrustTier, arm_store,
    classify_trap,
};

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        world: "sign-plugin",
        path: "../../wit",
        // Wired into the same async linker as WASI + the `signs` host funcs, so
        // the export is async (the `theme-plugin` precedent). Registration is
        // off every hot path, so async costs nothing.
        exports: { default: async },
        with: {
            "lattice:plugin-host/logging": crate::lattice::plugin_host::logging,
        },
    });
}

use bindings::lattice::plugin_host::signs as wit;

/// Convert a WIT `sign-spec` plus its namespaced name to the native
/// definition.
///
/// Total — every WIT shape maps, so there is no failure mode here and
/// `define-sign`'s `err` is reserved for identity-level rejections. The glyph
/// is NOT truncated here: `SignDefinition::glyph_char` does that at paint time,
/// so a future gutter that can afford a wider cell does not need this
/// conversion changed, and `:describe-sign` can still show what the plugin
/// actually declared.
pub fn sign_definition_from_wit(name: String, spec: wit::SignSpec) -> SignDefinition {
    SignDefinition {
        name,
        text: spec.text,
        fallback: spec.fallback,
        theme_element: spec.theme_element,
        priority: spec.priority,
    }
}

/// The `define-sign` host-service body. Registers into the SAME registry native
/// producers use, namespaced by plugin id so unload can reverse it.
///
/// Returns the registered (namespaced) name so the caller can record a teardown
/// token.
///
/// Copy-on-write against the `ArcSwap`: definitions are written at load and
/// read on the render path, so the write clones and stores rather than locking
/// anything a frame might wait on.
pub fn define_plugin_sign(
    registry: &SignRegistryHandle,
    plugin_id: &str,
    name: &str,
    spec: wit::SignSpec,
) -> String {
    let full = format!("{plugin_id}.{name}");
    let mut next: SignRegistry = (**registry.load()).clone();
    next.define(sign_definition_from_wit(full.clone(), spec));
    registry.store(std::sync::Arc::new(next));
    full
}

impl PluginHost {
    /// Instantiate a `sign-plugin` component under its capability grant, drive
    /// its `register-signs` export once, and return the host-issued id plus the
    /// sign names it declared (the teardown tokens).
    ///
    /// Mirror of [`spawn_theme_plugin`](Self::spawn_theme_plugin): the registry
    /// is wired onto `PluginState` BEFORE the export runs so the guest's
    /// imported `define-sign` reaches it.
    pub async fn spawn_sign_plugin(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        budget: PluginBudget,
        registry: &SignRegistryHandle,
    ) -> Result<(crate::PluginId, Vec<String>), PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, tier);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "sign plugin loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(wasi, outcome.grant, budget, Some(&manifest.id))?;
        let bindings = bindings::SignPlugin::instantiate_async(&mut store, component, &self.linker)
            .await
            .map_err(|e| PluginHostError::Instantiate(e.into()))?;

        let id = self.alloc_id();
        store.data_mut().sign_registry = Some(registry.clone());
        store.data_mut().log_ctx = self.log_ctx_for(id);

        arm_store(&mut store, budget)?;
        bindings
            .call_register_signs(&mut store)
            .await
            .map_err(|source| PluginHostError::Trap {
                func: "register-signs",
                kind: classify_trap(&source),
                source: source.into(),
            })?;

        let signs = std::mem::take(&mut store.data_mut().sign_contributions);
        Ok((id, signs))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    fn spec(text: &str, fallback: &str, priority: i32) -> wit::SignSpec {
        wit::SignSpec {
            text: text.to_string(),
            fallback: fallback.to_string(),
            theme_element: "debugger.breakpoint".to_string(),
            priority,
        }
    }

    fn registry() -> SignRegistryHandle {
        std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(SignRegistry::new()))
    }

    #[test]
    fn declaration_namespaces_by_plugin_id() {
        let reg = registry();
        let full = define_plugin_sign(&reg, "debugger", "breakpoint", spec("\u{f111}", "●", 20));
        assert_eq!(full, "debugger.breakpoint");
        assert!(reg.load().id_of("debugger.breakpoint").is_some());
        // Unnamespaced must NOT exist — a plugin cannot squat a bare name or
        // shadow a native producer's sign.
        assert!(reg.load().id_of("breakpoint").is_none());
    }

    #[test]
    fn redefining_keeps_the_id_so_a_reload_does_not_orphan_placements() {
        // A plugin reloading with a new glyph must not strand the placements
        // already in flight. They keep resolving and start painting the new
        // glyph, which is what "redefine" should mean.
        let reg = registry();
        define_plugin_sign(&reg, "debugger", "breakpoint", spec("\u{f111}", "●", 20));
        let before = reg.load().id_of("debugger.breakpoint").unwrap();
        define_plugin_sign(&reg, "debugger", "breakpoint", spec("\u{f192}", "◆", 20));
        let after = reg.load().id_of("debugger.breakpoint").unwrap();
        assert_eq!(before, after, "a redefinition must keep the id");
        assert_eq!(reg.load().get(after).unwrap().fallback, "◆");
    }

    #[test]
    fn a_namespace_is_removed_whole_on_unload() {
        // What teardown reverses. `undefine_prefix` is the reason a plugin's
        // definitions need no individual tracking anywhere else.
        let reg = registry();
        define_plugin_sign(&reg, "debugger", "breakpoint", spec("\u{f111}", "●", 20));
        define_plugin_sign(&reg, "debugger", "current-line", spec("\u{f105}", "▶", 30));
        define_plugin_sign(&reg, "other", "mark", spec("\u{f111}", "◆", 5));
        let mut next: SignRegistry = (**reg.load()).clone();
        next.undefine_prefix("debugger.");
        reg.store(std::sync::Arc::new(next));
        assert!(reg.load().id_of("debugger.breakpoint").is_none());
        assert!(reg.load().id_of("debugger.current-line").is_none());
        assert!(
            reg.load().id_of("other.mark").is_some(),
            "unloading one plugin must not take another's signs with it"
        );
    }

    #[test]
    fn the_spec_crosses_without_losing_the_fallback_palette() {
        // Both palettes have to survive: the theme decides the colour, the
        // font capability decides the glyph, and a crossing that dropped the
        // fallback would render tofu for every user without a patched font.
        let def = sign_definition_from_wit("p.mark".to_string(), spec("\u{f111}", "●", 7));
        assert_eq!(def.name, "p.mark");
        assert_eq!(def.glyph(true), "\u{f111}");
        assert_eq!(def.glyph(false), "●");
        assert_eq!(def.priority, 7);
        assert_eq!(def.theme_element, "debugger.breakpoint");
    }

    /// A wide glyph is not rejected at the boundary — it is truncated at paint
    /// time. The plugin's declaration is preserved so a future wider cell (or
    /// `:describe-sign`) still sees what it actually asked for.
    #[test]
    fn a_wide_glyph_crosses_intact_and_truncates_at_paint_time() {
        let def = sign_definition_from_wit("p.wide".to_string(), spec("ab", "cd", 1));
        assert_eq!(def.text, "ab", "the declaration survives the crossing");
        assert_eq!(def.glyph_char(true), 'a', "but the gutter takes one cell");
        assert_eq!(def.glyph_char(false), 'c');
    }
}
