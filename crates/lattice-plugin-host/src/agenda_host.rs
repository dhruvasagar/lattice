//! OM.A1 — the agenda-source guest world.
//!
//! A WASM agenda provider implements the `agenda-source-plugin` world: it
//! exports `extensions` / `begin` / `scan` and imports the capability-gated
//! host seams. This module holds the `bindgen!` for that world, using the same
//! shared-types trick as every seam before it — `with:` points `types` +
//! `logging` at the `plugin` world's generated modules so a crossed value is
//! the SAME Rust type the boundary round-trips.
//!
//! **Producer, async.** A scan runs on a spawned task, never the keystroke
//! path, and a `scan` call suspends the guest stack rather than pinning the
//! caller's thread.
//!
//! Unlike `media`, the exports sit at world level rather than inside a named
//! interface. That is `error-parser`'s shape and it is deliberate: the design
//! fragment (`org-mode.md` §6.2) chose it because the two seams have the same
//! job — a stateful per-run parser fed one unit of input at a time — and
//! giving them different shapes would be inventing a second spelling for one
//! idea.

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        world: "agenda-source-plugin",
        path: "../../wit",
        // A scan call suspends the guest rather than pinning the caller's
        // thread — and it is never on the keystroke path.
        exports: { default: async },
        with: {
            // Only `logging` is shared: an `entry` is declared in this seam's
            // own interface rather than in `types`, and the guest touches no
            // filesystem so there is no `host-services` import to reuse.
            "lattice:plugin-host/logging": crate::lattice::plugin_host::logging,
        },
    });
}
