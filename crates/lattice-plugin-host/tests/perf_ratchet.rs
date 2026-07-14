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
//! picker path, cold-start, the grammar-extension round-trip (PH7.7), the
//! event-handler delivery path (PH7.8), **and the status/gutter segment update**
//! now the decoration seam exists, PH7.9). The still-forward-looking rows
//! (picker-filter-per-item, major-mode event handler beyond delivery) map to
//! seams not fully exercised yet; each lands its own ratchet with its seam. Skips
//! cleanly when the `wasm32-wasip2` guests weren't built (CI installs the target
//! so the gate runs there — see build.rs).

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
            &std::sync::Arc::new(lattice_runtime::EventBus::new()),
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

// ── §7 row: grammar-extension round-trip (< 1µs p50 / < 5µs p99 release) ──────
// The end-to-end SYNC path a plugin motion pays on every dispatch (the PH7.7
// fork): `execute_motion_only` → project the `MotionContext` → the sync guest
// `apply-motion` call (canonical-ABI lift/lower, no runtime) → `MotionResult::
// from_wit`. Measured through the `grammar-guest` fixture's `down-n` motion,
// registered into a real `CommandRegistry` and dispatched exactly as a builtin.

#[test]
fn grammar_round_trip_stays_within_ceiling() {
    use lattice_core::buffer::Buffer;
    use lattice_core::buffers::BufferId;
    use lattice_grammar::CancellationToken;
    use lattice_grammar::command::{CommandInvocation, Count};
    use lattice_grammar::dispatcher::execute_motion_only;
    use lattice_grammar::registry::{CommandRegistry, TextObjectEnv};
    use lattice_protocol::position::Position as GrammarPos;

    let path = env!("GRAMMAR_GUEST_WASM");
    if path.is_empty() {
        eprintln!("SKIP: grammar round-trip ratchet — grammar fixture not built");
        return;
    }

    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();
    let manifest = PluginManifest::new("grammar-fixture", Vec::new(), CapabilitySet::empty());
    let set = host
        .instantiate_grammar_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            &std::sync::Arc::new(lattice_runtime::EventBus::new()),
        )
        .unwrap();
    // Leak the host so its engine + epoch ticker outlive the dispatched closures
    // (the trampoline holds the guest store for the plugin's life).
    Box::leak(Box::new(host));

    let mut registry = CommandRegistry::new();
    set.register_all(&mut registry);
    let motion_id = registry
        .id_by_name("down-n")
        .expect("fixture motion registered");

    let buffer = Buffer::from_text("l0\nl1\nl2\nl3\nl4\nl5\n");
    let cancel = CancellationToken::never();
    let cursor = GrammarPos { line: 1, byte: 0 };
    let invocation = CommandInvocation::of(motion_id).with_count(Count(3));

    // Warm, then measure the median sync round-trip.
    for _ in 0..100 {
        let _ = execute_motion_only(
            &registry,
            &buffer,
            BufferId(1),
            cursor,
            invocation.clone(),
            &cancel,
            TextObjectEnv::default(),
        )
        .unwrap();
    }
    let iters = 2_000usize;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        let out = execute_motion_only(
            &registry,
            &buffer,
            BufferId(1),
            cursor,
            invocation.clone(),
            &cancel,
            TextObjectEnv::default(),
        )
        .unwrap();
        samples.push(t.elapsed());
        std::hint::black_box(out);
    }
    let m = median(samples);
    eprintln!("[ph7.5-ratchet] grammar_round_trip median (debug): {m:?}");

    // §7 release budget is < 5µs p99. Debug inflates the canonical-ABI lift/lower
    // + the context projection + `from_wit`; 250µs is orders of magnitude above
    // the real cost yet far under what a per-call blowup (a lost budget arm, an
    // O(buffer) projection, a mutex-contention regression) would produce.
    assert!(
        m < Duration::from_micros(250),
        "grammar round-trip median was {m:?}; expected < 250µs \
         (§7 release budget < 5µs p99). A gross regression in the sync grammar \
         trampoline (project → guest call → from_wit)."
    );
}

