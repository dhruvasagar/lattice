//! PR.6 project fixture guest.
//!
//! A minimal `wasm32-wasip2` component implementing the base `plugin` lifecycle
//! world. From `activate` it asks the host where its project is — by buffer id
//! and by path, plus the two failure shapes — and reports each answer through
//! `logging.log`, which the host test reads back out of the `PluginTracer`.
//!
//! Logging is the reporting channel rather than a return value because the base
//! `plugin` world's `activate` returns nothing; the point of the fixture is to
//! prove a REAL component can reach the seam and get a real answer, which is
//! not provable from host-side unit tests of the conversion function.

#![allow(clippy::all)]

wit_bindgen::generate!({
    world: "plugin",
    path: "../../../../../wit",
});

use lattice::plugin_host::logging::{self, Level};
use lattice::plugin_host::project::{self, ProjectKind};

/// The host test writes these ids into the message so it can assert on
/// specific calls rather than on ordering.
fn report(tag: &str, info: Option<project::ProjectInfo>) {
    let line = match info {
        Some(i) => {
            let kind = match i.kind {
                ProjectKind::Marker => "marker",
                ProjectKind::Pwd => "pwd",
            };
            format!("{tag}|some|{}|{}|{}", i.root, kind, i.marker)
        }
        // Five fields in both arms so the host-side split is one shape.
        None => format!("{tag}|none|||"),
    };
    logging::log(Level::Info, "project", &line);
}

struct Component;

impl Guest for Component {
    fn activate() {
        // Buffer 1 is the document the host test boots with — a real buffer
        // with a path inside a temporary repository.
        report("buffer-known", project::root_for_buffer(1));
        // An id the host has never issued. `none` here is the untrusted-input
        // case, distinct from "buffer exists but has no path".
        report("buffer-unknown", project::root_for_buffer(9_999_999));
        // By path: the host test passes a path it controls via a second call
        // below, but a guest can also ask about anything it can name.
        report("path-tmp", project::root_for_path("/"));
    }

    fn deactivate() {}
}

export!(Component);
