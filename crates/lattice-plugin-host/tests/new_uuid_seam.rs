//! OR.3 end-to-end — the host mints ids, from the seam that cannot mint its own.
//!
//! `new-uuid` exists for `read-file`'s exact reason, and this file's job is to
//! prove it against the case that reason names. `:org-roam-id-create` mints an
//! `:ID:` for the headline at point, which is a **grammar action**: it runs on
//! the grammar seam's *synchronous* linker, where `wasmtime-wasi`'s sync shim
//! blocks on a runtime internally and panics on a thread already inside one. A
//! guest minting through `wasi:random` would therefore work perfectly on the
//! async picker path and take the plugin down here.
//!
//! So the assertion that matters is not "the string looks like a UUID" — it is
//! that the call **returns at all from the sync trampoline**. A test that minted
//! from an async seam would pass against the guest-side implementation this seam
//! exists to replace.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::HashSet;
use std::sync::Arc;

use lattice_core::BufferId;
use lattice_grammar::{CommandInvocation, CommandRegistry, GrammarEnv};
use lattice_mode::CapabilitySet;
use lattice_plugin_host::{PluginHost, PluginManifest, TrustTier};
use lattice_protocol::CancellationToken;
use lattice_protocol::position::Position;

fn guest_wasm() -> Option<&'static str> {
    let path = env!("MULTISEAM_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// Run the fixture's `multiseam-uuid` action through the real sync trampoline
/// and return the two ids it minted.
fn mint_two(host: &PluginHost, component: &wasmtime::component::Component) -> (String, String) {
    let manifest = PluginManifest::new("multiseam", Vec::new(), CapabilitySet::empty());
    let bus = Arc::new(lattice_runtime::EventBus::new());
    let grammar_set = host
        .instantiate_grammar_plugin(component, &manifest, TrustTier::Bundled, &bus, None, None)
        .expect("grammar drain instantiates");
    let mut commands = CommandRegistry::new();
    grammar_set.register_all(&mut commands);
    let id = commands.id_by_name("multiseam-uuid").unwrap();

    let mut document = lattice_core::Document::from_text("x\n");
    let cancel = CancellationToken::never();
    let effect = lattice_grammar::execute_with_env(
        &commands,
        &mut document,
        BufferId(1),
        Position { line: 0, byte: 0 },
        CommandInvocation::of(id),
        &cancel,
        GrammarEnv::default(),
    )
    .expect("the mint action dispatches through the sync trampoline");

    let text = match effect {
        lattice_grammar::effect::Effect::Echo { text, .. } => text,
        other => panic!("expected an Echo carrying two ids, got {other:?}"),
    };
    let (a, b) = text.split_once('|').expect("<uuid>|<uuid>");
    (a.to_string(), b.to_string())
}

/// A well-formed canonical v4 UUID: `8-4-4-4-12` uppercase hex, version nibble
/// `4`, variant nibble in `8..=B`.
fn assert_well_formed_v4(id: &str) {
    let groups: Vec<&str> = id.split('-').collect();
    assert_eq!(groups.len(), 5, "canonical 8-4-4-4-12 form: {id}");
    assert_eq!(
        groups.iter().map(|g| g.len()).collect::<Vec<_>>(),
        vec![8, 4, 4, 4, 12],
        "group widths: {id}"
    );
    assert!(
        id.chars().all(|c| c == '-' || c.is_ascii_hexdigit()),
        "hex and hyphens only: {id}"
    );
    assert!(
        !id.chars().any(|c| c.is_ascii_lowercase()),
        "uppercase — the reference corpus is uppercase throughout (macOS \
         `uuidgen`, which `org-id` shells out to): {id}"
    );
    assert_eq!(
        groups[2].chars().next().unwrap(),
        '4',
        "version 4 (random) in the third group's first nibble: {id}"
    );
    assert!(
        matches!(groups[3].chars().next().unwrap(), '8' | '9' | 'A' | 'B'),
        "RFC 4122 variant in the fourth group's first nibble: {id}"
    );
}

/// **The test that matters.** The mint happens on the SYNC grammar linker, which
/// is the one place a guest-side UUID would have taken the plugin down.
#[test]
fn the_grammar_seam_can_mint_an_id() {
    let Some(path) = guest_wasm() else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();

    let (a, b) = mint_two(&host, &component);
    assert_well_formed_v4(&a);
    assert_well_formed_v4(&b);
    assert_ne!(
        a, b,
        "two mints in one call differ — a constant would satisfy every shape \
         assertion a single id could carry"
    );
}

/// No grant of any kind. `new-uuid` is deliberately ungated: it names no path
/// and reaches no resource, and a plugin that cannot give its own records
/// identities is a plugin that cannot keep records.
#[test]
fn minting_needs_no_capability() {
    let Some(path) = guest_wasm() else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();
    // `mint_two` builds a manifest requesting nothing at all.
    let (a, _) = mint_two(&host, &component);
    assert_well_formed_v4(&a);
}

/// A batch with no collisions. Not a proof of randomness — 122 random bits do
/// not collide in a thousand draws even if half of them are stuck — but it does
/// catch the failure that actually happens: a counter, a constant, or a seed
/// re-derived per call.
#[test]
fn a_batch_of_ids_has_no_duplicates() {
    let Some(path) = guest_wasm() else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();

    let mut seen: HashSet<String> = HashSet::new();
    for _ in 0..250 {
        let (a, b) = mint_two(&host, &component);
        assert!(seen.insert(a.clone()), "duplicate id: {a}");
        assert!(seen.insert(b.clone()), "duplicate id: {b}");
    }
    assert_eq!(seen.len(), 500);

    // …and the randomness is spread across the whole id, not just its tail: a
    // seed re-derived per call would leave the leading group constant.
    let leading: HashSet<&str> = seen.iter().filter_map(|s| s.split('-').next()).collect();
    assert!(
        leading.len() > 400,
        "the leading group varies ({} distinct of 500) — a per-call reseed \
         would pin it",
        leading.len()
    );
}
