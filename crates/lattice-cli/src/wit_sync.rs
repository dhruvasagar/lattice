//! WT.4: `lattice --wit-sync [DIR]` — rewrite a plugin source's `wit/` from
//! this editor's canonical API package.
//!
//! Design: [`wit-ownership.md`](../../../docs/dev/architecture/wit-ownership.md)
//! §3(b), the repair path.
//!
//! ## Why this exists when WT.2b already refreshes every build
//!
//! WT.2b covers every source *the editor builds*, which is very nearly
//! everything — and it would still not have unstuck the failure that started
//! this. `init.wasm` holds the `require("org")` that installs and rebuilds
//! everything else. When `init.wasm` itself will not instantiate, nothing runs,
//! so nothing rebuilds — including `init.wasm`. The thing that repairs stale
//! plugins was itself the stale plugin, and both had to be fixed by hand.
//!
//! This is the hand, made into one command. It needs no editor to boot
//! successfully, no plugin to load, and no build to have worked; it is a file
//! copy from the binary you are already running.
//!
//! ## What it deliberately does not do
//!
//! It does not build. A user whose install is broken wants the two steps
//! separate: repair the API definition, then watch the build succeed or fail on
//! its own terms. Folding a build in here would bury the second failure inside
//! the first.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Run the sync for `dir`, or for the default set when `dir` is `None`.
///
/// The default set is the user's `init` directory plus every immediate child of
/// the plugins directory — the sources this editor would build. A directory that
/// is not a plugin source is skipped with a word rather than an error: sweeping
/// the plugins dir is a convenience, and one unrelated folder in it should not
/// fail the repair of everything else.
pub fn wit_sync(dir: Option<&Path>) -> Result<()> {
    let targets = match dir {
        Some(d) => vec![d.to_path_buf()],
        None => default_targets()?,
    };
    if targets.is_empty() {
        println!("No plugin sources found to sync.");
        println!("Pass a directory explicitly: `lattice --wit-sync path/to/plugin`.");
        return Ok(());
    }

    let mut synced = 0usize;
    for target in &targets {
        match sync_one(target) {
            Ok(true) => {
                synced += 1;
                println!("  synced  {}", target.display());
            }
            // Explicitly reported rather than counted silently: a user running a
            // repair wants to know which directories it declined to touch, or
            // the one they cared about disappears into a success total.
            Ok(false) => println!("  skipped {} (not a plugin source)", target.display()),
            Err(e) => println!("  FAILED  {}: {e:#}", target.display()),
        }
    }

    println!(
        "\n{synced} of {} synced against ABI {}.",
        targets.len(),
        lattice_wit::ABI_FINGERPRINT
    );
    if synced > 0 {
        println!("Rebuild happens on the next start — or `:reload-config` in a running editor.");
    }
    Ok(())
}

/// Write the package into `dir/wit`, or report that `dir` is not a source.
///
/// "Is a plugin source" means it has a `Cargo.toml`. That is the same test
/// `build_init_if_needed` uses to decide whether a directory is buildable at
/// all, and using a different one here would let the two disagree about what a
/// plugin is.
fn sync_one(dir: &Path) -> Result<bool> {
    if !dir.join("Cargo.toml").is_file() {
        return Ok(false);
    }
    lattice_wit::write_to(dir.join("wit"))
        .with_context(|| format!("writing the wit package into {}", dir.display()))?;
    Ok(true)
}

/// The init dir plus every immediate child of the plugins dir.
///
/// A missing directory contributes nothing rather than failing: a user with no
/// plugins installed and a hand-dropped `init.wasm` should get a clean "nothing
/// to sync", not an error about a path they have never heard of.
fn default_targets() -> Result<Vec<PathBuf>> {
    let home = lattice_config::config_home()
        .context("no config home directory on this platform (set $XDG_CONFIG_HOME)")?
        .join("lattice");

    let mut out = Vec::new();
    let init = home.join("init");
    if init.is_dir() {
        out.push(init);
    }
    if let Ok(entries) = std::fs::read_dir(home.join("plugins")) {
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        // Sorted, so the printed report is the same on every run and on every
        // machine. Readdir order is not.
        dirs.sort();
        out.extend(dirs);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lattice-wt4-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The repair on a source that has no `wit/` at all — the shape a dead
    /// `init.wasm` leaves behind once someone has deleted the drifted copy, and
    /// the shape a fresh clone of a repo that gitignores `wit/` arrives in.
    #[test]
    fn a_source_with_no_wit_directory_is_populated() {
        let dir = tempdir("empty");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();

        assert!(sync_one(&dir).unwrap(), "reported as synced");
        for name in lattice_wit::file_names() {
            assert!(dir.join("wit").join(name).is_file(), "wit/{name} landed");
        }
    }

    /// The case the command exists for: a copy that drifted behind the editor.
    #[test]
    fn a_drifted_copy_is_overwritten() {
        let dir = tempdir("drift");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        std::fs::create_dir_all(dir.join("wit")).unwrap();
        std::fs::write(dir.join("wit").join("types.wit"), "// three ABIs ago\n").unwrap();

        sync_one(&dir).unwrap();
        let repaired = std::fs::read_to_string(dir.join("wit").join("types.wit")).unwrap();
        assert!(!repaired.contains("three ABIs ago"));
    }

    /// A directory that is not a cargo project is declined, not written into.
    /// The plugins dir is swept wholesale, and a stray folder in it must not
    /// acquire a `wit/` — nor fail the repair of everything beside it.
    #[test]
    fn a_directory_that_is_not_a_source_is_skipped() {
        let dir = tempdir("notsource");
        std::fs::write(dir.join("notes.txt"), "hello").unwrap();

        assert!(!sync_one(&dir).unwrap(), "reported as skipped");
        assert!(!dir.join("wit").exists(), "and nothing was written");
    }

    /// An explicit directory is synced without consulting the config home, so
    /// the command works against a checkout anywhere — including one belonging
    /// to a plugin the editor has never installed.
    #[test]
    fn an_explicit_directory_needs_no_config_home() {
        let dir = tempdir("explicit");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();

        wit_sync(Some(&dir)).unwrap();
        assert!(dir.join("wit").join("types.wit").is_file());
    }
}
