//! AT.5 — the `llm-tools` guest world.
//!
//! A plugin that offers tools to the LLM assistant implements the
//! `llm-tools-plugin` world: it exports `tools` / `call` / `profiles` and
//! imports `host-services`, `logging` and `project`. This module holds the
//! `bindgen!` for that world, with `with:` pointing the shared interfaces at
//! the `plugin` world's generated modules so a crossed value is the same Rust
//! type every other seam uses.
//!
//! **Async, and never on the keystroke path.** `call` is reached when a model
//! asks for the tool, from the assistant's request task; it suspends the guest
//! rather than pinning a thread.
//!
//! Design: `docs/dev/architecture/llm-assistant-tools.md` §5.

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        world: "llm-tools-plugin",
        path: "../lattice-wit/wit",
        exports: { default: async },
        with: {
            "lattice:plugin-host/types": crate::lattice::plugin_host::types,
            "lattice:plugin-host/host-services": crate::lattice::plugin_host::host_services,
            "lattice:plugin-host/logging": crate::lattice::plugin_host::logging,
        },
    });
}
