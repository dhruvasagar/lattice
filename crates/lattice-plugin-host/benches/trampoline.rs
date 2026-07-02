//! PH7.3d end-to-end guest↔host typed-call bench.
//!
//! The §7 headline gate — "typed host function call < 100ns p50 / < 500ns p99"
//! — deferred from PH7.3a (which benched only the marshalling component) to
//! here, where a real guest export exists to call. This measures the WARM
//! typed call: the guest is instantiated once, then `apply-effect` /
//! `next-batch` are called in a tight loop, so the number is the per-call
//! overhead (canonical-ABI lift/lower + wasm execution + the return), NOT
//! instantiation. Skips when the `wasm32-wasip2` fixture wasn't built (see
//! build.rs). Not a CI-gated ratchet yet (that is PH7.5); this is the
//! descriptive baseline.
//!
//! A standalone `bindgen!` (own generated types) — a bench is a separate crate
//! and can't reach the lib's internal `types` module; it doesn't need to (it
//! measures the call, not the native round-trip).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use wasmtime::component::{Component, Linker};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    world: "trampoline-fixture",
    path: "../../wit",
});

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

fn typed_call(c: &mut Criterion) {
    let path = env!("TRAMPOLINE_GUEST_WASM");
    if path.is_empty() {
        eprintln!("SKIP: trampoline bench — fixture guest not built (add wasm32-wasip2)");
        return;
    }

    let engine = Engine::new(wasmtime::Config::new().wasm_component_model(true)).expect("engine");
    let component = Component::from_file(&engine, path).expect("load component");
    let mut linker: Linker<State> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).expect("add wasi");
    let mut store = Store::new(
        &engine,
        State {
            wasi: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
        },
    );
    let guest =
        TrampolineFixture::instantiate(&mut store, &component, &linker).expect("instantiate");

    // §4.1 warm typed call: args in → list<effect> out.
    let arg = Args::String("hello".to_string());
    c.bench_function("trampoline_apply_effect_warm_call", |b| {
        b.iter(|| {
            let out = guest
                .call_apply_effect(&mut store, black_box(&arg))
                .expect("apply-effect");
            black_box(out);
        })
    });

    // §4.3 warm typed call: one batch pull.
    c.bench_function("trampoline_next_batch_warm_call", |b| {
        b.iter(|| {
            let out = guest.call_next_batch(&mut store).expect("next-batch");
            black_box(out);
        })
    });
}

criterion_group!(benches, typed_call);
criterion_main!(benches);
