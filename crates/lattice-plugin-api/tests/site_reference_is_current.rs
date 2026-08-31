//! PI.6: the site's plugin-API reference is GENERATED from `wit/`, and this
//! test is what keeps it that way.
//!
//! **Why a checked-in file rather than a build step.** The reference has to be
//! readable on the site, in a browser, by someone deciding whether lattice's
//! plugin API can do what they need — before they have cloned anything. A page
//! that only exists after a build is a page that is not there when the
//! decision is made.
//!
//! **Why a test rather than a script someone remembers to run.** `wit/` is the
//! canonical API and it changes; three ABI additions landed in one day this
//! session alone. A generated doc nobody regenerates is worse than no doc,
//! because it is confidently wrong. This fails the build the moment the two
//! disagree, and `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`
//! writes the new one.

use std::path::PathBuf;

fn reference_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/dev/reference/plugin-api.md")
}

const HEADER: &str = "\
<!-- @generated from wit/ by crates/lattice-plugin-api/tests/site_reference_is_current.rs.
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

";

#[test]
fn the_site_reference_matches_the_wit_package() {
    let rendered = format!("{HEADER}{}", lattice_plugin_api::render::markdown());
    let path = reference_path();

    if std::env::var_os("UPDATE_SITE_REFERENCE").is_some() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).expect("create the reference directory");
        }
        std::fs::write(&path, &rendered).expect("write the reference");
        return;
    }

    let on_disk = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "{} is missing. Generate it with:\n  \
             UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api",
            path.display()
        )
    });
    assert_eq!(
        on_disk, rendered,
        "\nthe site's plugin-API reference is stale — `wit/` changed and the \
         page did not.\nRegenerate with:\n  \
         UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api\n"
    );
}

/// The reference is worth having only if it actually carries the surface. A
/// renderer that emitted a header and no interfaces would satisfy the equality
/// test above forever.
#[test]
fn the_reference_covers_every_seam_with_its_docs() {
    let md = lattice_plugin_api::render::markdown();
    let cat = lattice_plugin_api::catalog();
    assert!(
        cat.interfaces.len() > 20,
        "the catalog itself looks empty: {} seams",
        cat.interfaces.len()
    );
    for iface in &cat.interfaces {
        assert!(
            md.contains(&iface.name),
            "seam `{}` is missing from the reference",
            iface.name
        );
    }
    // Every seam carrying functions documents them.
    let with_fns = cat.interfaces.iter().filter(|i| !i.functions.is_empty());
    for iface in with_fns {
        for f in &iface.functions {
            assert!(
                md.contains(&f.name),
                "`{}::{}` is missing from the reference",
                iface.name,
                f.name
            );
        }
    }
}

/// **KNOWN GAP, asserted so it is a fact rather than an impression.**
///
/// The catalog carries interfaces and their FUNCTIONS. It does not carry type
/// definitions — records, variants, enums — so `types.wit`, which is the
/// largest file in the package and holds every payload shape a guest actually
/// constructs (`raw-candidate`, `effect`, `open-synthetic-buffer-payload`),
/// renders as an interface with no functions and no detail.
///
/// That is a real hole in a reference aimed at plugin authors: knowing that
/// `apply-action` exists does not tell you what an `effect` may be. Extending
/// `build.rs` to parse type definitions is the fix (PI.7).
///
/// This test pins the CURRENT boundary. When PI.7 lands it will fail, which is
/// the intent — the reference will then cover types and this assertion becomes
/// the wrong way round.
#[test]
fn pi7_type_definitions_are_not_in_the_reference_yet() {
    let md = lattice_plugin_api::render::markdown();
    assert!(
        !md.contains("display-spans"),
        "PI.7 appears to have landed: record fields are in the reference now, \
         so this test should be inverted into one that requires them"
    );
}
