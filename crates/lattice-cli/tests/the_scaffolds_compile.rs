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
//! So: run the real binary, then build what it wrote with the same builder
//! the editor uses at boot ([`CargoComponentBuilder`] — same target, same
//! env scrubbing, same `wit/` the scaffold embedded).
//!
//! Slow (a cold component build, twice) and it needs the network for
//! `wit-bindgen`; that is the price of testing the claim rather than its
//! file listing.

use std::path::{Path, PathBuf};
use std::process::Command;

use lattice_plugin_loader::{CargoComponentBuilder, ComponentBuilder, Toolchain};

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

fn assert_builds(dir: &Path) -> PathBuf {
    CargoComponentBuilder
        .build(dir)
        .unwrap_or_else(|error| panic!("{} does not build:\n{error}", dir.display()))
}

#[test]
fn the_init_scaffold_compiles() {
    let tmp = tempfile::tempdir().unwrap();
    if !toolchain_ready(tmp.path()) {
        return;
    }
    scaffold(tmp.path(), &["--scaffold-init"]);
    let wasm = assert_builds(&tmp.path().join("lattice").join("init"));
    // The name the editor stages from: a renamed crate would build fine and
    // then never be found.
    assert_eq!(
        wasm.file_name().and_then(|n| n.to_str()),
        Some("lattice_init.wasm")
    );
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
