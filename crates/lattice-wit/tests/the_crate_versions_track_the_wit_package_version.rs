//! The published crates' `major.minor` is the WIT package's `major.minor`.
//!
//! **The rule.** `lattice-wit`, `lattice-plugin-sdk` and
//! `lattice-plugin-sdk-derive` share one number, and its `major.minor` equals
//! the `package lattice:plugin-host@X.Y.Z` declared by the `.wit` files. Patch
//! is the crates' own, so a packaging fix does not have to masquerade as an ABI
//! change.
//!
//! **Why the crate version tracks the ABI and not the editor.** A plugin author
//! writes one line — `lattice-wit = "0.2"` — and that line has to answer "which
//! ABI generation am I compiled against", because it is the only place they
//! look. The editor's version cannot answer it: the ABI does not move when the
//! editor does. This is the same argument that took these three crates off
//! `version.workspace = true`, carried one step further.
//!
//! The SDK carries the number too. It has no `lattice-wit` dependency and is
//! WIT-agnostic Rust, so this is not a structural coupling — but it is a real
//! one: `ConfigShape` flattens into exactly the arena shape the config seam
//! consumes. A semantic coupling with independent version lines is the worst
//! case, because nothing says the two are related until something is subtly
//! wrong at runtime.
//!
//! **Not to be confused with `ABI_FINGERPRINT`.** The fingerprint is a content
//! hash that moves on any edit, including a doc comment, and it drives
//! *rebuilds* through the `.build-stamp`. The package version drives whether a
//! component *links at all*, because the Component Model puts it in the
//! imported interface names. Different jobs; only the second is a version.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::Path;

/// The three crates published to crates.io under one plugin-API version.
const PUBLISHED: &[&str] = &[
    "lattice-wit",
    "lattice-plugin-sdk",
    "lattice-plugin-sdk-derive",
];

fn crates_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
}

fn wit_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/wit"))
}

/// `major.minor` of an `X.Y.Z`.
fn minor_of(version: &str) -> String {
    let mut parts = version.split('.');
    let major = parts.next().unwrap_or_default();
    let minor = parts.next().unwrap_or_default();
    format!("{major}.{minor}")
}

/// The `version = "X.Y.Z"` from a crate's `[package]` section.
fn crate_version(name: &str) -> String {
    let path = crates_dir().join(name).join("Cargo.toml");
    let manifest = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    manifest
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("version = \""))
        .and_then(|rest| rest.split('"').next())
        .unwrap_or_else(|| {
            panic!(
                "{name} has no literal `version = \"X.Y.Z\"`. These three crates \
                 are deliberately NOT `version.workspace = true` — see this \
                 file's header."
            )
        })
        .to_owned()
}

/// Every `package lattice:plugin-host@X.Y.Z;` declared under `wit/`, by file.
fn declared_packages() -> Vec<(String, String)> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(wit_dir()).expect("wit/ is readable") {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "wit") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let version = text
            .lines()
            .map(str::trim)
            .find_map(|line| line.strip_prefix("package lattice:plugin-host@"))
            .and_then(|rest| rest.split(';').next())
            .unwrap_or_else(|| panic!("{name} declares no `package lattice:plugin-host@X.Y.Z;`"))
            .to_owned();
        found.push((name, version));
    }
    assert!(!found.is_empty(), "no .wit files found under wit/");
    found
}

/// The one package version, having established the files agree.
fn wit_package_version() -> String {
    let declared = declared_packages();
    let (_, first) = &declared[0];
    first.clone()
}

#[test]
fn every_wit_file_declares_the_same_package_version() {
    let declared = declared_packages();
    let (_, expected) = &declared[0];
    let disagreeing: Vec<String> = declared
        .iter()
        .filter(|(_, v)| v != expected)
        .map(|(name, v)| format!("{name} declares @{v}, expected @{expected}"))
        .collect();
    assert!(
        disagreeing.is_empty(),
        "the WIT package version is stated in {} files and they must agree — a \
         single file drifting produces a package that fails to parse or links \
         only half its interfaces:\n  {}",
        declared.len(),
        disagreeing.join("\n  ")
    );
}

#[test]
fn the_published_crates_share_one_version() {
    let versions: Vec<(&str, String)> = PUBLISHED
        .iter()
        .map(|name| (*name, crate_version(name)))
        .collect();
    let (_, first) = &versions[0];
    let odd: Vec<String> = versions
        .iter()
        .filter(|(_, v)| v != first)
        .map(|(name, v)| format!("{name} is {v}, expected {first}"))
        .collect();
    assert!(
        odd.is_empty(),
        "the three published crates share one plugin-API version, so an author \
         tracks one number rather than three:\n  {}",
        odd.join("\n  ")
    );
}

#[test]
fn the_crate_version_tracks_the_wit_package_minor() {
    let package = wit_package_version();
    let expected = minor_of(&package);
    let wrong: Vec<String> = PUBLISHED
        .iter()
        .map(|name| (*name, crate_version(name)))
        .filter(|(_, v)| minor_of(v) != expected)
        .map(|(name, v)| format!("{name} {v} → {}, expected {expected}", minor_of(&v)))
        .collect();
    assert!(
        wrong.is_empty(),
        "`lattice-wit = \"{expected}\"` must mean ABI \
         `lattice:plugin-host@{expected}.x`. The .wit files declare @{package}, \
         so the crates' major.minor must be {expected}. Bump both together when \
         the ABI moves; patch alone is free for crate-only fixes:\n  {}",
        wrong.join("\n  ")
    );
}
