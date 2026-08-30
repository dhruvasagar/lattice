//! MV.1 — the multibuffer-view guest world.
//!
//! Design: `docs/dev/architecture/plugin-multibuffer-views.md`.
//!
//! A view plugin implements `multibuffer-view-plugin`: it exports
//! `register-multibuffer-views` + `build`, and imports the
//! `multibuffer-view-registry` seam it declares through. This module holds the
//! **second `bindgen!`** for that world — the two-bindgen-with-shared-types
//! trick, with the `with:` map pointing `types` / `host-services` / `logging` /
//! `project` at the `plugin` world's generated modules so a value crossing here
//! is the SAME Rust type `WitBoundary` round-trips rather than a fresh,
//! incompatible copy.

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        world: "multibuffer-view-plugin",
        path: "../../wit",
        // `build` is async: producing a view's excerpts may read a store or a
        // file, and it must never pin the caller's thread.
        exports: { default: async },
        with: {
            "lattice:plugin-host/types": crate::lattice::plugin_host::types,
            "lattice:plugin-host/host-services": crate::lattice::plugin_host::host_services,
            "lattice:plugin-host/logging": crate::lattice::plugin_host::logging,
            "lattice:plugin-host/project": crate::lattice::plugin_host::project,
        },
    });
}

use crate::lattice::plugin_host::types::MultibufferViewSpec;

/// The specs a guest declared during `register-multibuffer-views`.
///
/// Recorded on the plugin's `PluginState` and drained by the spawn after the
/// export returns — the `PickerContributions` shape, for its reason: the guest
/// calls a host import N times and the host collects, rather than the component
/// being one view.
#[derive(Debug, Default)]
pub struct MultibufferViewContributions {
    pub specs: Vec<MultibufferViewSpec>,
}

impl MultibufferViewContributions {
    /// Record one declared view.
    ///
    /// A second registration under an id this same plugin already used
    /// **replaces** it — that is a reload, not a collision. Two different
    /// plugins claiming one id is resolved where the provider registry can see
    /// both (`plugin_view.rs`), because only there is the other claimant known.
    pub fn declare(&mut self, spec: MultibufferViewSpec) {
        if let Some(existing) = self.specs.iter_mut().find(|s| s.id == spec.id) {
            *existing = spec;
            return;
        }
        self.specs.push(spec);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lattice::plugin_host::types::MultibufferViewInput;

    fn spec(id: &str, buffer: &str) -> MultibufferViewSpec {
        MultibufferViewSpec {
            id: id.to_string(),
            doc: String::new(),
            buffer_name: buffer.to_string(),
            view_mode: None,
            reuse: true,
            input: MultibufferViewInput::Pull,
        }
    }

    #[test]
    fn a_guest_may_declare_several_views() {
        let mut c = MultibufferViewContributions::default();
        c.declare(spec("a", "*a*"));
        c.declare(spec("b", "*b*"));
        assert_eq!(
            c.specs.len(),
            2,
            "the registry shape, not one-per-component"
        );
    }

    /// A reload re-declares; the second wins rather than accumulating a
    /// duplicate the provider registry would then see twice.
    #[test]
    fn re_declaring_an_id_replaces_rather_than_appends() {
        let mut c = MultibufferViewContributions::default();
        c.declare(spec("a", "*old*"));
        c.declare(spec("a", "*new*"));
        assert_eq!(c.specs.len(), 1);
        assert_eq!(c.specs[0].buffer_name, "*new*");
    }
}
