//! CR.4 bench: the cost of a plugin dashboard section's render.
//!
//! CR.4 put a synchronous guest call on the actor thread, inside
//! `Editor::compose_dashboard_sections`. The design fragment
//! (`contributable-registries.md` §3.2) argues that is acceptable because
//! composition is a `LatencyClass::Display` action — `:dashboard`, startup, or
//! a DB.6 option-change recompose — never per-keystroke and never per-frame.
//!
//! An argument is not a measurement. This is the measurement, so the claim
//! lives in `benchmarks.md` as a number that CI can watch rather than as a
//! paragraph nobody can falsify.
//!
//! Two numbers:
//!
//! - `dashboard_section_render_ns` — one `render(&ctx)` on a live plugin
//!   section. This is what a compose pays PER plugin section. Compare against
//!   `dashboard_creation` in the DB.7 benches (~571 µs for the whole page):
//!   the question is whether a plugin section is a rounding error against the
//!   compose it joins, not whether it is fast in the abstract.
//! - `dashboard_section_spawn_ns` — declaring + instantiating the sections at
//!   load. Paid once, on the loader's off-boot-thread task, and recorded so a
//!   regression there is visible too (a plugin must not delay boot).
//!
//! Skips — reporting a zero-work bench rather than failing — when the fixture
//! guest was not built (no `wasm32-wasip2` target).

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use lattice_dashboard::{DashboardCtx, DashboardSection};
use lattice_mode::CapabilitySet;
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};

fn guest_wasm() -> Option<&'static str> {
    let path = env!("DASHBOARD_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

fn ctx() -> DashboardCtx {
    DashboardCtx {
        pane_width: 80,
        nerd_fonts: false,
        version: "0.1.0".to_string(),
    }
}

fn bench_dashboard_section(c: &mut Criterion) {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: dashboard-guest fixture not built; skipping CR.4 benches");
        return;
    };
    let Ok(bytes) = std::fs::read(wasm) else {
        eprintln!("SKIP: dashboard-guest wasm unreadable");
        return;
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let host = std::sync::Arc::new(
        PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data"))
            .expect("host builds"),
    );
    let component = host.compile(&bytes).expect("compile dashboard fixture");
    let manifest = PluginManifest::new("dashboard-guest", Vec::new(), CapabilitySet::empty());

    // Load-time: declare + instantiate one guest per section.
    c.bench_function("dashboard_section_spawn_ns", |b| {
        b.iter(|| {
            let (_id, sections) = host
                .spawn_dashboard_sections(
                    &component,
                    &manifest,
                    TrustTier::Bundled,
                    PluginBudget::grammar(),
                )
                .expect("spawn");
            black_box(sections.len())
        });
    });

    // Compose-time: the per-section guest call the actor thread pays.
    let (_id, sections) = host
        .spawn_dashboard_sections(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::grammar(),
        )
        .expect("spawn");
    let section = sections
        .iter()
        .find(|s| s.id() == "recent")
        .expect("the recent section");
    let ctx = ctx();
    c.bench_function("dashboard_section_render_ns", |b| {
        b.iter(|| black_box(section.render(&ctx).rows.len()));
    });
}

criterion_group!(benches, bench_dashboard_section);
criterion_main!(benches);
