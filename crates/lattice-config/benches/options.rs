#![allow(clippy::unwrap_used)]
//! Benches for the typed-options hot path (DESIGN.md §5.12).
//!
//! Measures the cost of the renderer-side reads that replaced
//! direct `App.foldmethod` / `.tabstop` field accesses in the
//! pre-typed-options era. Each `config.get(handle)` path is:
//!
//! 1. brief mutex acquire on the registry's `Inner` (to clone the
//!    `Arc<dyn ErasedOption>`),
//! 2. mutex release,
//! 3. `Arc::as_any().downcast_ref::<Option<T>>()` (compile-time
//!    monomorphic; one type id compare),
//! 4. `ArcSwap::load_full()` returning `Arc<T>`,
//! 5. caller's deref of the `Arc<T>`.
//!
//! Steady-state target (per DESIGN.md §8.2): comfortably under
//! 100ns per read so the renderer can poll a handful of options
//! per frame without registering on the budget.

use std::sync::Arc;

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use lattice_config::option::{Option, OptionHandle};
use lattice_config::{ConfigRegistry, EventPublisher};

fn bench_typed_get(c: &mut Criterion) {
    let registry = ConfigRegistry::new();
    let h: OptionHandle<bool> = registry.register(Option::new("number", true, ""));
    c.bench_function("config::get_bool_via_handle", |b| {
        b.iter(|| {
            let v = registry.get(black_box(h));
            black_box(*v)
        })
    });
}

fn bench_typed_with(c: &mut Criterion) {
    // `with` skips one Arc::clone vs `get` -- closure-style read
    // useful for one-shot accesses inside a render hot path.
    let registry = ConfigRegistry::new();
    let h: OptionHandle<i64> = registry.register(Option::new("tabstop", 8, ""));
    c.bench_function("config::with_int_via_handle", |b| {
        b.iter(|| registry.with(black_box(h), |v| black_box(*v)))
    });
}

fn bench_lookup_by_name(c: &mut Criterion) {
    // The cmdline `:set foo=bar` path resolves by name. Slower than
    // typed handle access (HashMap probe + Arc clone) but we don't
    // expect this on the per-frame render path.
    let registry = ConfigRegistry::new();
    registry.register(Option::<bool>::new("number", true, ""));
    c.bench_function("config::lookup_by_name", |b| {
        b.iter(|| {
            let opt = registry.lookup(black_box("number")).unwrap();
            black_box(opt.get_formatted())
        })
    });
}

fn bench_set_with_publisher_noop(c: &mut Criterion) {
    // Cost of a typed write when no event publisher is wired.
    // Dominated by the registry's brief mutex + ArcSwap::store.
    let registry = ConfigRegistry::new();
    let h: OptionHandle<bool> = registry.register(Option::new("number", true, ""));
    c.bench_function("config::set_no_publisher", |b| {
        let mut on = false;
        b.iter(|| {
            on = !on;
            registry.set(black_box(h), black_box(on)).unwrap()
        })
    });
}

fn bench_set_with_publisher(c: &mut Criterion) {
    // Cost of a typed write WITH a no-op publisher closure (the
    // shape the App wires at boot, where the closure pushes the
    // event to the §5.10 bus). Measures the publish overhead per
    // set; subscribers consume the channel asynchronously and
    // don't register on this measurement.
    let registry = ConfigRegistry::new();
    let h: OptionHandle<bool> = registry.register(Option::new("number", true, ""));
    let publisher: EventPublisher = Arc::new(|_event| {
        // No-op: just check the publisher overhead, not the bus.
    });
    registry.set_event_publisher(publisher);
    c.bench_function("config::set_with_publisher", |b| {
        let mut on = false;
        b.iter(|| {
            on = !on;
            registry.set(black_box(h), black_box(on)).unwrap()
        })
    });
}

fn bench_resolved_options_get(c: &mut Criterion) {
    // M.2.1: type-keyed read against a populated
    // ResolvedOptions cache. The §6.3.2 perf gate is p99 < 50ns.
    use lattice_config::{ResolvedOptions, Tabstop};
    let registry = ConfigRegistry::new();
    registry.init_from_linkme();
    let mut resolved = ResolvedOptions::new();
    registry.bootstrap_resolved_with_current_values(&mut resolved);
    c.bench_function("config::resolved_get_typed", |b| {
        b.iter(|| {
            let v = resolved.get::<Tabstop>();
            black_box(*v.unwrap())
        })
    });
}

fn bench_resolver_recompute(c: &mut Criterion) {
    // M.2.1: end-to-end recompute. Bootstrap the cache with
    // current registry values, then resolve a layered chain
    // representing 10 active minor modes' worth of overrides.
    // Per §6.3.2 the perf gate is p99 < 10us at 10 minors.
    use lattice_config::{OptionOverride, OptionOverrideSet, ResolvedOptions, Resolver, Tabstop};
    use std::any::TypeId;

    let registry = ConfigRegistry::new();
    registry.init_from_linkme();

    // Build 10 layers, each contributing one Tabstop override.
    // Real modes contribute 0-4 overrides each typically; this
    // is intentionally heavier to stress the merge walk.
    let layers: Vec<OptionOverrideSet> = (0..10)
        .map(|i| {
            let mut set = OptionOverrideSet::new();
            set.push(OptionOverride::new(TypeId::of::<Tabstop>(), (i + 1) as i64));
            set
        })
        .collect();

    let resolver = Resolver::new();

    c.bench_function("config::resolve_into_10_layers", |b| {
        b.iter(|| {
            let mut out = ResolvedOptions::new();
            registry.bootstrap_resolved_with_current_values(&mut out);
            resolver.resolve_into(layers.iter(), &mut out);
            black_box(out)
        })
    });
}

fn bench_parse_and_set_command(c: &mut Criterion) {
    // The cmdline path's full cost: parse_set + lookup + parse_and_set
    // + format echo + publish. Hottest write path the user can
    // trigger today.
    let registry = ConfigRegistry::new();
    registry.register(Option::<bool>::new("number", true, ""));
    c.bench_function("config::parse_and_set_command_bool", |b| {
        let mut on = false;
        b.iter(|| {
            on = !on;
            let cmd = if on { "number" } else { "nonumber" };
            registry.parse_and_set_command(black_box(cmd)).unwrap()
        })
    });
}

criterion_group!(
    benches,
    bench_typed_get,
    bench_typed_with,
    bench_lookup_by_name,
    bench_set_with_publisher_noop,
    bench_set_with_publisher,
    bench_parse_and_set_command,
    bench_resolved_options_get,
    bench_resolver_recompute,
);
criterion_main!(benches);
