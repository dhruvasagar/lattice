//! Builds the fixture guests to `wasm32-wasip2` components so the tests/benches
//! can load them (real guest↔host canonical-ABI calls):
//!   - `trampoline-guest` (PH7.3d) → `TRAMPOLINE_GUEST_WASM`, used by
//!     `benches/trampoline.rs` (the typed-call bench).
//!   - `picker-guest` (PH7.4c.1b) → `PICKER_GUEST_WASM`, used by
//!     `tests/picker_actor.rs` (drives the per-plugin actor bridge).
//!
//! Each guest is a *standalone workspace* under `tests/fixtures/<name>` with its
//! own `target/`, so building it here never contends with the host's build lock.
//! On success we hand the consumer the component path via the named env var. If
//! the `wasm32-wasip2` target is missing or a guest build fails, we set that
//! var *empty* and warn — the host build still succeeds and the dependent
//! test/bench skips (with a clear message) rather than breaking every
//! `cargo build` on a box without the wasm target.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"),
    );
    let wit_dir = manifest_dir.join("..").join("..").join("wit");
    // A change to the shared WIT (the worlds the guests target) rebuilds both.
    println!("cargo:rerun-if-changed={}", wit_dir.display());

    // Test fixtures live under `tests/fixtures/`; bundled plugins live at the
    // repo-root `plugins/`. All build the same way. Crate name uses a dash,
    // artifact uses an underscore (`trampoline-guest` → `trampoline_guest.wasm`).
    let fixtures = manifest_dir.join("tests").join("fixtures");
    build_guest(
        &fixtures.join("trampoline-guest"),
        "trampoline-guest",
        "TRAMPOLINE_GUEST_WASM",
    );
    build_guest(
        &fixtures.join("picker-guest"),
        "picker-guest",
        "PICKER_GUEST_WASM",
    );
    build_guest(
        &fixtures.join("completion-guest"),
        "completion-guest",
        "COMPLETION_GUEST_WASM",
    );
    build_guest(
        &fixtures.join("grammar-guest"),
        "grammar-guest",
        "GRAMMAR_GUEST_WASM",
    );
    build_guest(
        &fixtures.join("events-guest"),
        "events-guest",
        "EVENTS_GUEST_WASM",
    );
    build_guest(
        &fixtures.join("decorations-guest"),
        "decorations-guest",
        "DECORATIONS_GUEST_WASM",
    );
    // TC.2: the sticky-context producer fixture. Walks the handed
    // `tree-snapshot`, so it is what proves a `borrow<>` survives an async
    // guest suspension (the repo's first).
    build_guest(
        &fixtures.join("context-guest"),
        "context-guest",
        "CONTEXT_GUEST_WASM",
    );
    // TC.4: the theme element-registration fixture.
    build_guest(
        &fixtures.join("theme-guest"),
        "theme-guest",
        "THEME_GUEST_WASM",
    );
    build_guest(
        &fixtures.join("config-guest"),
        "config-guest",
        "CONFIG_GUEST_WASM",
    );
    build_guest(
        &fixtures.join("modes-guest"),
        "modes-guest",
        "MODES_GUEST_WASM",
    );
    build_guest(
        &fixtures.join("keymap-guest"),
        "keymap-guest",
        "KEYMAP_GUEST_WASM",
    );
    build_guest(
        &fixtures.join("emacs-keys-guest"),
        "emacs-keys-guest",
        "EMACS_KEYS_GUEST_WASM",
    );
    // AP.1 spike: a single component providing grammar + modes + config, loaded
    // once per seam by `tests/multiseam.rs` to prove multi-seam plugins work.
    build_guest(
        &fixtures.join("multiseam-guest"),
        "multiseam-guest",
        "MULTISEAM_GUEST_WASM",
    );
    // CI.5: the init.rs-shape fixture — subscribes to plugin-loaded and calls
    // enable-mode from its handler (the with-eval-after-load pattern).
    build_guest(
        &fixtures.join("init-guest"),
        "init-guest",
        "INIT_GUEST_WASM",
    );
    // PO.5: the `logging` (Layer 2) fixture — a base `plugin`-world guest whose
    // `activate` calls the imported `logging.log`; `tests/logging_source.rs`
    // asserts the lines reach the tracer.
    build_guest(
        &fixtures.join("logging-guest"),
        "logging-guest",
        "LOGGING_GUEST_WASM",
    );
    // AP.1: the first bundled plugin — a multi-seam component (grammar + modes +
    // config). Built here so the loader integration test (loaded by known path,
    // like the mode/config drains) and the eventual bundling (AP.4) have the
    // `.wasm`.
    build_guest(
        &manifest_dir
            .join("..")
            .join("..")
            .join("plugins")
            .join("auto-pair"),
        "auto-pair",
        "AUTO_PAIR_WASM",
    );
    // TC.5: the second bundled plugin — sticky scope headers. Multi-seam
    // (context + config + theme) from one component, the `auto-pair` shape.
    build_guest(
        &manifest_dir
            .join("..")
            .join("..")
            .join("plugins")
            .join("treesitter-context"),
        "treesitter-context",
        "TREESITTER_CONTEXT_WASM",
    );
}

