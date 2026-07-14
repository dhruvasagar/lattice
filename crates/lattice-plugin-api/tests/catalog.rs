//! PI.1 catalog tests: the catalog is derived from `wit/`, and the host-authored
//! capability annotation covers every parsed interface (the drift guard — a new
//! WIT interface can't ship without a deliberate capability decision).

use lattice_plugin_api::{Capability, Direction, capability_for, catalog};

/// The headline four-artefact test: EVERY parsed interface has an explicit
/// capability annotation. A new `wit/` interface fails here until someone adds
/// a `CAPABILITY_ANNOTATIONS` row — the deliberate-decision gate.
#[test]
fn capability_annotation_covers_every_interface() {
    let cat = catalog();
    assert!(!cat.interfaces.is_empty(), "catalog parsed no interfaces");
    for iface in &cat.interfaces {
        assert!(
            capability_for(&iface.name).is_some(),
            "interface `{}` has no capability annotation — add a CAPABILITY_ANNOTATIONS row",
            iface.name
        );
    }
}

/// The catalog reflects the canonical `wit/` — spot-check the seams whose shape
/// is load-bearing so a regression in the build-time parse is caught.
#[test]
fn catalog_reflects_the_canonical_wit() {
    let cat = catalog();

    // `host-services` is the one guest→host, fs-capable seam today.
    let hs = cat
        .interface("host-services")
        .expect("host-services interface present");
    assert_eq!(hs.direction, Direction::GuestImport);
    assert_eq!(hs.capability, Capability::Fs);
    assert!(
        hs.functions.iter().any(|f| f.name == "walk"),
        "host-services should expose `walk`"
    );
    assert!(hs.doc.is_some(), "host-services carries its `///` doc");

    // `picker-source` is a guest-implemented seam (a world exports it).
    let ps = cat
        .interface("picker-source")
        .expect("picker-source interface present");
    assert_eq!(ps.direction, Direction::GuestExport);
    assert_eq!(ps.capability, Capability::None);

    // `types` is the shared type bag — referenced only for its types.
    assert_eq!(
        cat.interface("types")
            .expect("types interface present")
            .direction,
        Direction::TypesOnly,
    );
}

/// Functions and interfaces come out sorted (deterministic catalog output).
#[test]
fn catalog_is_sorted_and_deterministic() {
    let cat = catalog();

    let names: Vec<&str> = cat.interfaces.iter().map(|i| i.name.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "interfaces must be sorted by name");

    for iface in &cat.interfaces {
        let fns: Vec<&str> = iface.functions.iter().map(|f| f.name.as_str()).collect();
        let mut fsorted = fns.clone();
        fsorted.sort_unstable();
        assert_eq!(fns, fsorted, "functions in `{}` must be sorted", iface.name);
    }

    // Cached: the same reference on every call.
    assert!(std::ptr::eq(catalog(), catalog()));
}

/// The test-only `trampoline-fixture` world is not a plugin API and must be
/// excluded, while the real plugin worlds are present.
#[test]
fn worlds_exclude_the_test_fixture() {
    let cat = catalog();
    assert!(
        cat.world("trampoline-fixture").is_none(),
        "the test-only trampoline-fixture world must not appear in the API catalog"
    );
    assert!(
        cat.world("picker-source-plugin").is_some(),
        "real plugin worlds should be catalogued"
    );
    // A world that exports `picker-source` records that export edge.
    let w = cat
        .world("picker-source-plugin")
        .expect("picker-source-plugin world present");
    assert!(w.exports.iter().any(|e| e == "picker-source"));
}
