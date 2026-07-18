//! PO.5 logging fixture guest (plugin observability Layer 2).
//!
//! A minimal `wasm32-wasip2` component implementing the base `plugin` lifecycle
//! world. From its `activate` export it emits its own narrative through the
//! imported `logging.log` at several levels + contexts; the host routes each into
//! the `PluginTracer` as a `seam = logging`, `direction = host-import` record. The
//! host test (`tests/logging_source.rs`) instantiates this with a tracer wired and
//! asserts the lines land — proving the guest-narrative seam end to end,
//! language-agnostically (this happens to be Rust, but the host sees only the WIT
//! boundary).

#![allow(clippy::all)]

wit_bindgen::generate!({
    world: "plugin",
    path: "../../../../../wit",
});

use lattice::plugin_host::logging::{self, Level};

struct Component;

impl Guest for Component {
    fn activate() {
        // Distinct levels + contexts so the host test can assert routing, level
        // mapping, and the context→category rendering. `info`/`warn` are kept at
        // the default gate; `debug`/`trace` only when the plugin is raised.
        logging::log(Level::Info, "boot", "logging guest activated");
        logging::log(Level::Warn, "index", "reindex found 2 stale entries");
        logging::log(Level::Debug, "detail", "walked 40 files in 3ms");
        logging::log(Level::Error, "", "a context-less error line");
    }

    fn deactivate() {
        logging::log(Level::Info, "boot", "logging guest deactivated");
    }
}

export!(Component);