/// Build one standalone `wasm32-wasip2` guest crate at `guest_dir` to a
/// component and export its path via `env_var` (empty + a `warning` on any
/// failure, so the dependent test/bench skips gracefully). `name` is the crate
/// name; the artifact is `<name-with-underscores>.wasm`.
fn build_guest(guest_dir: &Path, name: &str, env_var: &str) {
    // A guest crate that isn't in the tree must be skipped BEFORE any
    // `rerun-if-changed` is emitted. Kept as a guard even though no caller
    // currently points at a missing directory: `fuzzy-finder` was removed
    // after `auto-pair` became the first bundled plugin, and its stale entry
    // here cost every build a full relink until it was found.
    //
    // This is not a tidiness point, it is the build's single biggest cost.
    // Cargo re-runs a build script on every invocation when any declared
    // `rerun-if-changed` path does not exist — it cannot prove the input is
    // unchanged, so it assumes it changed. Emitting those lines for an absent
    // directory therefore dirtied `lattice-plugin-host` on EVERY build, which
    // relinked `lattice-host` and each of its 25 integration-test binaries.
    // A fully-cached `cargo test -p lattice-host` cost ~445s almost entirely
    // in that relink; the tests themselves run in seconds.
    //
    // Skipping also emits the empty env var, so the dependent test still
    // compiles and skips gracefully — same contract as a failed build below.
    if !guest_dir.join("Cargo.toml").exists() {
        println!("cargo:rustc-env={env_var}=");
        return;
    }

    // Rebuild the guest whenever its source or manifest changes (the shared
    // WIT rerun is registered once in `main`).
    println!("cargo:rerun-if-changed={}", guest_dir.join("src").display());
    println!(
        "cargo:rerun-if-changed={}",
        guest_dir.join("Cargo.toml").display()
    );

    // The guest builds into its own workspace `target/`, pinned explicitly so a
    // leaked `CARGO_TARGET_DIR` can't redirect the output out from under the
    // path we check below.
    let target_dir = guest_dir.join("target");
    let artifact = format!("{}.wasm", name.replace('-', "_"));
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join(&artifact);

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(&cargo)
        .current_dir(guest_dir)
        .args(["build", "--release", "--target", "wasm32-wasip2"])
        .arg("--target-dir")
        .arg(&target_dir)
        // The build script's env carries the HOST compilation's flags/target
        // (workspace lints via `CARGO_ENCODED_RUSTFLAGS`, `CARGO_BUILD_TARGET`,
        // a `RUSTC` wrapper, …). Inherited into this nested cargo they are
        // applied to the wasm build and break it — the guest must build in a
        // clean environment.
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_BUILD_RUSTFLAGS")
        .env_remove("CARGO_BUILD_TARGET")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("RUSTC")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .output();

    match output {
        Ok(o) if o.status.success() && wasm.exists() => {
            println!("cargo:rustc-env={env_var}={}", wasm.display());
        }
        Ok(o) => {
            // Non-fatal: the test/bench skips when the var is empty. Surface the
            // guest build's stderr so a real failure isn't silent; the common
            // benign cause is a missing `wasm32-wasip2` target (`rustup target
            // add wasm32-wasip2`) — CI installs it so the perf gate runs there.
            let err = String::from_utf8_lossy(&o.stderr);
            let tail: String = err.lines().rev().take(4).collect::<Vec<_>>().join(" | ");
            println!(
                "cargo:warning={name} fixture guest not built (target missing or build failed); \
                 the dependent test/bench will skip. Last stderr: {tail}"
            );
            println!("cargo:rustc-env={env_var}=");
        }
        Err(e) => {
            println!("cargo:warning=could not run cargo for the {name} guest: {e}");
            println!("cargo:rustc-env={env_var}=");
        }
    }
}
