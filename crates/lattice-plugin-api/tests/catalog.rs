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

/// AD.1: the catalog carries full signatures, type definitions and `use`s —
/// spot-checked against shapes the WIT spells out, one per construct, so a
/// regression in any branch of the build-time type renderer is caught.
#[test]
fn catalog_carries_signatures_types_and_uses() {
    use lattice_plugin_api::{ApiFunctionKind, ApiTypeKind};
    let cat = catalog();

    // A freestanding function: params with names and WIT-spelled types, and a
    // nested generic result.
    let walk = cat
        .interface("host-services")
        .and_then(|i| i.functions.iter().find(|f| f.name == "walk"))
        .expect("host-services.walk");
    assert_eq!(walk.kind, ApiFunctionKind::Freestanding);
    assert_eq!(walk.params.len(), 1);
    assert_eq!(walk.params[0].name, "root");
    assert_eq!(walk.params[0].ty, "string");
    assert_eq!(walk.result.as_deref(), Some("result<list<string>, string>"));
    assert_eq!(
        walk.signature(),
        "walk: func(root: string) -> result<list<string>, string>"
    );

    // A resource method: owner resolved, `self` present in params but not in
    // the rendered signature, display name unmangled.
    let buffer = cat.interface("buffer").expect("buffer");
    let line = buffer
        .functions
        .iter()
        .find(|f| f.name == "[method]document.line")
        .expect("document.line");
    assert_eq!(line.kind, ApiFunctionKind::Method("document".into()));
    assert_eq!(line.params[0].name, "self");
    assert_eq!(line.params[0].ty, "borrow<document>");
    assert_eq!(line.display_name(), "document.line");
    assert_eq!(line.signature(), "line: func(n: u32) -> option<string>");
    assert!(
        buffer
            .types
            .iter()
            .any(|t| t.name == "document" && t.kind == ApiTypeKind::Resource),
        "the `document` resource is a type of `buffer`"
    );

    // A record with per-field types.
    let (owner, pair) = cat.type_def("candidate-pair").expect("candidate-pair");
    assert_eq!(owner.name, "picker-source");
    let fields = pair.kind.members();
    assert_eq!(pair.kind.keyword(), "record");
    assert_eq!(fields[0].name, "candidate");
    assert_eq!(fields[0].ty.as_deref(), Some("raw-candidate"));
    assert_eq!(fields[1].name, "routing");

    // A variant: payload-less and documented cases both come through.
    let (_, effect) = cat.type_def("effect").expect("effect");
    assert_eq!(effect.kind.keyword(), "variant");
    let cases = effect.kind.members();
    assert_eq!(cases[0].name, "none");
    assert_eq!(cases[0].ty, None);
    let declined = cases
        .iter()
        .find(|c| c.name == "declined")
        .expect("declined");
    assert!(
        declined.doc.as_deref().unwrap_or("").contains("DECLINES"),
        "variant case docs are carried"
    );

    // An enum and an alias.
    let (_, level) = cat.type_def("level").expect("logging.level");
    assert_eq!(level.kind.keyword(), "enum");
    assert!(!level.kind.members().is_empty());
    let (_, count) = cat.type_def("count").expect("types.count");
    assert_eq!(count.kind, ApiTypeKind::Alias("u32".into()));

    // A `use` is recorded as a use, not duplicated as a local definition.
    let ps = cat.interface("picker-source").expect("picker-source");
    assert!(
        ps.uses
            .iter()
            .any(|u| u.name == "raw-candidate" && u.from == "types"),
        "picker-source uses types.raw-candidate"
    );
    assert!(
        !ps.types.iter().any(|t| t.name == "raw-candidate"),
        "a used type must not also appear as a local definition"
    );
}

/// Every type named in a signature, field or case resolves to a definition
/// somewhere in the catalog. A renderer that emitted `<anonymous>` or a name
/// with no definition would produce a reference with dead ends.
#[test]
fn every_referenced_type_name_resolves() {
    let cat = catalog();
    let builtin = [
        "bool",
        "u8",
        "u16",
        "u32",
        "u64",
        "s8",
        "s16",
        "s32",
        "s64",
        "f32",
        "f64",
        "char",
        "string",
        "list",
        "option",
        "result",
        "tuple",
        "borrow",
        "map",
        "future",
        "stream",
        "error-context",
        "_",
    ];
    let mut exprs = Vec::new();
    for iface in &cat.interfaces {
        for f in &iface.functions {
            exprs.extend(f.params.iter().map(|p| p.ty.clone()));
            exprs.extend(f.result.clone());
        }
        for t in &iface.types {
            exprs.extend(t.kind.members().iter().filter_map(|m| m.ty.clone()));
            if let lattice_plugin_api::ApiTypeKind::Alias(a) = &t.kind {
                exprs.push(a.clone());
            }
        }
    }
    let known: Vec<&str> = cat
        .interfaces
        .iter()
        .flat_map(|i| {
            i.types
                .iter()
                .map(|t| t.name.as_str())
                .chain(i.uses.iter().map(|u| u.name.as_str()))
        })
        .collect();
    for expr in &exprs {
        assert!(!expr.contains("<anonymous>"), "unrendered type in `{expr}`");
        for word in expr
            .split(|c: char| "<>, ".contains(c))
            .filter(|w| !w.is_empty())
        {
            let is_number = word.chars().all(|c| c.is_ascii_digit());
            assert!(
                is_number || builtin.contains(&word) || known.contains(&word),
                "type `{word}` (in `{expr}`) has no definition in the catalog"
            );
        }
    }
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
        cat.worlds.iter().all(|w| !w.name.ends_with("-fixture")),
        "test-only `*-fixture` worlds must not appear in the API catalog"
    );
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
