//! `xtask` — workspace automation (the cargo-xtask pattern).
//!
//! `cargo xtask build-core-plugins` (PM.2) builds the plugins that ship WITH
//! lattice — the *core* set — to `wasm32-wasip2` components and stages them into
//! the dev **runtime root** `<workspace>/runtime/plugins/<name>/`, where the PM.1
//! search path (`<exe>/../../runtime/plugins` for a `target/<profile>/lattice`
//! binary) discovers them. So after one `cargo xtask build-core-plugins`, a plain
//! `cargo run` finds the core plugins with no hand-copy — the dev equivalent of
//! the release/packaging step that stages the same artifacts into the shipped
//! runtime root (plugin-manager.md §7).

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The plugins that ship with lattice. Each is a standalone `wasm32-wasip2` cargo
/// project under `plugins/<name>/` (NOT a workspace member — it builds in a clean
/// env, the `lattice-plugin-host` `build.rs` precedent).
const CORE_PLUGINS: &[&str] = &["auto-pair"];

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("build-core-plugins") => match build_core_plugins() {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("xtask: {err}");
                ExitCode::FAILURE
            }
        },
        other => {
            eprintln!("usage: cargo xtask build-core-plugins");
            if let Some(cmd) = other {
                eprintln!("unknown command: {cmd}");
            }
            ExitCode::FAILURE
        }
    }
}

/// The workspace root — the `xtask` crate lives at `<workspace>/xtask`.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the xtask crate has a parent (the workspace root)")
        .to_path_buf()
}

fn build_core_plugins() -> Result<(), String> {
    let ws = workspace_root();
    let runtime_plugins = ws.join("runtime").join("plugins");

    for &name in CORE_PLUGINS {
        let plugin_dir = ws.join("plugins").join(name);
        if !plugin_dir.join("Cargo.toml").exists() {
            return Err(format!("no plugin crate at {}", plugin_dir.display()));
        }

        println!("• building core plugin `{name}` (wasm32-wasip2, release)…");
        build_one(&plugin_dir)?;

        // The `wasm32-wasip2` target emits a component directly (no separate
        // convert step). The crate name underscores the plugin id.
        let artifact = plugin_dir
            .join("target")
            .join("wasm32-wasip2")
            .join("release")
            .join(format!("{}.wasm", name.replace('-', "_")));
        if !artifact.exists() {
            return Err(format!(
                "build produced no artifact at {}",
                artifact.display()
            ));
        }

        // Stage the component + its manifest into the runtime root (PM.1 layout:
        // one dir per plugin, a `plugin.toml` + the sole `.wasm`).
        let dest = runtime_plugins.join(name);
        std::fs::create_dir_all(&dest).map_err(|e| format!("mkdir {}: {e}", dest.display()))?;
        std::fs::copy(&artifact, dest.join(format!("{name}.wasm")))
            .map_err(|e| format!("stage component: {e}"))?;
        std::fs::copy(plugin_dir.join("plugin.toml"), dest.join("plugin.toml"))
            .map_err(|e| format!("stage plugin.toml: {e}"))?;
        println!("  staged → {}", dest.display());
    }

    println!(
        "done: {} core plugin(s) staged into {}",
        CORE_PLUGINS.len(),
        runtime_plugins.display()
    );
    Ok(())
}

/// Build one standalone plugin crate to a `wasm32-wasip2` component, in a **clean
/// environment** — inherited workspace `RUSTFLAGS` / target / rustc wrappers break
/// the wasm build (the `lattice-plugin-host` `build.rs` `build_guest` precedent).
fn build_one(plugin_dir: &Path) -> Result<(), String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let status = Command::new(&cargo)
        .current_dir(plugin_dir)
        .args(["build", "--release", "--target", "wasm32-wasip2"])
        // Pin the target dir so a leaked `CARGO_TARGET_DIR` can't redirect the
        // output out from under the path we stage from.
        .arg("--target-dir")
        .arg(plugin_dir.join("target"))
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_BUILD_RUSTFLAGS")
        .env_remove("CARGO_BUILD_TARGET")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("RUSTC")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .status()
        .map_err(|e| format!("failed to run cargo: {e}"))?;
    if !status.success() {
        return Err(format!(
            "plugin build failed ({status}). Is the target installed? \
             `rustup target add wasm32-wasip2`"
        ));
    }
    Ok(())
}
