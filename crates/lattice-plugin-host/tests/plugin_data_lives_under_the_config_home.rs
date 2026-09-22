//! A plugin's private data lives under the config home, not the install tree.
//!
//! **The bug this closes is data loss.** `install.sh` defaults to
//! `PREFIX=$HOME/.local`, so the bundled plugins install to
//! `~/.local/share/lattice/plugins/<id>/`. The plugin host used
//! `dirs::data_dir()/lattice/plugins/<id>/data/` for each plugin's private
//! store — and on Linux `dirs::data_dir()` IS `~/.local/share`. The installer
//! upgrades by swapping that directory and `rm -rf`-ing the old one, so every
//! reinstall deleted every plugin's saved state, user plugins included.
//! Reproduced 2026-09-22 against the published v0.9.0 archive: a seeded
//! `plugins/project/data/plugin-store.bin` did not survive the install.
//!
//! The fix is the layout, not a flag in the installer: a plugin's state now
//! lives beside the plugin itself, in lattice's config home —
//! `~/.config/lattice/plugins/<name>/data/` on Linux AND macOS, per this
//! project's "everything lattice lives under `~/.config/lattice`" policy.
//! That is the tree `require`d plugins are already built into, it is never an
//! install prefix, and an in-place update (git fetch + build) leaves it
//! alone; an explicit uninstall removes the plugin and its data together,
//! which is what uninstall should do. `migrate_plugin_data` carries existing
//! directories over once.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::Path;

use lattice_plugin_host::{data_dir_base_from, migrate_plugin_data};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn the_data_base_sits_under_the_config_home() {
    let base = data_dir_base_from(Some(Path::new("/home/u/.config")));
    assert_eq!(base, Path::new("/home/u/.config/lattice/plugins"));
}

/// The path that bit us: with the installer's default prefix, the OLD base and
/// the install tree are the same directory. The new one cannot collide with
/// any prefix, because `~/.config` is not one.
#[test]
fn the_data_base_is_not_inside_an_install_prefix() {
    let base = data_dir_base_from(Some(Path::new("/home/u/.config")));
    for prefix in ["/home/u/.local", "/usr/local", "/opt/lattice"] {
        assert!(
            !base.starts_with(Path::new(prefix).join("share")),
            "{} would be wiped by an install to {prefix}",
            base.display()
        );
    }
}

#[test]
fn migration_moves_each_plugins_data_once() {
    let tmp = tempfile::tempdir().unwrap();
    let old = tmp.path().join("old");
    let new = tmp.path().join("new");
    write(&old.join("project/data/plugin-store.bin"), "recents");
    write(&old.join("org/data/state.bin"), "agenda");

    let moved = migrate_plugin_data(&old, &new);

    assert_eq!(moved, 2, "both plugins' data moved");
    assert_eq!(
        std::fs::read_to_string(new.join("project/data/plugin-store.bin")).unwrap(),
        "recents"
    );
    assert_eq!(
        std::fs::read_to_string(new.join("org/data/state.bin")).unwrap(),
        "agenda"
    );
    assert!(!old.join("project/data").exists(), "the old copy is gone");

    // Idempotent: a second boot finds nothing left to carry.
    assert_eq!(migrate_plugin_data(&old, &new), 0);
}

/// The old base doubles as the INSTALL tree on Linux, so it holds the bundled
/// plugins' components and manifests. Migration must take `<name>/data` and
/// nothing else — moving a whole `<name>` directory would uninstall the
/// plugin.
#[test]
fn migration_leaves_installed_plugin_files_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let old = tmp.path().join("old");
    let new = tmp.path().join("new");
    write(&old.join("project/plugin.toml"), "id = \"project\"");
    write(&old.join("project/project.wasm"), "\0asm");
    write(&old.join("project/data/plugin-store.bin"), "recents");

    assert_eq!(migrate_plugin_data(&old, &new), 1);

    assert!(old.join("project/plugin.toml").exists());
    assert!(old.join("project/project.wasm").exists());
    assert!(new.join("project/data/plugin-store.bin").exists());
}

/// Never overwrite state the new location already has: whatever is there was
/// written by a newer lattice, and the old copy is the stale one.
#[test]
fn migration_never_overwrites_existing_data() {
    let tmp = tempfile::tempdir().unwrap();
    let old = tmp.path().join("old");
    let new = tmp.path().join("new");
    write(&old.join("project/data/plugin-store.bin"), "stale");
    write(&new.join("project/data/plugin-store.bin"), "current");

    assert_eq!(migrate_plugin_data(&old, &new), 0);
    assert_eq!(
        std::fs::read_to_string(new.join("project/data/plugin-store.bin")).unwrap(),
        "current"
    );
}

#[test]
fn migration_of_an_absent_old_base_is_a_no_op() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(
        migrate_plugin_data(&tmp.path().join("nothing-here"), &tmp.path().join("new")),
        0
    );
}
