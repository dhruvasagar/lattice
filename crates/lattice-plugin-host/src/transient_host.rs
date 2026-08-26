//! TR.2b — the transient-source guest world.
//!
//! A WASM transient source implements the `transient-source-plugin` world: it
//! exports `id` / `build` and imports `logging` + `project`. This module holds
//! the `bindgen!` for that world, using the same shared-types trick every seam
//! before it does — `with:` points `types` + `logging` at the `plugin` world's
//! generated modules so a crossed value is the SAME Rust type the boundary
//! round-trips, not a fresh incompatible copy.
//!
//! **Async, and on an explicit user action.** `build` is reached by a chord or
//! an ex-command — never per keystroke, never per frame — and it suspends the
//! guest stack rather than pinning the caller's thread. The editor parks on it
//! (`Editor::pending_transient_build`) and seats the menu when it lands.
//!
//! Design: `docs/dev/architecture/plugin-transients.md` §5.

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        world: "transient-source-plugin",
        path: "../../wit",
        // Building a menu suspends the guest rather than pinning the caller's
        // thread; it is never on the keystroke path.
        exports: { default: async },
        with: {
            // Reuse the `plugin` world's generated mirrors so a `transient-spec`
            // crossing here is the same Rust type `WitBoundary` round-trips.
            "lattice:plugin-host/types": crate::lattice::plugin_host::types,
            "lattice:plugin-host/logging": crate::lattice::plugin_host::logging,
        },
    });
}
