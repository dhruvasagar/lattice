//! The picker-source guest world (PH7.4c.1a).
//!
//! A WASM picker source implements the `picker-source-plugin` world: it exports
//! `register-picker-sources`/`init`/`accept` and imports the `host-services`
//! `walk` seam (PH7.4b) plus the `picker-registry` seam it declares through.
//! This module holds the **second `bindgen!`** for that world — the
//! two-bindgen-with-shared-types trick (the `with:` map points `types` +
//! `host-services` at the `plugin` world's generated modules so the crossed
//! values are the SAME Rust types the boundary round-trips, PH7.3d precedent).
//!
//! The per-plugin actor task that *drives* these async exports lands at
//! PH7.4c.1b; the `Arc<dyn PickerSourceGenerator>` adapter + registration at
//! PH7.4c.2. This slice lands the world + its generated bindings.
//!
//! **Deferred — the `document` handle.** The active buffer's bulk text should
//! ride a `borrow<document>` handle in `init` (PH7.3c `DocumentResource`, the
//! §4.2 read-back model). Passing a *host-owned* resource into a guest **export**
//! has a bindgen-modeling subtlety (a resource referenced only by an exported
//! signature is not seen as a host `with`-mapped import), so it is carved into a
//! focused follow-up. It does not block the ⭐ exit: the `fuzzy-finder`/`files`
//! source reads no buffer text (it walks the fs via `host-services`); only a
//! text-reading source (`:picker lines`) needs the handle.

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        world: "picker-source-plugin",
        path: "../../wit",
        // The guest exports (`init`/`accept`/`spec`) are async — a picker source
        // call suspends the guest stack, never pins the caller's thread.
        exports: { default: async },
        with: {
            // Reuse the `plugin` world's generated mirrors so a value crossing
            // here is the same Rust type `WitBoundary` round-trips (not a fresh,
            // incompatible copy).
            "lattice:plugin-host/types": crate::lattice::plugin_host::types,
            "lattice:plugin-host/host-services": crate::lattice::plugin_host::host_services,
            "lattice:plugin-host/logging": crate::lattice::plugin_host::logging,
        },
    });
}

/// OR.5b: the specs a guest declared during `register-picker-sources`.
///
/// Recorded on the plugin's `PluginState` and drained by the actor spawn after
/// the export returns — the `GrammarContributions` / `ModeContributions` shape,
/// and for the same reason: a guest registers by *calling*, so the host needs
/// somewhere for those calls to land before it knows how many there were.
#[derive(Debug, Default)]
pub struct PickerContributions {
    specs: Vec<lattice_picker::source::PickerSourceSpec>,
}

impl PickerContributions {
    /// Record one declared source.
    ///
    /// A repeat of an id this plugin already declared REPLACES it rather than
    /// appending: a guest that registers twice under one name means the second
    /// one, and appending would register two generators the registry then
    /// resolves by insertion order — which is a coin flip dressed as a rule.
    pub fn push(&mut self, spec: lattice_picker::source::PickerSourceSpec) {
        if let Some(existing) = self.specs.iter_mut().find(|s| s.id == spec.id) {
            *existing = spec;
            return;
        }
        self.specs.push(spec);
    }

    /// Take everything declared, leaving the store empty.
    pub fn take(&mut self) -> Vec<lattice_picker::source::PickerSourceSpec> {
        std::mem::take(&mut self.specs)
    }

    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use lattice_picker::source::PickerSourceSpec;

    #[test]
    fn declaring_several_sources_keeps_them_all() {
        let mut c = PickerContributions::default();
        c.push(PickerSourceSpec::no_args("a", "first"));
        c.push(PickerSourceSpec::no_args("b", "second"));
        let specs = c.take();
        assert_eq!(specs.len(), 2, "the whole point of the slice");
        assert!(c.is_empty(), "take leaves the store empty");
    }

    /// A guest that registers twice under one id means the second one. Appending
    /// would register two generators the registry resolves by insertion order,
    /// which is a coin flip dressed as a rule.
    #[test]
    fn re_declaring_an_id_replaces_rather_than_appends() {
        let mut c = PickerContributions::default();
        c.push(PickerSourceSpec::no_args("a", "first"));
        c.push(PickerSourceSpec::no_args("a", "second"));
        let specs = c.take();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].doc, "second");
    }
}
