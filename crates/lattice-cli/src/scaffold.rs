//! `lattice --init` — scaffold a starter `init.rs` config into
//! `~/.config/lattice/init/`.
//!
//! Writes a **self-contained, buildable** WASM-component config crate: the
//! `Cargo.toml`, the `plugin.toml` the editor discovers, a minimal `src/lib.rs`
//! that sets an option + a keybinding + an event handler, and a `wit/` copy of
//! the editor's own API package (embedded at build time — [`crate::WIT_FILES`]).
//! The user edits `src/lib.rs`, builds to `wasm32-wasip2`, drops the artifact in
//! as `init.wasm`, and reloads. Refuses to clobber an existing non-empty config.

use std::path::Path;

use anyhow::{bail, Context, Result};

const CARGO_TOML: &str = r#"# Your lattice config, compiled to a wasm32-wasip2 component.
# A standalone [workspace] so it doesn't inherit an outer cargo toolchain.
[package]
name = "lattice-init"
version = "0.0.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]      # a component, not an rlib

[dependencies]
wit-bindgen = "0.58"

[profile.release]
opt-level = "s"
strip = true

[workspace]
"#;

const PLUGIN_TOML: &str = r#"# The init config manifest. `id` MUST be "init" (the :reload-config target).
# `provides` lists the seams your init.rs registers into — add "grammar" /
# "decorations" / … as you use them, matching the exports in wit/init.wit.
id = "init"
provides = ["config", "keymap", "events"]
"#;

const INIT_WIT: &str = r#"package lattice:plugin-host@0.1.0;

// Your init.rs world — the seams you use. EXPORT the register-* your guest
// implements; IMPORT the host APIs you call. Add/remove seams as needed. Browse
// exact signatures in a running editor with `:describe-plugin-api <seam>`.
// (The comment sits BELOW `package` — only one file per package may doc it.)
world init {
    import config;
    import keymap;
    import events;
    import modes;

    export register-options: func();
    export register-keymap: func();
    export register-events: func();
    export on-event: func(handler: u32, ev: event);

    use types.{event};
}
"#;

const LIB_RS: &str = r####"//! Your lattice config, compiled to a WASM component and loaded at boot.
//! Edit freely, then rebuild + `:reload-config` (see this dir's build steps, or
//! `docs/user/init.md`). Everything below is illustrative — delete what you
//! don't want.

wit_bindgen::generate!({ world: "init", path: "wit" });

// `Event` is re-exported at the crate root by the world's `use types.{event}`
// (refer to it unqualified); other types come from their interface module.
use lattice::plugin_host::keymap::BindingMode;
use lattice::plugin_host::types::{EventFilter, EventKind};
use lattice::plugin_host::{config, events, keymap, modes};

struct Component;

impl Guest for Component {
    // IMMEDIATE — option overrides (also settable in lattice.toml or via `:set`).
    fn register_options() {
        config::set_option("tabstop", "4");

        // auto-pair is a CORE plugin, ON by default. Configure it here:
        // config::set_option("auto-pairs-style", "manual"); // manual close-key pairing
        // config::set_option("auto-pair.enabled", "false"); // …or turn it off
    }

    // IMMEDIATE — keybindings layered above the builtin vim grammar.
    fn register_keymap() {
        // <C-s> in Normal → an existing command (binds only if the command exists).
        keymap::register_binding(BindingMode::Normal, "<C-s>", "ex:write");
    }

    // Subscribe deferred / event-flow hooks (handler ids are yours to choose).
    fn register_events() {
        events::subscribe(
            &EventFilter {
                kinds: Some(vec![EventKind::PluginLoaded]),
                path_globs: None,
                major_modes: None,
            },
            1,
        );
    }

    // React to events — e.g. configure a USER plugin the moment it loads.
    // (Core plugins like auto-pair are on by default — configure them in
    // `register_options` above, not here.)
    fn on_event(handler: u32, ev: Event) {
        if let (1, Event::PluginLoaded(p)) = (handler, ev) {
            if p.name == "my-plugin" {
                modes::enable_mode("my-plugin-mode");
            }
        }
    }
}

export!(Component);
"####;

/// Scaffold `~/.config/lattice/init/`. Returns an error (never a panic) on a
/// missing config home or a non-empty existing config.
pub fn generate_init() -> Result<()> {
    let dir = lattice_config::config_home()
        .context("no config home directory on this platform (set $XDG_CONFIG_HOME)")?
        .join("lattice")
        .join("init");
    write_scaffold(&dir)?;

    let d = dir.display();
    println!("Created a starter lattice config at {d}\n");
    println!("Next — build it and drop the component in place:\n");
    println!("  rustup target add wasm32-wasip2   # once");
    println!("  cd {d}");
    println!("  cargo build --release --target wasm32-wasip2");
    println!("  cp target/wasm32-wasip2/release/lattice_init.wasm init.wasm\n");
    println!("Then start lattice — or `:reload-config` in a running editor.");
    println!("Edit src/lib.rs to customise; see docs/user/init.md for the seams.");
    Ok(())
}

/// Write the scaffold into `dir` (never clobbering a non-empty existing one) —
/// the pure filesystem half of [`generate_init`], testable against any dir.
fn write_scaffold(dir: &Path) -> Result<()> {
    if dir.exists() && std::fs::read_dir(dir)?.next().is_some() {
        bail!(
            "{} already exists and is not empty — edit it in place, or remove it first",
            dir.display()
        );
    }
    std::fs::create_dir_all(dir.join("src")).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::create_dir_all(dir.join("wit"))?;
    // The editor's own API package + the user's init world.
    for (name, content) in crate::WIT_FILES {
        std::fs::write(dir.join("wit").join(name), content)
            .with_context(|| format!("writing wit/{name}"))?;
    }
    write(&dir.join("wit").join("init.wit"), INIT_WIT)?;
    write(&dir.join("Cargo.toml"), CARGO_TOML)?;
    write(&dir.join("plugin.toml"), PLUGIN_TOML)?;
    write(&dir.join("src").join("lib.rs"), LIB_RS)?;
    Ok(())
}

fn write(path: &Path, content: &str) -> Result<()> {
    std::fs::write(path, content).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffolds_a_complete_buildable_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("init");
        write_scaffold(&dir).unwrap();

        // The four hand-written files + a non-empty embedded wit package.
        assert!(dir.join("Cargo.toml").exists());
        assert!(dir.join("plugin.toml").exists());
        assert!(dir.join("src/lib.rs").exists());
        assert!(dir.join("wit/init.wit").exists());
        assert!(dir.join("wit/types.wit").exists(), "the API package is copied");
        assert!(!crate::WIT_FILES.is_empty(), "wit package embedded at build");

        // The manifest the editor discovers declares the init id.
        let manifest = std::fs::read_to_string(dir.join("plugin.toml")).unwrap();
        assert!(manifest.contains("id = \"init\""));

        // Only one package may carry a doc comment: `init.wit` must NOT lead with
        // one before `package` (the bug that broke the first build).
        let init_wit = std::fs::read_to_string(dir.join("wit/init.wit")).unwrap();
        assert!(
            init_wit.trim_start().starts_with("package "),
            "init.wit starts with `package`, no leading comment"
        );
    }

    #[test]
    fn refuses_to_clobber_a_non_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("init");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("keep.me"), "existing config").unwrap();
        assert!(write_scaffold(&dir).is_err(), "won't overwrite existing config");
    }
}
