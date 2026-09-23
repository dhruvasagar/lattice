//! Caches live under the config home too, in a directory named `cache`.
//!
//! Lattice keeps everything under `~/.config/lattice/` on Linux and macOS
//! alike — config, the plugin tree, plugin data, tutor scores. The three
//! caches (wasmtime's compiled modules, plugin source checkouts, the picker's
//! MRU index) were the last paths outside it, in `dirs::cache_dir()`.
//!
//! They sit in their own `cache/` subdirectory rather than loose in the root,
//! so the one directory that is always safe to delete says so in its name.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::Path;

use lattice_config::{cache_home_from, migrate_path};

#[test]
fn the_cache_root_is_a_cache_directory_under_the_config_home() {
    let root = cache_home_from(Some(Path::new("/home/u/.config"))).unwrap();
    assert_eq!(root, Path::new("/home/u/.config/lattice/cache"));
}

#[test]
fn no_config_home_means_no_cache_root() {
    assert_eq!(cache_home_from(None), None);
}

#[test]
fn migrate_moves_a_directory_once() {
    let tmp = tempfile::tempdir().unwrap();
    let old = tmp.path().join("old-cache");
    let new = tmp.path().join("new-cache");
    std::fs::create_dir_all(old.join("modules")).unwrap();
    std::fs::write(old.join("modules/a.bin"), "compiled").unwrap();

    assert!(migrate_path(&old, &new));
    assert_eq!(
        std::fs::read_to_string(new.join("modules/a.bin")).unwrap(),
        "compiled"
    );
    assert!(!old.exists());
    // Second boot: nothing left to carry.
    assert!(!migrate_path(&old, &new));
}

#[test]
fn migrate_moves_a_file_too() {
    let tmp = tempfile::tempdir().unwrap();
    let old = tmp.path().join("picker-mru.bincode");
    let new = tmp.path().join("cache/picker-mru.bincode");
    std::fs::write(&old, "frecency").unwrap();

    assert!(migrate_path(&old, &new));
    assert_eq!(std::fs::read_to_string(&new).unwrap(), "frecency");
}

/// Whatever is at the destination was written by a newer lattice; the old
/// copy is the stale one, and moving it over would lose the live state.
#[test]
fn migrate_never_overwrites_the_destination() {
    let tmp = tempfile::tempdir().unwrap();
    let old = tmp.path().join("old");
    let new = tmp.path().join("new");
    std::fs::write(&old, "stale").unwrap();
    std::fs::write(&new, "current").unwrap();

    assert!(!migrate_path(&old, &new));
    assert_eq!(std::fs::read_to_string(&new).unwrap(), "current");
    assert!(old.exists(), "the old copy is left for the user to remove");
}

#[test]
fn migrating_something_absent_is_a_no_op() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(!migrate_path(
        &tmp.path().join("nothing"),
        &tmp.path().join("new")
    ));
}
