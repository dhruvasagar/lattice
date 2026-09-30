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
//!
//! AD.2: the reference is a SET of files — an index, one page per seam, and
//! `plugin-api.json`. The set is checked as a whole: a stale file, a missing
//! file, and a leftover page for a seam that no longer exists all fail.

use std::collections::BTreeSet;
use std::path::PathBuf;

fn reference_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/dev/reference")
}

const REGENERATE: &str = "Regenerate with:\n  \
     UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api";

/// The per-seam page files currently on disk, relative to `reference_dir()`.
fn seam_pages_on_disk() -> BTreeSet<String> {
    let dir = reference_dir().join("plugin-api");
    std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.file_name().into_string().ok())
                .map(|name| format!("plugin-api/{name}"))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn the_site_reference_matches_the_wit_package() {
    let pages = lattice_plugin_api::render::pages();
    let expected: BTreeSet<String> = pages.iter().map(|(p, _)| p.clone()).collect();
    // A seam page with no seam behind it: the interface was removed or
    // renamed. Left on disk it would keep documenting an API that is gone.
    let orphans: Vec<String> = seam_pages_on_disk()
        .into_iter()
        .filter(|p| !expected.contains(p))
        .collect();

    if std::env::var_os("UPDATE_SITE_REFERENCE").is_some() {
        std::fs::create_dir_all(reference_dir().join("plugin-api"))
            .expect("create the reference directory");
        for (rel, content) in &pages {
            std::fs::write(reference_dir().join(rel), content).expect("write the reference");
        }
        for rel in &orphans {
            std::fs::remove_file(reference_dir().join(rel)).expect("remove an orphaned page");
        }
        return;
    }

    let mut problems = Vec::new();
    for (rel, content) in &pages {
        match std::fs::read_to_string(reference_dir().join(rel)) {
            Ok(on_disk) if on_disk == *content => {}
            Ok(_) => problems.push(format!("stale:    {rel}")),
            Err(_) => problems.push(format!("missing:  {rel}")),
        }
    }
    problems.extend(orphans.iter().map(|rel| format!("orphaned: {rel}")));
    assert!(
        problems.is_empty(),
        "\nthe plugin-API reference disagrees with `wit/`:\n  {}\n{REGENERATE}\n",
        problems.join("\n  ")
    );
}

/// Every link the generator writes lands on a page it also wrote, at a
/// heading that exists. The site build validates links too, but only at
/// deploy time; this fails at `cargo test`, next to the change that broke it.
#[test]
fn every_generated_link_resolves() {
    let pages = lattice_plugin_api::render::pages();
    let anchors = |content: &str| -> BTreeSet<String> {
        content
            .lines()
            .filter(|l| l.starts_with('#'))
            .map(|l| {
                l.trim_start_matches('#')
                    .trim()
                    .replace('`', "")
                    .to_lowercase()
                    .replace(' ', "-")
            })
            .collect()
    };
    let by_path: std::collections::BTreeMap<&str, BTreeSet<String>> = pages
        .iter()
        .map(|(p, c)| (p.as_str(), anchors(c)))
        .collect();

    // Markdown as a reader sees it: fenced blocks and inline code are text,
    // not links (`help.wit` documents the `[label](help:topic)` syntax in a
    // code span). Dropping every code span keeps a generated link's target —
    // "[`name`](href)" becomes "[](href)".
    let prose = |content: &str| -> String {
        let mut in_fence = false;
        content
            .lines()
            .filter(|l| {
                if l.trim_start().starts_with("```") {
                    in_fence = !in_fence;
                    return false;
                }
                !in_fence
            })
            .map(|l| l.split('`').step_by(2).collect::<Vec<_>>().join(""))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let mut checked = 0;
    for (rel, content) in &pages {
        let dir = rel.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        for link in prose(content).split("](").skip(1) {
            let Some(target) = link.split(')').next() else {
                continue;
            };
            // Out-of-reference links (the authoring guide) are the site
            // sync's to resolve; this checks the reference's own web.
            if target.starts_with("../") || target.starts_with("http") {
                continue;
            }
            let (file, anchor) = target.split_once('#').unwrap_or((target, ""));
            let path = match (file, dir) {
                ("", _) => rel.clone(),
                (f, "") => f.to_string(),
                (f, d) => format!("{d}/{f}"),
            };
            let Some(heads) = by_path.get(path.as_str()) else {
                panic!("`{rel}` links to `{target}`, which is not a generated page");
            };
            assert!(
                anchor.is_empty() || heads.contains(anchor),
                "`{rel}` links to `{target}`, but `{path}` has no heading `#{anchor}`"
            );
            checked += 1;
        }
    }
    assert!(checked > 100, "suspiciously few links checked: {checked}");
}

/// The JSON export carries the same surface as the pages. Spot-checked by
/// key rather than parsed: this crate takes no JSON dependency, and the
/// escaping itself is unit-tested in `json.rs`.
#[test]
fn the_json_export_carries_the_whole_catalog() {
    let json = lattice_plugin_api::json::to_json(lattice_plugin_api::catalog());
    let cat = lattice_plugin_api::catalog();
    assert!(json.starts_with("{\n  \"package\": \"lattice:plugin-host@"));
    for iface in &cat.interfaces {
        assert!(json.contains(&format!("\"name\": \"{}\"", iface.name)));
        for f in &iface.functions {
            assert!(
                json.contains(&format!("\"display_name\": \"{}\"", f.display_name())),
                "`{}.{}` missing from the JSON",
                iface.name,
                f.name
            );
        }
    }
    // Balanced structure — a cheap proof the writer closed what it opened.
    let opens = json.matches(['{', '[']).count();
    let closes = json.matches(['}', ']']).count();
    let in_strings = json
        .split('"')
        .skip(1)
        .step_by(2)
        .map(|s| s.matches(['{', '[']).count() as i64 - s.matches(['}', ']']).count() as i64)
        .sum::<i64>();
    assert_eq!(opens as i64 - closes as i64, in_strings, "unbalanced JSON");
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
