//! PH7.0 exit test: the host compiles + instantiates a hand-written no-op
//! component and its `activate` runs; malformed bytes are a typed error, not
//! a panic. This is the "CI builds + instantiates it" gate — it runs inside
//! the existing `cargo test --workspace` job, so no bespoke CI target is
//! needed for the scaffold.

use lattice_plugin_host::{PluginHost, PluginHostError};

/// The degenerate `init.rs` — a component that registers nothing.
const NOOP_WAT: &str = include_str!("fixtures/noop.wat");

fn noop_component_bytes() -> Vec<u8> {
    wat::parse_str(NOOP_WAT).expect("no-op component WAT assembles to component bytes")
}

#[test]
fn load_activate_deactivate_drop() {
    let host = PluginHost::new().expect("host builds");
    let bytes = noop_component_bytes();
    let component = host.compile(&bytes).expect("no-op component compiles");
    let mut plugin = host
        .instantiate(&component)
        .expect("no-op component instantiates");

    // The scaffold's whole point: the lifecycle round-trip runs end to end.
    plugin.activate().expect("activate runs");
    plugin.deactivate().expect("deactivate runs");

    // Store teardown on drop must not panic.
    drop(plugin);
}

#[test]
fn a_component_can_be_instantiated_many_times() {
    // Lazy instantiation (PH7.1) reuses one compiled component across many
    // Stores; prove the compiled artifact is reusable now so the shape holds.
    let host = PluginHost::new().expect("host builds");
    let component = host
        .compile(&noop_component_bytes())
        .expect("component compiles");

    for _ in 0..8 {
        let mut plugin = host.instantiate(&component).expect("instantiates");
        plugin.activate().expect("activate runs");
    }
}

#[test]
fn malformed_bytes_are_a_typed_compile_error_not_a_panic() {
    let host = PluginHost::new().expect("host builds");

    // Garbage input — not a component, not even valid wasm. Must be rejected
    // as a value on the `Compile` path, never a panic. (`Component` has no
    // `Debug`, so we assert on the discriminant rather than format the Ok arm.)
    assert!(
        matches!(
            host.compile(b"definitely not a wasm component"),
            Err(PluginHostError::Compile(_)),
        ),
        "expected a typed Compile error for garbage bytes",
    );
}

#[test]
fn a_bare_core_module_is_not_a_component() {
    // A valid *core* wasm module is not a *component*; the host must reject it
    // on the compile path rather than instantiate it.
    let core_module =
        wat::parse_str("(module (func (export \"activate\")))").expect("core module assembles");
    let host = PluginHost::new().expect("host builds");

    assert!(
        matches!(host.compile(&core_module), Err(PluginHostError::Compile(_)),),
        "expected a typed Compile error for a core module",
    );
}
