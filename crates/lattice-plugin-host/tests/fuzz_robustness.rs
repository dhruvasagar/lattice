//! PH7.12c — graceful-degradation fuzz of the host's untrusted-input boundaries.
//!
//! The two places arbitrary bytes/text cross into the host from outside its
//! control are (1) component bytes handed to [`PluginHost::compile`], and (2)
//! plugin-manifest TOML ([`PluginManifest::from_toml_str`]). Both are the
//! security boundary: any input — random, truncated, adversarially malformed —
//! must degrade to a **typed error, never a panic / abort / hang**. These
//! property tests hammer each with randomised and hand-picked adversarial inputs
//! and assert exactly that.
//!
//! Two boundaries are deliberately NOT fuzzed here, with reason:
//!   - the guest→host *value* path (boundary `from_wit`) can't take arbitrary
//!     bytes — wasmtime's typed ABI only ever hands the host well-typed WIT
//!     values, and every `from_wit` returns `Result<_, String>` by construction
//!     (the PH7.12c audit verified the production path has no `unwrap`/`panic`/
//!     unguarded index on guest data);
//!   - malformed *timing* (fuel/epoch traps mid-call) is covered by PH7.12a's
//!     crash-quarantine tests — a trap becomes a typed `Trap` + one
//!     `PluginCrashed`, never a panic.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::str::FromStr;

use lattice_plugin_host::{Capability, PluginHost, PluginHostError, PluginManifest};
use proptest::prelude::*;

const NOOP_WAT: &str = include_str!("fixtures/noop.wat");

fn noop_component_bytes() -> Vec<u8> {
    wat::parse_str(NOOP_WAT).expect("no-op component WAT assembles")
}

proptest! {
    // Arbitrary bytes at the component-load boundary: whatever the input,
    // `compile` returns (the call completing IS the no-panic proof). A random
    // byte string is effectively never a valid component, but we do not assert
    // `Err` — only that the security boundary never panics on hostile input.
    #[test]
    fn compile_never_panics_on_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..1024)) {
        let host = PluginHost::new().expect("host builds");
        let _ = host.compile(&bytes);
    }

    // Bytes that START like real wasm (valid magic) but diverge — the case most
    // likely to reach deep into the parser — must still never panic.
    #[test]
    fn compile_never_panics_on_wasm_magic_prefixed_garbage(tail in proptest::collection::vec(any::<u8>(), 0..512)) {
        let host = PluginHost::new().expect("host builds");
        let mut bytes = b"\0asm".to_vec();
        bytes.extend_from_slice(&tail);
        let _ = host.compile(&bytes);
    }

    // Arbitrary text at the manifest boundary: `from_toml_str` degrades to a
    // typed `ManifestError`, never a panic — for any string.
    #[test]
    fn manifest_from_toml_never_panics_on_arbitrary_text(text in ".*") {
        let _ = PluginManifest::from_toml_str(&text);
    }

    // Arbitrary capability strings degrade to a typed parse result, never a panic.
    #[test]
    fn capability_from_str_never_panics(text in ".*") {
        let _ = Capability::from_str(&text);
    }
}

#[test]
fn compile_rejects_adversarial_component_prefixes_as_typed_errors() {
    let host = PluginHost::new().expect("host builds");
    let noop = noop_component_bytes();

    // Each is a value on the `Compile` path, never a panic. `Component` has no
    // `Debug`, so assert on the discriminant.
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("magic only, truncated", b"\0asm".to_vec()),
        // A valid CORE wasm module header (magic + version 1) — well-formed wasm,
        // but a module, not a component: the loader must reject it, not accept it.
        ("core-module header", b"\0asm\x01\0\0\0".to_vec()),
        // The first half of a REAL component: valid prefix, truncated body — the
        // parser must fail cleanly partway through.
        ("truncated real component", noop[..noop.len() / 2].to_vec()),
        ("ascii garbage", b"not a wasm component at all".to_vec()),
    ];

    for (name, bytes) in cases {
        assert!(
            matches!(host.compile(&bytes), Err(PluginHostError::Compile(_))),
            "case `{name}`: expected a typed Compile error, not a panic or Ok",
        );
    }
}

#[test]
fn manifest_handles_malformed_but_parseable_toml_gracefully() {
    // Valid TOML that is an invalid manifest — exercises the RawManifest →
    // PluginManifest conversion (past the TOML lexer), the layer a random string
    // rarely reaches. The contract is "never panics": some parse into a manifest
    // with defaults (e.g. an empty doc), some are typed errors — either is
    // graceful. The assertion is simply that each call returns, deterministically.
    let cases = [
        ("empty document", ""),
        ("wrong type for id", "id = 42"),
        (
            "unknown capability string",
            "id = \"p\"\ncapabilities = [\"nonsense:everything\"]",
        ),
        (
            "capability not a list",
            "id = \"p\"\ncapabilities = \"fs:read\"",
        ),
        ("array-of-tables where scalar expected", "[[id]]\nx = 1"),
    ];
    for (name, toml) in cases {
        let first = PluginManifest::from_toml_str(toml).is_ok();
        // A second call proves determinism / no interior corruption.
        let second = PluginManifest::from_toml_str(toml).is_ok();
        assert_eq!(first, second, "case `{name}`: parse is deterministic");
    }
}
