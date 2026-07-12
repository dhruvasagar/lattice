//! PH7.5 — the plugin-host perf ratchets (the CI gate on the §7 budgets).
//!
//! Companions to the criterion benches (`trampoline`, `fuzzy_finder`,
//! `instantiate`), which record the descriptive numbers on `main`. These are
//! the CI GATE: each measures a warm operation inline and asserts a **generous
//! absolute ceiling** — orders of magnitude above the real (release) cost, so it
//! catches a *gross* regression (an O(file) term, a boundary blowup, a lost
//! module cache) without tripping on the ~20% bench variance of GitHub runners
//! or the inflation of debug builds (these run under `cargo test`, i.e. debug).
//! Mirrors `lattice-host/tests/keystroke_publish_ratchet.rs`.
//!
//! Only the EXERCISED §7 rows are gated here (typed host call, the guest→host
//! picker path, cold-start). The forward-looking rows (grammar round-trip,
//! status/gutter segment, picker-filter-per-item, major-mode event) map to
//! seams that don't exist yet (PH7.6–7.11); each lands its own ratchet with its
//! seam. Skips cleanly when the `wasm32-wasip2` guests weren't built (CI installs
//! the target so the gate runs there — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::time::{Duration, Instant};

use lattice_mode::CapabilitySet;
use lattice_plugin_host::picker_task::{ActiveBufferSnapshot, PickerContext, Position};
use lattice_plugin_host::{Capability, PluginBudget, PluginHost, PluginManifest, TrustTier};

/// The median of `samples`, sorted in place.
fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

// ── §7 row: typed host function call (< 100ns p50 / < 500ns p99 release) ──────
// Measured on the PH7.3d trampoline fixture (the pure canonical-ABI typed call,
// no walk / no marshalling of large payloads). A sync bindgen — the trampoline
// world's exports are sync, so this is a plain loop with no runtime.
mod trampoline {
    wasmtime::component::bindgen!({
        world: "trampoline-fixture",
        path: "../../wit",
    });
}

#[test]
fn typed_call_stays_within_ceiling() {
    use wasmtime::component::{Component, Linker};
    use wasmtime::{Engine, Store};
    use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

    let path = env!("TRAMPOLINE_GUEST_WASM");
    if path.is_empty() {
        eprintln!("SKIP: typed_call ratchet — trampoline fixture not built");
        return;
    }

    struct State {
        wasi: WasiCtx,
        table: ResourceTable,
    }
    impl WasiView for State {
        fn ctx(&mut self) -> WasiCtxView<'_> {
            WasiCtxView {
                ctx: &mut self.wasi,
                table: &mut self.table,
            }
        }
    }

    let engine = Engine::new(wasmtime::Config::new().wasm_component_model(true)).expect("engine");
    let component = Component::from_file(&engine, path).expect("load component");
    let mut linker: Linker<State> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).expect("wasi");
    let mut store = Store::new(
        &engine,
        State {
            wasi: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
        },
    );
    let guest = trampoline::TrampolineFixture::instantiate(&mut store, &component, &linker)
        .expect("instantiate");
    let arg = trampoline::Args::String("hello".to_string());

    // Warm, then measure the median typed call.
    for _ in 0..100 {
        let _ = guest.call_apply_effect(&mut store, &arg).unwrap();
    }
    let iters = 2_000usize;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        let out = guest.call_apply_effect(&mut store, &arg).unwrap();
        samples.push(t.elapsed());
        std::hint::black_box(out);
    }
    let m = median(samples);
    eprintln!("[ph7.5-ratchet] typed_call median (debug): {m:?}");

    // Release p99 budget is < 500ns (§7). Debug inflates the canonical-ABI
    // lift/lower; 50µs is orders of magnitude above the real cost yet far under
    // what a marshalling blowup would produce.
    assert!(
        m < Duration::from_micros(50),
        "typed host call median was {m:?}; expected < 50µs (§7 release budget < 500ns p99). \
         A gross regression in the canonical-ABI typed-call path."
    );
}

