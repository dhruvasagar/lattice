//! The context-provider guest world (TC.2).
//!
//! A WASM context provider implements the `context-plugin` world: it exports the
//! `context` producer (`context-scopes`), and imports `tree-sitter` (so the
//! host-owned `tree-snapshot` resource is in scope for the `borrow<>` parameter)
//! plus the `host-services` walk seam. This module holds the **seventh
//! `bindgen!`** (after `plugin`, `picker-source-plugin`,
//! `completion-source-plugin`, `grammar-plugin`, `events-plugin`,
//! `decorations-plugin`) for that world, with the same shared-types trick:
//! `with:` points `types` / `host-services` / `tree-sitter` at the already-
//! generated modules so a crossed `context-scope` is the SAME Rust type
//! `WitBoundary` round-trips (`boundary_context.rs`).
//!
//! **Producer, async** — the `decorations` fork, and here the reason is sharper
//! than "don't block": the guest runs a whole-buffer tree-sitter query inside
//! `context-scopes`, so a synchronous seam would put that on the caller's
//! thread. `exports: { default: async }`.
//!
//! **The `tree-sitter` `with:` points at the GRAMMAR world's module.** That is
//! where `HostTreeSnapshot` / `HostNode` are implemented (`lib.rs`), and those
//! resources are already added to the async linker — so an async world can take
//! a `borrow<tree-snapshot>` without a second host impl. Pointing this world at
//! its own freshly generated `tree-sitter` module instead would mint a second,
//! incompatible set of resource types for the same host objects.

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        world: "context-plugin",
        path: "../../wit",
        // A produce call suspends the guest stack; the whole-buffer query it
        // runs must never pin the caller's thread (nor the render path).
        exports: { default: async },
        with: {
            "lattice:plugin-host/types": crate::lattice::plugin_host::types,
            "lattice:plugin-host/host-services": crate::lattice::plugin_host::host_services,
            "lattice:plugin-host/logging": crate::lattice::plugin_host::logging,
            // The tree resources + their `Host*` impls live with the grammar
            // world (TS.1); reuse them rather than minting a parallel set.
            "lattice:plugin-host/tree-sitter":
                crate::grammar_host::bindings::lattice::plugin_host::tree_sitter,
        },
    });
}
