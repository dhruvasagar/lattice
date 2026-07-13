//! The decoration-provider guest world (PH7.9b).
//!
//! A WASM decoration provider implements the `decorations-plugin` world: it
//! exports the `decorations` producer (`gutter-decorations`) and imports the
//! `host-services` `walk` seam (PH7.4b). This module holds the **sixth `bindgen!`**
//! (after `plugin`, `picker-source-plugin`, `completion-source-plugin`,
//! `grammar-plugin`, `events-plugin`) for that world — the shared-types trick
//! (`with:` points `types` + `host-services` at the `plugin` world's generated
//! modules so a crossed `gutter-decoration` value is the SAME Rust type
//! `WitBoundary` round-trips, `boundary_decoration.rs`; PH7.3d precedent).
//!
//! **Producer, async (the completion PH7.6 fork).** The native
//! `Mode::gutter_decorations` is read per-frame by the renderer; a WASM mode
//! can't satisfy it inline (per-frame WASM = a paramount-#1 violation). So the
//! guest is an async producer the host calls OFF the render path on a trigger,
//! caching the result; `exports: { default: async }` — a produce call suspends
//! the guest, never the render path. The per-plugin actor that drives it is the
//! `decoration_task` bridge.

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        world: "decorations-plugin",
        path: "../../wit",
        // The guest `gutter-decorations` producer is async — a produce call
        // suspends the guest stack, never pins the caller's thread (nor the
        // render path).
        exports: { default: async },
        with: {
            // Reuse the `plugin` world's generated mirrors so a crossed value is
            // the same Rust type `WitBoundary` round-trips; `host-services` reuses
            // the already-wired `Host` impl (the completion/picker precedent).
            "lattice:plugin-host/types": crate::lattice::plugin_host::types,
            "lattice:plugin-host/host-services": crate::lattice::plugin_host::host_services,
        },
    });
}
