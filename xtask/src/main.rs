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
const CORE_PLUGINS: &[&str] = &[
    "auto-pair",
    "treesitter-context",
    "project",
    "comment",
    "lighthouse",
];

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let result = match args.next().as_deref() {
        Some("build-core-plugins") => build_core_plugins(),
        Some("bump-plugin-api") => match args.next() {
            Some(version) => bump_plugin_api(&version),
            None => {
                Err("bump-plugin-api needs a version: cargo xtask bump-plugin-api 0.2.0".into())
            }
        },
        other => {
            eprintln!(
                "usage:\n  \
                 cargo xtask build-core-plugins\n  \
                 cargo xtask bump-plugin-api <X.Y.Z>"
            );
            if let Some(cmd) = other {
                eprintln!("unknown command: {cmd}");
            }
            return ExitCode::FAILURE;
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("xtask: {err}");
            ExitCode::FAILURE
        }
    }
}

/// The three crates published under one plugin-API version.
const PUBLISHED_CRATES: &[&str] = &[
    "lattice-wit",
    "lattice-plugin-sdk",
    "lattice-plugin-sdk-derive",
];

/// `cargo xtask bump-plugin-api <X.Y.Z>` — move the plugin ABI to a new version.
///
/// The ABI version is stated in 37 places: `package lattice:plugin-host@X.Y.Z;`
/// at the top of every `.wit` file, and the `version` of each published crate.
/// Editing those by hand is how one file gets left behind, and a single
/// disagreeing file produces a package that fails to parse or links half its
/// interfaces — so the bump is one command and
/// `the_crate_versions_track_the_wit_package_version` is the check that it
/// landed everywhere.
///
/// This does NOT decide whether a bump is warranted. Changing the package
/// version breaks every existing plugin at instantiation, because the Component
/// Model puts it in the imported interface names; an additive seam usually does
/// not need one. See `docs/dev/guides/plugin-authoring.md`.
fn bump_plugin_api(version: &str) -> Result<(), String> {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.parse::<u32>().is_err()) {
        return Err(format!("'{version}' is not an X.Y.Z version"));
    }
    let root = workspace_root();

    let wit_dir = root.join("crates/lattice-wit/wit");
    let mut wit_touched = 0usize;
    let entries =
        std::fs::read_dir(&wit_dir).map_err(|e| format!("reading {}: {e}", wit_dir.display()))?;
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().is_none_or(|e| e != "wit") {
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        let mut out = String::with_capacity(text.len());
        let mut replaced = false;
        for line in text.lines() {
            if !replaced
                && line
                    .trim_start()
                    .starts_with("package lattice:plugin-host@")
            {
                out.push_str(&format!("package lattice:plugin-host@{version};"));
                replaced = true;
            } else {
                out.push_str(line);
            }
            out.push('\n');
        }
        if !replaced {
            return Err(format!(
                "{} declares no `package lattice:plugin-host@...;`",
                path.display()
            ));
        }
        std::fs::write(&path, out).map_err(|e| format!("writing {}: {e}", path.display()))?;
        wit_touched += 1;
    }

    for name in PUBLISHED_CRATES {
        let path = root.join("crates").join(name).join("Cargo.toml");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        let mut out = String::with_capacity(text.len());
        let mut replaced = false;
        for line in text.lines() {
            // The package's own `version`, not a dependency's: the first bare
            // `version = "..."` at the start of a line. A dependency carries it
            // inline inside `{ ... }`, so it is never at column zero.
            if !replaced && line.starts_with("version = \"") {
                out.push_str(&format!("version = \"{version}\""));
                replaced = true;
            } else {
                out.push_str(line);
            }
            out.push('\n');
        }
        if !replaced {
            return Err(format!(
                "{} has no literal `version = \"X.Y.Z\"` — is it still \
                 version.workspace = true?",
                path.display()
            ));
        }
        std::fs::write(&path, out).map_err(|e| format!("writing {}: {e}", path.display()))?;
    }

    // WIT that lives INSIDE Rust source: the scaffold's templates (what
    // `lattice plugin new` writes) and a fixture's inline world. Neither is a
    // `.wit` file, so the loop above cannot see them, and the first real bump
    // (0.1 -> 0.2) left both behind: scaffolded plugins would have declared a
    // generation the editor no longer implements.
    let mut inline_touched = 0usize;
    for rel in INLINE_WIT {
        let path = root.join(rel);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        let (out, count) = rewrite_inline_wit_versions(&text, version);
        if count == 0 {
            return Err(format!(
                "{} names no `lattice:plugin-host...@X.Y.Z` — did the inline WIT move? \
                 Update INLINE_WIT in xtask.",
                path.display()
            ));
        }
        std::fs::write(&path, out).map_err(|e| format!("writing {}: {e}", path.display()))?;
        inline_touched += count;
    }

    // The SDK's dependency on the derive crate carries the version too, and a
    // stale one there fails `cargo publish`, not the build -- late, and after
    // the ABI files are already committed.
    let sdk = root.join("crates/lattice-plugin-sdk/Cargo.toml");
    let text = std::fs::read_to_string(&sdk).map_err(|e| e.to_string())?;
    let updated = text
        .lines()
        .map(|line| {
            if line.starts_with("lattice-plugin-sdk-derive") {
                let (before, _) = line.split_once(", version = \"").unwrap_or((line, ""));
                format!("{before}, version = \"{version}\" }}")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&sdk, format!("{updated}\n")).map_err(|e| e.to_string())?;

    // Cargo.lock records these three by version, so leaving it behind makes the
    // lock and the manifests disagree -- which shows up as a diff on the next
    // unrelated build rather than here, where it was caused.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let status = Command::new(&cargo)
        .args(["update", "--workspace", "--quiet"])
        .current_dir(&root)
        .status()
        .map_err(|e| format!("cargo update --workspace: {e}"))?;
    if !status.success() {
        return Err(
            "cargo update --workspace failed; Cargo.lock still names the old version".into(),
        );
    }

    println!("plugin API -> {version}");
    println!("  {wit_touched} .wit package declarations");
    println!("  {inline_touched} inline WIT references in Rust source");
    println!(
        "  {} crate versions, and Cargo.lock",
        PUBLISHED_CRATES.len()
    );
    println!();
    println!("Next: cargo test -p lattice-wit   (the guard proves it landed everywhere)");
    println!("      every existing plugin must rebuild; a pinned one will not instantiate.");
    Ok(())
}

/// Rust sources that embed WIT naming the plugin-host package by version.
const INLINE_WIT: &[&str] = &[
    "crates/lattice-cli/src/scaffold.rs",
    "crates/lattice-plugin-host/tests/fixtures/language-guest/src/lib.rs",
];

/// Rewrite every `lattice:plugin-host@X.Y.Z` and
/// `lattice:plugin-host/<name>@X.Y.Z` in `text` to `version`; returns the new
/// text and how many were rewritten.
fn rewrite_inline_wit_versions(text: &str, version: &str) -> (String, usize) {
    const PACKAGE: &str = "lattice:plugin-host";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut count = 0;
    while let Some(at) = rest.find(PACKAGE) {
        let (before, tail) = rest.split_at(at + PACKAGE.len());
        out.push_str(before);
        // An optional `/<interface-or-world>` path, then `@`.
        let path_len = tail.strip_prefix('/').map_or(0, |t| {
            1 + t
                .find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'))
                .unwrap_or(t.len())
        });
        let (path, after_path) = tail.split_at(path_len);
        out.push_str(path);
        if let Some(old) = after_path.strip_prefix('@') {
            let ver_len = old
                .find(|c: char| !(c.is_ascii_digit() || c == '.'))
                .unwrap_or(old.len());
            out.push('@');
            out.push_str(version);
            rest = &old[ver_len..];
            count += 1;
        } else {
            rest = after_path;
        }
    }
    out.push_str(rest);
    (out, count)
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
        // PM.8a: mark it bundled. The `:plugins` SOURCE column reads this
        // marker like it reads a required plugin's, so a core plugin says
        // `bundled` rather than `—`; and `is_buildable() == false` is what
        // stops the rebuild chord offering to build something the editor
        // ships prebuilt and cannot rebuild from here.
        std::fs::write(dest.join(".source"), "kind = bundled\n")
            .map_err(|e| format!("stage source marker: {e}"))?;
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
