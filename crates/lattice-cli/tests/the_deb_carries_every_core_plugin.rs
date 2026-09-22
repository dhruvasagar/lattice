//! The `.deb` asset list names every core plugin.
//!
//! `cargo-deb` builds the Linux package from `[package.metadata.deb] assets`
//! in this crate's `Cargo.toml`, and that list spells each plugin's three
//! files out one by one — deliberately, since a glob would silently miss the
//! `.source` marker. Deliberate, and a list: adding a core plugin means
//! remembering a file the plugin's own slice plan never mentioned.
//!
//! `comment` was not remembered. It shipped as the fourth core plugin, was
//! added to `CORE_PLUGINS`, to all four `release.yml` verification loops, to
//! `plugins.toml` and to the user docs — and the v0.9.1 release then failed
//! in the pipeline's own verify step: "lattice-gui-0.9.1-x86_64.deb is
//! missing comment/comment.wasm". The gate worked; nothing local told us
//! first, and a tag had already been pushed.
//!
//! So: the same check, here, before the tag. `CORE_PLUGINS` in `xtask` is the
//! source of truth for which plugins ship, and it is read rather than copied,
//! because a copy is exactly what failed.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::Path;

fn workspace_root() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

/// The plugin names from `xtask`'s `CORE_PLUGINS` literal.
fn core_plugins() -> Vec<String> {
    let src = std::fs::read_to_string(workspace_root().join("xtask/src/main.rs")).unwrap();
    let (_, rest) = src
        .split_once("const CORE_PLUGINS")
        .expect("CORE_PLUGINS exists");
    // `= &[` rather than the first `[`, which belongs to the `&[&str]` type.
    let list = rest
        .split_once("= &[")
        .and_then(|(_, r)| r.split_once(']'))
        .expect("CORE_PLUGINS is a slice literal")
        .0;
    let names: Vec<String> = list
        .split(',')
        .filter_map(|part| {
            let part = part.trim().trim_matches('"');
            (!part.is_empty()).then(|| part.to_owned())
        })
        .collect();
    assert!(
        names.len() >= 4,
        "parsed CORE_PLUGINS as {names:?} — the literal's shape changed"
    );
    names
}

#[test]
fn every_core_plugin_is_staged_into_the_deb() {
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    let mut missing = Vec::new();
    for name in core_plugins() {
        for file in [
            format!("{name}.wasm"),
            "plugin.toml".into(),
            ".source".into(),
        ] {
            let asset = format!("usr/share/lattice/plugins/{name}/{file}");
            if !manifest.contains(&asset) {
                missing.push(asset);
            }
        }
    }
    assert!(
        missing.is_empty(),
        "[package.metadata.deb] assets is missing core-plugin files — the .deb \
         would ship without them, and release.yml's verify step fails the whole \
         release:\n  {}",
        missing.join("\n  ")
    );
}
