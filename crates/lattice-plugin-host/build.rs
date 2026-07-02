//! Builds the PH7.3d trampoline fixture guest to a `wasm32-wasip2` component so
//! `tests/trampoline.rs` can load it (a real guest↔host canonical-ABI call).
//!
//! The guest is a *standalone workspace* under `tests/fixtures/trampoline-guest`
//! with its own `target/`, so building it here never contends with the host's
//! build lock. On success we hand the test the component path via the
//! `TRAMPOLINE_GUEST_WASM` env var. If the `wasm32-wasip2` target is missing or
//! the guest build fails, we set the var *empty* and warn — the host build
//! still succeeds and the trampoline test skips (with a clear message) rather
//! than breaking every `cargo build` on a box without the wasm target.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo"),
    );
    let guest_dir = manifest_dir
        .join("tests")
        .join("fixtures")
        .join("trampoline-guest");

    // Rebuild the fixture whenever its source, manifest, or the shared WIT
    // (which defines the world it targets) changes.
    println!("cargo:rerun-if-changed={}", guest_dir.join("src").display());
    println!(
        "cargo:rerun-if-changed={}",
        guest_dir.join("Cargo.toml").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("..").join("..").join("wit").display()
    );

    // The guest builds into its own workspace `target/`, pinned explicitly so a
    // leaked `CARGO_TARGET_DIR` can't redirect the output out from under the
    // path we check below.
    let target_dir = guest_dir.join("target");
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join("trampoline_guest.wasm");

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(&cargo)
        .current_dir(&guest_dir)
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
            println!("cargo:rustc-env=TRAMPOLINE_GUEST_WASM={}", wasm.display());
        }
        Ok(o) => {
            // Non-fatal: the test/bench skip when the var is empty. Surface the
            // guest build's stderr so a real failure isn't silent; the common
            // benign cause is a missing `wasm32-wasip2` target (`rustup target
            // add wasm32-wasip2`) — CI installs it so the perf gate runs there.
            let err = String::from_utf8_lossy(&o.stderr);
            let tail: String = err.lines().rev().take(4).collect::<Vec<_>>().join(" | ");
            println!(
                "cargo:warning=trampoline fixture guest not built (target missing or build \
                 failed); PH7.3d trampoline test/bench will skip. Last stderr: {tail}"
            );
            println!("cargo:rustc-env=TRAMPOLINE_GUEST_WASM=");
        }
        Err(e) => {
            println!("cargo:warning=could not run cargo for the trampoline guest: {e}");
            println!("cargo:rustc-env=TRAMPOLINE_GUEST_WASM=");
        }
    }
}
