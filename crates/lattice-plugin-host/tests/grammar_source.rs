//! PH7.7c — the sync grammar trampoline, driven through a real guest.
//!
//! Instantiates the `grammar-guest` fixture (a `wasm32-wasip2` `grammar-plugin`
//! component) via [`PluginHost::instantiate_grammar_plugin`], registers its
//! contributions into a native `CommandRegistry`, and dispatches them through the
//! real `lattice_grammar` dispatcher — proving the whole seam end to end:
//!   - registration crosses (the guest's `register-grammar` → the `register-*`
//!     host funcs → 3 native specs: 2 motions + 1 text object),
//!   - provenance is host-stamped `SourceLayer::Plugin(id)` (a plugin cannot
//!     forge it),
//!   - a plugin motion dispatches through `execute_motion_only` — the **sync**
//!     trampoline fires into the guest and the `motion-result` crosses back
//!     (target = cursor line + count),
//!   - a guest-returned `err` degrades gracefully to `CommandError::Plugin` (a
//!     no-op), distinct from a host trap (§8).
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::buffer::Buffer;
use lattice_core::buffers::BufferId;
use lattice_grammar::CancellationToken;
use lattice_grammar::command::{CommandInvocation, Count};
use lattice_grammar::dispatcher::execute_motion_only;
use lattice_grammar::error::CommandError;
use lattice_grammar::registry::{CommandRegistry, TextObjectEnv};
use lattice_grammar::source::SourceLayer;
use lattice_mode::CapabilitySet;
use lattice_plugin_host::{PluginHost, PluginManifest, TrustTier};
use lattice_protocol::position::Position;
use tempfile::TempDir;

/// The fixture grammar component path, or `None` when it wasn't built (skip).
fn guest_wasm() -> Option<&'static str> {
    let path = env!("GRAMMAR_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// Instantiate the fixture + register its grammar into a fresh registry; returns
/// `(registry, plugin_id)`.
fn load(dir: &TempDir) -> (CommandRegistry, u32) {
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data"))
        .expect("host builds with tempdirs");
    let component = host
        .compile(&std::fs::read(guest_wasm().unwrap()).unwrap())
        .expect("compile grammar fixture");
    let manifest = PluginManifest::new("grammar-fixture", Vec::new(), CapabilitySet::empty());
    let set = host
        .instantiate_grammar_plugin(&component, &manifest, TrustTier::Bundled)
        .expect("instantiate + register-grammar");
    let plugin_id = set.plugin_id().0;
    // 2 motions (down-n, fails) + 1 text object (to-cursor).
    assert_eq!(
        set.len(),
        3,
        "guest contributed motion + text object + failing motion"
    );

    let mut registry = CommandRegistry::new();
    set.register_all(&mut registry);
    (registry, plugin_id)
}

#[test]
fn plugin_grammar_registers_with_host_stamped_plugin_provenance() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: grammar fixture guest not built (add the wasm32-wasip2 target)");
        return;
    }
    let dir = TempDir::new().unwrap();
    let (registry, plugin_id) = load(&dir);

    for name in ["down-n", "to-cursor", "fails"] {
        let id = registry
            .id_by_name(name)
            .unwrap_or_else(|| panic!("{name} registered"));
        assert_eq!(
            registry.lookup(id).unwrap().source.layer,
            SourceLayer::Plugin(plugin_id),
            "{name} stamped Plugin provenance (unforgeable, host-issued)"
        );
    }
}

#[test]
fn plugin_motion_dispatches_through_the_sync_trampoline() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: grammar fixture guest not built");
        return;
    }
    let dir = TempDir::new().unwrap();
    let (registry, _) = load(&dir);
    let motion_id = registry.id_by_name("down-n").unwrap();

    let buffer = Buffer::from_text("l0\nl1\nl2\nl3\nl4\nl5\n");
    let cancel = CancellationToken::never();
    // `down-n` returns cursor.line + count; from line 1 with count 3 → line 4.
    let target = execute_motion_only(
        &registry,
        &buffer,
        BufferId(1),
        Position { line: 1, byte: 0 },
        CommandInvocation::of(motion_id).with_count(Count(3)),
        &cancel,
        TextObjectEnv::default(),
    )
    .expect("plugin motion dispatches through the sync trampoline");
    assert_eq!(target, Position { line: 4, byte: 0 });
}

#[test]
fn plugin_motion_guest_err_degrades_to_a_graceful_no_op() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: grammar fixture guest not built");
        return;
    }
    let dir = TempDir::new().unwrap();
    let (registry, _) = load(&dir);
    let fails_id = registry.id_by_name("fails").unwrap();

    let buffer = Buffer::from_text("l0\nl1\n");
    let cancel = CancellationToken::never();
    // The guest has no `apply-motion` arm for this callback → a WIT `err`, which
    // the trampoline maps to `CommandError::Plugin` (the dispatcher commits no
    // effect — a graceful no-op, §8), NOT a panic or a host trap.
    let err = execute_motion_only(
        &registry,
        &buffer,
        BufferId(1),
        Position { line: 0, byte: 0 },
        CommandInvocation::of(fails_id),
        &cancel,
        TextObjectEnv::default(),
    )
    .expect_err("a guest err is a typed CommandError, not a success");
    assert!(
        matches!(err, CommandError::Plugin(_)),
        "guest err maps to CommandError::Plugin, got {err:?}"
    );
}