// ── §7 row: major-mode event handler (< 50µs p50 / < 250µs p99 release) ───────
// The OFF-keystroke async delivery path a plugin hook pays per event (PH7.8):
// bus sink → actor channel → guest `on-event` (project `Event` → WIT, async
// canonical-ABI call). Measured through the `events-guest` fixture's no-op
// handler 4 (`DocumentChanged`), which isolates the DISPATCH cost (no fs, no
// handler work). Per-delivery is a mean over N (the actor drains a channel; it
// consumes itself in `run`, so per-call samples aren't available — a mean over a
// large N is the honest gross-regression gate).

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_handler_stays_within_ceiling() {
    use lattice_protocol::Event;
    use lattice_protocol::ids::DocumentId;
    use lattice_runtime::EventBus;
    use std::sync::Arc;

    let path = env!("EVENTS_GUEST_WASM");
    if path.is_empty() {
        eprintln!("SKIP: event handler ratchet — events fixture not built");
        return;
    }
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();
    let manifest = PluginManifest::new("events-fixture", Vec::new(), CapabilitySet::empty());
    let bus = Arc::new(EventBus::new());
    let (sub_ids, actor) = host
        .spawn_event_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::event(),
            &bus,
        )
        .await
        .unwrap();

    // Queue N deliveries to the no-op handler (handler 4 → DocumentChanged, no
    // edits) so the measurement is the dispatch path, not handler work. Publish
    // first, then unsubscribe (closes the channel) so `run` drains exactly N and
    // returns.
    let n = 1_000u32;
    for v in 0..n {
        bus.publish(Event::DocumentChanged {
            id: DocumentId::new(1),
            path: None,
            version: v as u64,
            edits: Vec::new(),
        });
    }
    for id in sub_ids {
        bus.unsubscribe(id);
    }
    let t = Instant::now();
    actor.run().await;
    let total = t.elapsed();
    let per = total / n;
    eprintln!("[ph7.8-ratchet] event_handler mean per delivery ({n} events, debug): {per:?}");

    // §7 release budget: major-mode event handler < 250µs p99. The per-delivery
    // DISPATCH cost decomposes into event marshalling (~23ns, PH7.8a) + one async
    // guest `on-event` call (≈ the PH7.3d typed call) + a sub-µs channel hop —
    // µs-scale in debug. 2ms mean is orders of magnitude above that yet well under
    // a per-delivery re-instantiation (~200µs each, cold-start ratchet) sustained
    // across N, or an O(payload) marshalling blowup.
    assert!(
        per < Duration::from_millis(2),
        "event handler mean per-delivery was {per:?}; expected < 2ms \
         (§7 release budget < 250µs p99). A gross regression in the event \
         delivery path (bus sink → channel → guest on-event)."
    );
}

// ── §7 row: status / gutter segment update (< 10µs p50 / < 50µs p99 release) ──
// The OFF-render-path cost a decoration provider pays per trigger (PH7.9): the
// host projects the `decoration-context`, calls the guest `gutter-decorations`
// producer (async canonical-ABI), and converts the result to native. Measured
// through the `decorations-guest` fixture (a deterministic 3-decoration
// producer). Off the render path (the completion PH7.6 fork), so a slow producer
// never touches a frame — this gates the marshalling + dispatch overhead.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decoration_produce_stays_within_ceiling() {
    use lattice_plugin_host::WasmDecorationSource;

    let path = env!("DECORATIONS_GUEST_WASM");
    if path.is_empty() {
        eprintln!("SKIP: decoration produce ratchet — decorations fixture not built");
        return;
    }
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();
    let manifest = PluginManifest::new("decorations-fixture", Vec::new(), CapabilitySet::empty());
    let (client, actor) = host
        .spawn_decoration_source(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::decoration(),
            &std::sync::Arc::new(lattice_runtime::EventBus::new()),
        )
        .await
        .unwrap();
    tokio::spawn(actor.run());
    let src = WasmDecorationSource::new(client);

    // Warm, then measure the median produce round-trip (project ctx → guest
    // producer → convert; no walk).
    for _ in 0..20 {
        let _ = src.gutter_decorations(1, None, 200).await.unwrap();
    }
    let iters = 200usize;
    let mut samples = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        let out = src.gutter_decorations(1, None, 200).await.unwrap();
        samples.push(t.elapsed());
        std::hint::black_box(out);
    }
    let m = median(samples);
    eprintln!("[ph7.9-ratchet] decoration_produce median (debug): {m:?}");

    // §7 release budget: status / gutter segment update < 50µs p99. The
    // per-trigger cost decomposes into the ~ns context projection + one async
    // guest producer call (≈ the PH7.3d typed call) + per-decoration marshalling
    // (~ns, PH7.9a) + a sub-µs channel hop — µs-scale in debug. 5ms is orders of
    // magnitude above that yet well under a per-trigger re-instantiation or an
    // O(payload) blowup.
    assert!(
        m < Duration::from_millis(5),
        "decoration produce median was {m:?}; expected < 5ms \
         (§7 release budget < 50µs p99). A gross regression in the decoration \
         producer path (project → guest producer → from_wit)."
    );
}
