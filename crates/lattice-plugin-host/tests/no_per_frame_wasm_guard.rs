//! PH7.5 — the no-per-frame-WASM guard (paramount #4, enforced by construction).
//!
//! design.md §7: *"The no-per-frame-WASM rule is absolute: the renderer never
//! calls into a plugin on the UI tick."* The strongest form of that rule is
//! structural — a renderer that cannot **name** the plugin host cannot call it.
//! This test asserts the renderer crates (`lattice-ui-tui`, `lattice-ui-gpui`)
//! do not list `lattice-plugin-host` as a **runtime** dependency.
//!
//! Scope note: this checks DIRECT runtime deps, not the transitive closure. A
//! transitive path renderer → `lattice-host` → `lattice-plugin-host` is
//! *expected* once plugins are boot-wired (PH7.4d stayed validation-only, so
//! there is no such path today) — but even then the renderer still cannot reach
//! a plugin synchronously: it calls `lattice-host`'s render API, and all plugin
//! interaction is host-mediated + off-thread (host-built decoration snapshots,
//! never a synchronous plugin call). The invariant that keeps the tick
//! plugin-free is precisely "the renderer never names the plugin host directly."

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;

/// Runtime dependency table keys of a crate's `Cargo.toml`: `[dependencies]`
/// plus every `[target.'…'.dependencies]`. Dev- and build-dependencies are
/// excluded — they never ship in the renderer binary, so they cannot put a
/// plugin call on the UI tick.
fn runtime_dep_names(manifest: &toml::Value) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(deps) = manifest.get("dependencies").and_then(|d| d.as_table()) {
        names.extend(deps.keys().cloned());
    }
    if let Some(targets) = manifest.get("target").and_then(|t| t.as_table()) {
        for cfg in targets.values() {
            if let Some(deps) = cfg.get("dependencies").and_then(|d| d.as_table()) {
                names.extend(deps.keys().cloned());
            }
        }
    }
    names
}

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = crates/lattice-plugin-host → repo root is ../..
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

#[test]
fn renderers_do_not_directly_depend_on_the_plugin_host() {
    let root = workspace_root();
    for renderer in ["lattice-ui-tui", "lattice-ui-gpui"] {
        let manifest_path = root.join("crates").join(renderer).join("Cargo.toml");
        let text = std::fs::read_to_string(&manifest_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", manifest_path.display()));
        let manifest: toml::Value = toml::from_str(&text)
            .unwrap_or_else(|e| panic!("parse {}: {e}", manifest_path.display()));
        let deps = runtime_dep_names(&manifest);
        assert!(
            !deps.iter().any(|d| d == "lattice-plugin-host"),
            "{renderer} lists `lattice-plugin-host` as a runtime dependency — this violates the \
             no-per-frame-WASM rule (design.md §7). The renderer must not be able to name the \
             plugin host; all plugin interaction is host-mediated + off-thread."
        );
    }
}
