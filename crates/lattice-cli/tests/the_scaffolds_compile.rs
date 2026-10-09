//! `lattice --scaffold-init` and `--scaffold-plugin` promise a **buildable**
//! starter. This is the test that the promise is kept.
//!
//! The scaffold's `src/lib.rs` is a string constant in `scaffold.rs`, so
//! nothing compiles it: when the WIT gained `EventFilter::minor-modes`, every
//! in-repo plugin failed to build and was fixed the same day, and the
//! scaffold kept the old initializer. 0.9.2 and 0.9.3 both shipped a
//! `--scaffold-init` whose output did not compile — the first thing a new
//! user's config does. The existing unit test checked that the files exist.
//!
//! So: run the real binary, then build what it wrote through
//! [`build_plugin`] — the editor's whole build path, not just its cargo
//! invocation. The difference is not academic. `build_plugin` first rewrites
//! the API package into `wit/`, and the plugin scaffold kept its world in a
//! file that rewrite replaces; a test that called the builder alone passed
//! over a scaffold the editor could not build.
//!
//! Slow (a cold component build, twice) and it needs the network for
//! `wit-bindgen`; that is the price of testing the claim rather than its
//! file listing.

use std::path::{Path, PathBuf};
use std::process::Command;

use lattice_plugin_loader::{BuildOutcome, CargoComponentBuilder, Toolchain, build_plugin};

/// Whether this machine can build a component. Without the toolchain there
/// is nothing to test, so a developer's run skips — but CI has the target
/// installed on purpose, and a skip there would be the guard quietly
/// switching itself off, so under `CI` a missing toolchain is a failure.
fn toolchain_ready(dir: &Path) -> bool {
    match Toolchain::probe(dir).problem() {
        None => true,
        Some(problem) if std::env::var_os("CI").is_some() => {
            panic!("CI cannot build the scaffolds: {problem}")
        }
        Some(problem) => {
            eprintln!("SKIP: cannot build the scaffolds here: {problem}");
            false
        }
    }
}

/// Run `lattice <args>` with its config home pointed at `config_home`.
fn scaffold(config_home: &Path, args: &[&str]) {
    let output = Command::new(env!("CARGO_BIN_EXE_lattice"))
        .args(args)
        .env("XDG_CONFIG_HOME", config_home)
        .output()
        .expect("run lattice");
    assert!(
        output.status.success(),
        "lattice {args:?} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Build `dir` in place, exactly as the editor does at boot, and return the
/// staged component.
fn assert_builds(dir: &Path) -> PathBuf {
    let name = dir.file_name().unwrap().to_str().unwrap();
    match build_plugin(
        &CargoComponentBuilder,
        dir,
        name,
        dir.parent().unwrap(),
        false,
    ) {
        BuildOutcome::Fresh { artifact } => artifact,
        other => panic!("{} does not build:\n{other:?}", dir.display()),
    }
}

#[test]
fn the_init_scaffold_compiles() {
    let tmp = tempfile::tempdir().unwrap();
    if !toolchain_ready(tmp.path()) {
        return;
    }
    scaffold(tmp.path(), &["--scaffold-init"]);
    let wasm = assert_builds(&tmp.path().join("lattice").join("init"));
    // Staged where the loader looks for it.
    assert_eq!(wasm.file_name().and_then(|n| n.to_str()), Some("init.wasm"));
}

#[test]
fn the_plugin_scaffold_compiles() {
    let tmp = tempfile::tempdir().unwrap();
    if !toolchain_ready(tmp.path()) {
        return;
    }
    scaffold(tmp.path(), &["--scaffold-plugin", "my-plugin"]);
    assert_builds(&tmp.path().join("lattice").join("plugins").join("my-plugin"));
}
