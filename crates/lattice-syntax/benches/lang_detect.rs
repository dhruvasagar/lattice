#![allow(clippy::unwrap_used, clippy::panic)]
//! LG.2 — the cost of `Lang::detect_from_path` once plugin languages
//! can exist.
//!
//! Design:
//! [`plugin-languages.md`](../../../docs/dev/architecture/plugin-languages.md) §2.3.
//!
//! Detection used to be a pure `match` over a string. LG.2 adds a
//! fallthrough that consults a runtime registry, and the function has
//! nineteen call sites — including magit's diff highlighting, which
//! calls it **per hunk**, and grep highlighting, which calls it per
//! result. So "it got a bit slower" is not something to find out later
//! from a user with a 2000-hunk diff.
//!
//! Three cases, because they fail differently:
//!
//! * **native** — `main.rs`. Must be untouched: it returns from a native
//!   arm before the registry is reached at all.
//! * **unmatched, empty registry** — the common case in a session with
//!   no language plugins loaded. Guarded by one relaxed atomic load, so
//!   this is the measurement that says whether "free" was accurate.
//! * **unmatched / plugin, populated registry** — what a session with a
//!   language plugin actually pays: an `ArcSwap` load plus a hash lookup.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use std::path::PathBuf;

use lattice_syntax::Lang;
use lattice_syntax::plugin_lang;

fn bench(c: &mut Criterion) {
    let native = PathBuf::from("src/main.rs");
    let unmatched = PathBuf::from("notes.lg2bench-unmatched");
    let plugin_path = PathBuf::from("notes.lg2benchext");

    let mut g = c.benchmark_group("lang_detect");

    // Registry empty. Nothing else in this bench binary registers, so
    // this really is the empty path.
    g.bench_function("native/empty_registry", |b| {
        b.iter(|| black_box(Lang::detect_from_path(Some(black_box(native.as_path())))));
    });
    g.bench_function("unmatched/empty_registry", |b| {
        b.iter(|| black_box(Lang::detect_from_path(Some(black_box(unmatched.as_path())))));
    });

    // Now with a language registered, so the atomic guard no longer
    // short-circuits and every unmatched extension pays the lookup.
    let name = plugin_lang::register("lg2bench", &["lg2benchext"], 1).expect("register");
    assert_eq!(
        Lang::detect_from_path(Some(plugin_path.as_path())),
        Lang::Plugin(name)
    );

    g.bench_function("native/populated_registry", |b| {
        b.iter(|| black_box(Lang::detect_from_path(Some(black_box(native.as_path())))));
    });
    g.bench_function("unmatched/populated_registry", |b| {
        b.iter(|| black_box(Lang::detect_from_path(Some(black_box(unmatched.as_path())))));
    });
    g.bench_function("plugin/populated_registry", |b| {
        b.iter(|| {
            black_box(Lang::detect_from_path(Some(black_box(
                plugin_path.as_path(),
            ))))
        });
    });

    // LG.3a: `LangRegistry::standard()` stopped being a `OnceLock` read
    // and became an `ArcSwap` snapshot of the live registry, so plugin
    // languages land in the same map bundled ones do. Both shapes are one
    // atomic RMW, but that was a claim — magit's hunk highlighting calls
    // this per hunk, so it is measured rather than asserted.
    g.bench_function("registry_snapshot", |b| {
        b.iter(|| black_box(lattice_syntax::LangRegistry::standard().unwrap()));
    });

    g.finish();
    plugin_lang::unregister_plugin(1);
}

criterion_group!(benches, bench);
criterion_main!(benches);
