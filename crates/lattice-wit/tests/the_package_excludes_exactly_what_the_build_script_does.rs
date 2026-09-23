//! `Cargo.toml`'s `exclude` and `build.rs`'s `EXCLUDE` name the same files.
//!
//! Two lists, one intent: the `wit/` files that are not public API — a bundled
//! plugin's own world and this repo's host-test fixtures. `build.rs` keeps them
//! out of the embedded `FILES` and the ABI fingerprint; `Cargo.toml` keeps them
//! out of the published tarball.
//!
//! They can drift in both directions and each drift is quiet:
//!
//!   * a file added to `build.rs` only — still shipped to plugin authors, who
//!     find a fixture world in what is meant to be the API package;
//!   * a file added to `Cargo.toml` only — absent from the tarball while
//!     `build.rs` still expects to filter it, so the published crate's `wit/`
//!     and its fingerprint describe different file sets than the repo's.
//!
//! Neither shows up in a build. So the lists are read from their own sources
//! and compared, rather than kept in step by attention.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

fn crate_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// The `exclude = [...]` array in this crate's `Cargo.toml`, as bare filenames.
fn manifest_excludes() -> BTreeSet<String> {
    let manifest = std::fs::read_to_string(crate_dir().join("Cargo.toml")).unwrap();
    let (_, rest) = manifest
        .split_once("\nexclude = [")
        .expect("Cargo.toml has an `exclude = [` array");
    let list = rest.split_once(']').expect("exclude array is closed").0;
    list.split(',')
        .map(|part| part.trim().trim_matches('"'))
        .filter(|part| !part.is_empty())
        .map(|part| part.trim_start_matches("wit/").to_owned())
        .collect()
}

/// The `EXCLUDE` slice literal in `build.rs`.
fn build_script_excludes() -> BTreeSet<String> {
    let build = std::fs::read_to_string(crate_dir().join("build.rs")).unwrap();
    let (_, rest) = build
        .split_once("const EXCLUDE")
        .expect("build.rs declares EXCLUDE");
    let list = rest
        .split_once("= &[")
        .and_then(|(_, r)| r.split_once(']'))
        .expect("EXCLUDE is a slice literal")
        .0;
    list.split(',')
        .map(|part| part.trim().trim_matches('"'))
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_two_exclude_lists_agree() {
    let manifest = manifest_excludes();
    let build = build_script_excludes();
    assert!(!manifest.is_empty(), "parsed no excludes out of Cargo.toml");
    assert_eq!(
        manifest, build,
        "Cargo.toml's `exclude` and build.rs's `EXCLUDE` disagree. Every \
         non-API file must be in BOTH — out of the tarball and out of the \
         embedded FILES — or the published crate and this repo describe \
         different ABIs."
    );
}

#[test]
fn every_excluded_file_actually_exists() {
    let missing: Vec<String> = build_script_excludes()
        .into_iter()
        .filter(|name| !crate_dir().join("wit").join(name).is_file())
        .collect();
    assert!(
        missing.is_empty(),
        "the exclude lists name wit/ files that are not there. A stale entry \
         hides the next real one — if the file was deleted, drop it from both \
         lists:\n  {}",
        missing.join("\n  ")
    );
}
