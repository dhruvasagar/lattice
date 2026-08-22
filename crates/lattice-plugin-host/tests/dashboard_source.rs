//! CR.4 — the plugin-contributed dashboard seam, through a real guest.
//!
//! The fixture declares three sections and renders from the passed `ctx`.
//! What each assertion proves is documented on the test.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_dashboard::{DashboardCtx, DashboardSection, LinkTarget};
use lattice_mode::CapabilitySet;
use lattice_plugin_host::dashboard_host::WasmDashboardSection;
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};
use tempfile::TempDir;

const PLUGIN_ID: &str = "dashboard-guest";

fn guest_wasm() -> Option<&'static str> {
    let path = env!("DASHBOARD_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

fn sections(dir: &TempDir) -> Option<Vec<WasmDashboardSection>> {
    let wasm = guest_wasm()?;
    let host = std::sync::Arc::new(
        PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data"))
            .expect("host builds"),
    );
    let component = host
        .compile(&std::fs::read(wasm).unwrap())
        .expect("compile dashboard fixture");
    let manifest = PluginManifest::new(PLUGIN_ID, Vec::new(), CapabilitySet::empty());
    let (_id, sections) = host
        .spawn_dashboard_sections(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::grammar(),
        )
        .expect("spawn dashboard sections");
    Some(sections)
}

fn find<'a>(all: &'a [WasmDashboardSection], id: &str) -> Option<&'a WasmDashboardSection> {
    all.iter().find(|s| s.id() == id)
}

fn ctx(nerd_fonts: bool) -> DashboardCtx {
    DashboardCtx {
        pane_width: 80,
        nerd_fonts,
        version: "9.9.9".to_string(),
    }
}

/// The reason this seam keeps a live guest instead of taking a fragment once
/// at load: the section is a FUNCTION of the ctx. Render the same section
/// twice with `ui.nerd_fonts` flipped and the output must differ — a
/// data-at-registration design could not do this at all.
#[test]
fn a_section_renders_from_the_live_ctx() {
    let dir = TempDir::new().unwrap();
    let Some(all) = sections(&dir) else {
        eprintln!("SKIP: dashboard fixture guest not built");
        return;
    };
    let recent = find(&all, "recent").expect("the recent section registered");

    let plain = recent.render(&ctx(false));
    let nerd = recent.render(&ctx(true));

    let plain_text: Vec<String> = plain.rows.iter().map(|r| r.text()).collect();
    let nerd_text: Vec<String> = nerd.rows.iter().map(|r| r.text()).collect();
    assert_ne!(
        plain_text, nerd_text,
        "the icon must follow ui.nerd_fonts, not be frozen at load"
    );
    assert!(plain_text.iter().any(|l| l.contains('\u{25c6}')));
    assert!(nerd_text.iter().any(|l| l.contains('\u{f07b}')));

    // The version crossed too, so a section can show live editor facts.
    assert!(plain_text.iter().any(|l| l.contains("9.9.9")));

    // Roles and links survive the crossing.
    let link = plain
        .rows
        .iter()
        .flat_map(|r| r.spans.iter())
        .find_map(|s| s.link.clone())
        .expect("the link span kept its target");
    assert_eq!(link, LinkTarget::Command("tutor".into()));
}

/// Section ids are deliberately NOT namespaced, unlike help topics —
/// replacing a builtin is the stated capability. So the fixture's
/// `getting-started` must arrive under that exact id.
#[test]
fn a_section_may_claim_a_builtin_id() {
    let dir = TempDir::new().unwrap();
    let Some(all) = sections(&dir) else {
        eprintln!("SKIP: dashboard fixture guest not built");
        return;
    };
    let replacement = find(&all, "getting-started").expect("claimed the builtin id verbatim");
    assert!(
        replacement
            .render(&ctx(false))
            .rows
            .iter()
            .any(|r| r.text().contains("REPLACED-BY-PLUGIN"))
    );
    // And it carries plugin provenance, which is what unload removes by.
    assert!(replacement.plugin_id().is_some());
}

/// A malformed declaration costs itself and nothing else — the fixture's
/// third section has an empty id.
#[test]
fn a_rejected_section_does_not_cost_the_load_or_its_siblings() {
    let dir = TempDir::new().unwrap();
    let Some(all) = sections(&dir) else {
        eprintln!("SKIP: dashboard fixture guest not built");
        return;
    };
    assert_eq!(
        all.len(),
        2,
        "the two well-formed sections registered: {:?}",
        all.iter().map(|s| s.id()).collect::<Vec<_>>()
    );
}

/// Each section gets its OWN instance. If they shared a store, a trap in one
/// would blank the others and their renders would serialise behind one mutex.
#[test]
fn sections_are_independent_instances() {
    let dir = TempDir::new().unwrap();
    let Some(all) = sections(&dir) else {
        eprintln!("SKIP: dashboard fixture guest not built");
        return;
    };
    let recent = find(&all, "recent").expect("recent");
    let replaced = find(&all, "getting-started").expect("getting-started");

    // Each answers for its own id — a shared instance handed the wrong id
    // would return the other's rows or nothing.
    assert!(
        recent.render(&ctx(false)).rows.len() > 1,
        "recent renders its own block"
    );
    assert!(
        replaced
            .render(&ctx(false))
            .rows
            .iter()
            .any(|r| r.text().contains("REPLACED-BY-PLUGIN")),
        "getting-started renders its own block"
    );
}

/// A section is rendered on EVERY compose, for the editor's lifetime. Fuel is
/// a per-call budget, so it has to be re-armed per call — arming once at
/// instantiate makes a section work for the first few composes and then trap
/// on exhaustion, permanently, with no user-visible cause.
///
/// Caught by `benches/dashboard_section.rs`: the render benched at 9ns, which
/// is the poisoned early-return, not a wasm call. A test that renders two or
/// three times passes against the broken version, so this one renders enough
/// to drain any plausible one-shot budget.
#[test]
fn a_section_survives_being_rendered_many_times() {
    let dir = TempDir::new().unwrap();
    let Some(all) = sections(&dir) else {
        eprintln!("SKIP: dashboard fixture guest not built");
        return;
    };
    let recent = find(&all, "recent").expect("recent");

    let first = recent.render(&ctx(false));
    assert!(!first.rows.is_empty(), "sanity: the first render works");

    for i in 0..2_000 {
        let f = recent.render(&ctx(false));
        assert_eq!(
            f.rows.len(),
            first.rows.len(),
            "section stopped rendering after {i} composes — fuel is not being \
             re-armed per call"
        );
    }
}
