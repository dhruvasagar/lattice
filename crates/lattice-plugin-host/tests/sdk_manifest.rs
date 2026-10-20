//! PMF.2: what `lattice_plugin_sdk::manifest!` embeds is a manifest the host
//! parses — the SDK and the loader agree on the section name and the payload.

#![allow(clippy::unwrap_used)]

use lattice_plugin_host::PluginManifest;
use lattice_plugin_host::manifest::MANIFEST_SECTION;

lattice_plugin_sdk::manifest!(
    r#"
id = "sdk-fixture"
provides = ["config"]
capabilities = ["fs:write:{sdk-fixture.dir|~/sdk}", "state:write"]
min_lattice = "0.9.10"
"#
);

#[test]
fn the_sdk_payload_round_trips_through_the_host_parser() {
    let text = std::str::from_utf8(&LATTICE_MANIFEST).unwrap();
    let manifest = PluginManifest::from_toml_str(text).unwrap();
    assert_eq!(manifest.id, "sdk-fixture");
    assert_eq!(manifest.min_lattice, Some((0, 9, 10)));
    assert_eq!(manifest.requested.len(), 2);
}

#[test]
fn the_sdk_and_the_host_name_the_same_section() {
    assert_eq!(lattice_plugin_sdk::MANIFEST_SECTION, MANIFEST_SECTION);
}
