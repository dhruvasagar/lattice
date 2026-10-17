//! PMF.2: `manifest!` builds in a crate that denies unsafe code, and what it
//! embeds is the manifest text, byte for byte.
//!
//! That the section survives into a `wasm32-wasip2` component — through
//! componentization, strip and LTO — is the spike recorded in
//! `docs/dev/architecture/plugin-manifest.md` §1; the host's parse of this
//! payload is `lattice-plugin-host/tests/sdk_manifest.rs`.

#![deny(unsafe_code)]
#![allow(clippy::unwrap_used)]

const TOML: &str = "id = \"sdk-fixture\"\nprovides = [\"config\"]\nmin_lattice = \"0.9.10\"\n";

lattice_plugin_sdk::manifest!(TOML);

#[test]
fn the_static_is_the_manifest_text() {
    assert_eq!(std::str::from_utf8(&LATTICE_MANIFEST).unwrap(), TOML);
}

#[test]
fn the_section_name_is_the_one_the_loader_reads() {
    assert_eq!(lattice_plugin_sdk::MANIFEST_SECTION, "lattice-manifest");
}
