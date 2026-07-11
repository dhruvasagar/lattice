//! The picker-source guest world (PH7.4c.1a).
//!
//! A WASM picker source implements the `picker-source-plugin` world: it exports
//! `spec`/`init`/`accept` and imports the `host-services` `walk` seam (PH7.4b).
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
        },
    });
}
