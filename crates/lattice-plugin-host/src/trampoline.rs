//! The §4.1 trampoline + §4.3 result-carrier (plugin-host.md, PH7.3d).
//!
//! §4.1 — closures cannot cross the boundary, so a contribution's `apply`
//! becomes "the guest *exports* a function; the host stores `(id → export)` and
//! calls it by id, projecting the context in and mapping the returned WIT
//! `effect` back to native." The *production* trampoline (the shim closure the
//! grammar/picker dispatcher calls) is world-specific and lands with the
//! `grammar` / `picker-source` worlds (PH7.4/7.7); this slice proves the
//! mechanism end-to-end against a minimal `wasm32-wasip2` fixture guest — a real
//! guest↔host canonical-ABI call — retiring §14's highest risk (the whole
//! `effect` mirror actually crosses).
//!
//! §4.3 — a plugin's `Future`/`Stream` result cannot cross either. The carrier
//! re-expresses it as "guest returns batches; host owns the loop / `Future` /
//! `mpsc`." [`collect_batches`] is that host-owned loop: it pulls batches from a
//! (world-specific) guest export until one comes back empty. The guest never
//! names a tokio type.

/// Drive a batch-returning guest export to exhaustion, aggregating every batch
/// into one owned `Vec` (§4.3). `next` wraps the guest's `next-batch`-style
/// call; an **empty** batch is the exhausted sentinel. The host owns this loop —
/// the guest only ever returns data. A `next` that errors aborts the drive with
/// that error (a trapped/fuel-exhausted batch call never yields a partial-but-
/// silent result).
pub fn collect_batches<T, E>(mut next: impl FnMut() -> Result<Vec<T>, E>) -> Result<Vec<T>, E> {
    let mut all = Vec::new();
    loop {
        let batch = next()?;
        if batch.is_empty() {
            return Ok(all);
        }
        all.extend(batch);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn collect_batches_aggregates_until_empty() {
        let batches = [vec![1, 2], vec![3], vec![]];
        let mut i = 0;
        let out = collect_batches(|| {
            let b = batches[i].clone();
            i += 1;
            Ok::<_, ()>(b)
        })
        .unwrap();
        assert_eq!(out, vec![1, 2, 3]);
    }

    #[test]
    fn collect_batches_propagates_an_error() {
        let mut calls = 0;
        let out: Result<Vec<i32>, &str> = collect_batches(|| {
            calls += 1;
            if calls == 2 { Err("boom") } else { Ok(vec![1]) }
        });
        assert_eq!(out, Err("boom"));
    }
}

/// The real guest↔host trampoline proof (§4.1 + §4.3) against the
/// `wasm32-wasip2` fixture. A unit-test module (not an integration test) so its
/// second `bindgen!` can **reuse** the host's already-generated `types` — the
/// guest-returned `Effect` is then the *same* Rust type `WitBoundary::from_wit`
/// consumes, so the round-trip maps back to native. Skips (not fails) when the
/// fixture wasn't built (no `wasm32-wasip2` target — see `build.rs`).
#[cfg(test)]
mod fixture {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::collect_batches;
    use crate::WitBoundary;
    use lattice_grammar::effect::Effect as NativeEffect;
    use wasmtime::component::{Component, Linker};
    use wasmtime::{Engine, Store};
    use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

    // Second bindgen for the fixture world. `with` reuses the host's generated
    // `types` module (from the `plugin`-world bindgen in `lib.rs`) so the
    // guest-returned `effect`/`args` are the SAME Rust types the host boundary
    // round-trips — not a fresh, incompatible copy. Sync exports: the test
    // drives the calls directly without a tokio runtime.
    wasmtime::component::bindgen!({
        world: "trampoline-fixture",
        path: "../../wit",
        with: {
            "lattice:plugin-host/types": crate::lattice::plugin_host::types,
        },
    });

    /// Store state for the fixture guest (it imports WASI as a `wasm32-wasip2`
    /// component).
    struct FixtureState {
        wasi: WasiCtx,
        table: ResourceTable,
    }
    impl WasiView for FixtureState {
        fn ctx(&mut self) -> WasiCtxView<'_> {
            WasiCtxView {
                ctx: &mut self.wasi,
                table: &mut self.table,
            }
        }
    }

    /// Instantiate the fixture, or `None` when it wasn't built.
    fn instantiate() -> Option<(Store<FixtureState>, TrampolineFixture)> {
        let path = env!("TRAMPOLINE_GUEST_WASM");
        if path.is_empty() {
            eprintln!("SKIP: trampoline fixture guest not built (add the wasm32-wasip2 target)");
            return None;
        }
        let engine = Engine::new(wasmtime::Config::new().wasm_component_model(true)).unwrap();
        let component = Component::from_file(&engine, path).unwrap();
        let mut linker: Linker<FixtureState> = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).unwrap();
        let mut store = Store::new(
            &engine,
            FixtureState {
                wasi: WasiCtxBuilder::new().build(),
                table: ResourceTable::new(),
            },
        );
        let bindings = TrampolineFixture::instantiate(&mut store, &component, &linker).unwrap();
        Some((store, bindings))
    }

    /// §4.1: a `String` arg flows in and comes back as an `Effect::Echo` — the
    /// whole context→WIT→guest→effect→native path, through a real guest.
    #[test]
    fn apply_effect_round_trips_a_payload_arm_through_the_guest() {
        let Some((mut store, guest)) = instantiate() else {
            return;
        };
        let wit = guest
            .call_apply_effect(&mut store, &Args::String("hi".to_string()))
            .unwrap();
        let native = NativeEffect::from_wit(wit).expect("from_wit");
        match native {
            NativeEffect::Echo { level, text } => {
                assert_eq!(text, "hi");
                assert_eq!(level, lattice_grammar::effect::EchoLevel::Info);
            }
            other => panic!("expected Echo, got {other:?}"),
        }
    }

    /// §4.1: a non-string arg returns a two-effect list → the host rebuilds
    /// `Effect::Many` (the `list<effect>` seam) through a real guest.
    #[test]
    fn apply_effect_rebuilds_many_from_a_list() {
        let Some((mut store, guest)) = instantiate() else {
            return;
        };
        let wit = guest.call_apply_effect(&mut store, &Args::None).unwrap();
        let native = NativeEffect::from_wit(wit).expect("from_wit");
        match native {
            NativeEffect::Many(list) => {
                assert_eq!(list.len(), 2);
                assert!(matches!(list[0], NativeEffect::RecordJump));
                assert!(matches!(list[1], NativeEffect::SetColorscheme(ref s) if s == "nord"));
            }
            other => panic!("expected Many, got {other:?}"),
        }
    }

    /// §4.3: the host drives `next-batch` to exhaustion, owning the loop; the
    /// guest just returns batches (["a","b"], ["c"], []).
    #[test]
    fn next_batch_carrier_aggregates_host_side() {
        let Some((mut store, guest)) = instantiate() else {
            return;
        };
        let all = collect_batches(|| guest.call_next_batch(&mut store)).unwrap();
        assert_eq!(all, vec!["a", "b", "c"]);
    }
}