// ── §7 row: guest→host picker path (the fuzzy-finder init round-trip) ─────────

fn wit_ctx(workspace_root: &str) -> PickerContext {
    PickerContext {
        active_buffer: ActiveBufferSnapshot {
            buffer_id: 0,
            path: None,
            language: None,
            cursor: Position { line: 0, byte: 0 },
            selection: None,
            syntax_symbols: Vec::new(),
        },
        workspace_root: workspace_root.to_string(),
        recent_files: Vec::new(),
        position_history: Vec::new(),
        buffers: Vec::new(),
        marks: Vec::new(),
        registers: Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn picker_init_round_trip_stays_within_ceiling() {
    let path = env!("FUZZY_FINDER_WASM");
    if path.is_empty() {
        eprintln!("SKIP: picker_init ratchet — fuzzy-finder plugin not built");
        return;
    }
    // A small fixed tree so the walk is bounded — we gate the CALL path (channel
    // + guest export + walk round-trip + marshalling), not disk throughput.
    let tree = tempfile::tempdir().unwrap();
    for i in 0..10 {
        std::fs::write(tree.path().join(format!("f{i}.rs")), "").unwrap();
    }
    let root = std::fs::canonicalize(tree.path()).unwrap();
    let root_str = root.to_str().unwrap().to_string();

    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();
    let manifest = PluginManifest::new(
        "fuzzy-finder",
        vec![Capability::FsRead(root.clone())],
        CapabilitySet::empty(),
    );
    let (client, actor) = host
        .spawn_picker_source(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::default(),
        )
        .await
        .unwrap();
    tokio::spawn(actor.run());

    let ctx = wit_ctx(&root_str);
    // Warm, then measure the median init round-trip.
    for _ in 0..20 {
        let _ = client
            .init(ctx.clone(), vec![root_str.clone()])
            .await
            .unwrap();
    }
    let iters = 200usize;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        let out = client
            .init(ctx.clone(), vec![root_str.clone()])
            .await
            .unwrap()
            .unwrap();
        samples.push(t.elapsed());
        std::hint::black_box(out);
    }
    let m = median(samples);
    eprintln!("[ph7.5-ratchet] picker_init median (10 files, debug): {m:?}");

    // Release warm init over 50 files ≈ 110µs; a 10-file tree is less. 20ms is
    // far above the real cost yet well under an O(file)-blowup or a lost-cache
    // re-instantiation.
    assert!(
        m < Duration::from_millis(20),
        "picker init round-trip median was {m:?}; expected < 20ms. \
         A gross regression in the guest→host picker path (channel / walk / marshalling)."
    );
}

// ── §7 row: cold-start (50 lazily-loaded plugins < 30ms release) ──────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_start_50_instantiations_stays_within_ceiling() {
    // The no-op WAT component (no wasm target needed) — instantiation cost is
    // linear-memory alloc + import resolution, the per-plugin cold-start unit.
    let bytes = wat::parse_str(include_str!("fixtures/noop.wat")).expect("noop assembles");
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let component = host.compile(&bytes).expect("compile"); // AOT once; cached.

    // Warm the code path.
    let _ = host.instantiate(&component).await.unwrap();

    let n = 50usize;
    let t = Instant::now();
    for _ in 0..n {
        let _ = host.instantiate(&component).await.unwrap();
    }
    let total = t.elapsed();
    eprintln!("[ph7.5-ratchet] cold-start {n} instantiations (debug): {total:?}");

    // §7 release budget: 50 plugins < 30ms. Debug + no real AOT-cache hit on the
    // first compile (done above, outside the loop) inflates it; 2s is far above
    // the real cost yet catches a per-instantiation blowup.
    assert!(
        total < Duration::from_secs(2),
        "cold-start of {n} instantiations was {total:?}; expected < 2s \
         (§7 release budget: 50 plugins < 30ms). A per-instantiation regression."
    );
}
