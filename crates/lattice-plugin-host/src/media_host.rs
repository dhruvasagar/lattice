//! IM.6b — the inline-media provider guest world.
//!
//! A WASM media provider implements the `media-plugin` world: it exports the
//! `media` producer (`media-blocks`) and imports the capability-gated
//! `host-services` a scan might need. This module holds the `bindgen!` for
//! that world, using the same shared-types trick as every seam before it —
//! `with:` points `types` + `host-services` at the `plugin` world's generated
//! modules so a crossed `media-block` is the SAME Rust type the boundary
//! round-trips.
//!
//! **Producer, async.** The guest is called OFF the render path on a trigger
//! and its result cached; a guest on the render path would be per-frame WASM,
//! a paramount-#1 violation.

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        world: "media-plugin",
        path: "../../wit",
        // A produce call suspends the guest stack rather than pinning the
        // caller's thread — and never the render path.
        exports: { default: async },
        with: {
            "lattice:plugin-host/types": crate::lattice::plugin_host::types,
            "lattice:plugin-host/host-services": crate::lattice::plugin_host::host_services,
            "lattice:plugin-host/logging": crate::lattice::plugin_host::logging,
        },
    });
}
