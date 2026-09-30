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
    // Every function — freestanding or resource method — is documented with
    // its signature, under the name a reader calls it by.
    for iface in &cat.interfaces {
        for f in &iface.functions {
            let heading = format!("`{}`", f.display_name());
            assert!(
                md.contains(&heading) && md.contains(&f.signature()),
                "`{}::{}` or its signature is missing from the reference",
                iface.name,
                f.name
            );
        }
    }
}

/// AD.1 (was the PI.7 known-gap pin, inverted as that test asked).
///
/// The reference now carries every type a seam DEFINES — records, variants,
/// enums, flags, resources, aliases — with each member. `types.wit` is the
/// largest file in the package and holds every payload a guest constructs;
/// knowing `apply-action` exists is no use without knowing what an `effect`
/// may be.
#[test]
fn every_type_and_member_is_in_the_reference() {
    let md = lattice_plugin_api::render::markdown();
    let cat = lattice_plugin_api::catalog();
    let mut members = 0;
    for iface in &cat.interfaces {
        for t in &iface.types {
            let heading = format!("{} `{}`", t.kind.keyword(), t.name);
            assert!(
                md.contains(&heading),
                "type `{}.{}` is missing from the reference",
                iface.name,
                t.name
            );
            for m in t.kind.members() {
                members += 1;
                assert!(
                    md.contains(&format!("    {}", m.name)),
                    "member `{}.{}.{}` is missing from the reference",
                    iface.name,
                    t.name,
                    m.name
                );
            }
        }
    }
    // The record the PI.7 pin named, by field: a regression that dropped
    // members wholesale would otherwise pass on the heading checks alone.
    assert!(md.contains("display-spans"), "record fields are rendered");
    assert!(members > 200, "suspiciously few members: {members}");
}
