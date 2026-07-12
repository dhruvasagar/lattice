//! The completion-source guest world (PH7.6).
//!
//! A WASM completion source implements the `completion-source-plugin` world: it
//! exports `spec`/`generate` and imports the `host-services` `walk` seam
//! (PH7.4b). This module holds the **third `bindgen!`** (after `plugin` and
//! `picker-source-plugin`) for that world — the two-bindgen-with-shared-types
//! trick (the `with:` map points `types` + `host-services` at the `plugin`
//! world's generated modules so the crossed values are the SAME Rust types the
//! boundary round-trips, PH7.3d precedent).
//!
//! Generator-only by design (option A): a completion source produces candidates
//! asynchronously off the keystroke path, and the host runs the NATIVE
//! `match_and_rank` over them (matching/ranking/annotation stay native — the
//! sync-pipeline + paramount-#1 reason, see `wit/completion-source.wit`). The
//! per-plugin actor task that drives the async `generate` export is the
//! `completion_task` bridge; the adapter is `WasmCompletionSource`.

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        world: "completion-source-plugin",
        path: "../../wit",
        // The guest `generate` export is async — a produce call suspends the
        // guest stack, never pins the caller's thread (nor the keystroke path).
        exports: { default: async },
        with: {
            // Reuse the `plugin` world's generated mirrors so a value crossing
            // here is the same Rust type `WitBoundary` round-trips.
            "lattice:plugin-host/types": crate::lattice::plugin_host::types,
            "lattice:plugin-host/host-services": crate::lattice::plugin_host::host_services,
        },
    });
}
