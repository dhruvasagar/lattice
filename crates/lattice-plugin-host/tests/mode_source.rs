//! PH7.11a — the modes declaration seam, driven through a real guest.
//!
//! Instantiates the `modes-guest` fixture (a `wasm32-wasip2` `modes-plugin`
//! component) via [`PluginHost::spawn_mode_plugin`], which drives its
//! `register-modes` export against a native [`ModeRegistry`]. Proves the seam:
//!   - the guest's imported `register-mode` calls land modes in the SAME
//!     registry builtins use, carrying their activation policy + capabilities,
//!   - a mis-suffixed id is rejected by the registry's `-mode` gate (not in the
//!     returned ids, not in the registry),
//!   - OM.2: a MAJOR registers as a major, claims its language in the registry's
//!     language index, and binds into its own `MajorMode` layer — while a MINOR's
//!     language claim is dropped rather than honoured.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_grammar::CommandRegistry;
use lattice_keymap::{BindingMode, KeymapHandle, LookupResult};
use lattice_mode::{ActivationPolicy, CapabilitySet, ModeId, ModeRegistry};
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};
use tempfile::TempDir;

const PLUGIN_ID: &str = "modes-fixture";

/// The fixture modes component path, or `None` when it wasn't built (skip).
fn guest_wasm() -> Option<&'static str> {
    let path = env!("MODES_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_declares_minor_modes_into_the_shared_registry_end_to_end() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: modes fixture guest not built (add the wasm32-wasip2 target)");
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data"))
        .expect("host builds");
    let component = host
        .compile(&std::fs::read(wasm).unwrap())
        .expect("compile modes fixture");
    let manifest = PluginManifest::new(PLUGIN_ID, Vec::new(), CapabilitySet::empty());

    // A fresh registry (no foundation modes) — the plugin modes are the only
    // entries, so assertions are hermetic.
    let mut registry = ModeRegistry::default();
    // Commands the mode's keymap binds to (by name) + the keymap it pushes into.
    let mut commands = CommandRegistry::new();
    let _ = lattice_grammar::ex_commands::populate(&mut commands);
    let keymap = KeymapHandle::new();

    let (_plugin_id, ids) = host
        .spawn_mode_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::event(),
            &mut registry,
            &commands,
            &keymap,
            None,
        )
        .await
        .expect("spawn mode plugin");

    // Four well-formed modes registered; the mis-suffixed `not-suffixed` was
    // rejected by the registry's `-mode` gate (not returned, not registered).
    assert_eq!(ids.len(), 4, "four modes accepted: {ids:?}");
    for expected in [
        "git-blame-mode",
        "lsp-lens-mode",
        "fixture-lang-mode",
        "fixture-greedy-mode",
    ] {
        assert!(
            ids.iter().any(|id| id.as_str() == expected),
            "{expected} accepted"
        );
    }
    assert!(registry.is_registered(ModeId::new("git-blame-mode")));
    assert!(
        !registry.is_registered(ModeId::new("not-suffixed")),
        "the `-mode` suffix gate rejected the mis-named declaration"
    );

    // The declared activation policy + capability requirements are carried onto
    // the registered mode.
    let lens = registry
        .get(ModeId::new("lsp-lens-mode"))
        .expect("registered");
    assert!(matches!(
        lens.activation_policy(),
        ActivationPolicy::Universal
    ));
    assert_eq!(
        lens.required_capabilities(),
        CapabilitySet::LSP | CapabilitySet::DIAGNOSTICS
    );

    // PH7.11b: git-blame-mode's declared `<C-s>` → `ex:write` binding landed in
    // its OWN MinorMode layer (the capability-gated push). It resolves only when
    // the mode is active — the gated-layer contract.
    let blame = ModeId::new("git-blame-mode");
    let chord = lattice_protocol::parse_chord_sequence("<C-s>").expect("chord parses");
    assert!(
        matches!(
            keymap.lookup_with_context(BindingMode::Normal, &chord, &[blame]),
            LookupResult::Bound { .. }
        ),
        "the plugin mode's keymap binding resolves when the mode is active"
    );
    assert!(
        matches!(
            keymap.lookup_with_context(BindingMode::Normal, &chord, &[]),
            LookupResult::Unbound
        ),
        "the gated binding does not fire when the mode is inactive"
    );
}

/// OM.2 — a plugin declares a MAJOR and claims a language, end-to-end through a
/// real guest. Before this the host rejected `major` outright, so a
/// plugin-contributed language had no route to a major mode at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_declares_a_major_that_claims_its_language_end_to_end() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: modes fixture guest not built (add the wasm32-wasip2 target)");
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data"))
        .expect("host builds");
    let component = host
        .compile(&std::fs::read(wasm).unwrap())
        .expect("compile modes fixture");
    let manifest = PluginManifest::new(PLUGIN_ID, Vec::new(), CapabilitySet::empty());

    let mut registry = ModeRegistry::default();
    let mut commands = CommandRegistry::new();
    let _ = lattice_grammar::ex_commands::populate(&mut commands);
    let keymap = KeymapHandle::new();

    host.spawn_mode_plugin(
        &component,
        &manifest,
        TrustTier::Bundled,
        PluginBudget::event(),
        &mut registry,
        &commands,
        &keymap,
        None,
    )
    .await
    .expect("spawn mode plugin");

    // It registered AS a major — not silently downgraded, which is what the
    // pre-OM.2 host would have had to do if it had accepted the declaration.
    let major = ModeId::new("fixture-lang-mode");
    let entry = registry.get(major).expect("the major registered");
    assert_eq!(entry.kind(), lattice_mode::ModeKind::Major);

    // And it owns its language, so `resolve_major_mode` puts documents of that
    // language onto it. This is the whole point of the slice.
    assert_eq!(
        registry.find_major_for_lang("fixturelang"),
        Some(major),
        "the major claimed its language in the registry's language index"
    );

    // The minor that ALSO claimed the language registered, but did not win it —
    // a buffer has exactly one major, and it is not this.
    let greedy = ModeId::new("fixture-greedy-mode");
    assert!(registry.is_registered(greedy));
    assert_ne!(
        registry.find_major_for_lang("fixturelang"),
        Some(greedy),
        "a minor's language claim is dropped, not honoured"
    );

    // The major's keymap landed in its OWN `MajorMode` layer, gated the same
    // way a minor's is — a major layer is not always-on (K.1.c).
    let chord = lattice_protocol::parse_chord_sequence("<C-y>").expect("chord parses");
    assert!(
        matches!(
            keymap.lookup_with_context(BindingMode::Normal, &chord, &[major]),
            LookupResult::Bound { .. }
        ),
        "the major's binding resolves when it is the active major"
    );
    assert!(
        matches!(
            keymap.lookup_with_context(BindingMode::Normal, &chord, &[]),
            LookupResult::Unbound
        ),
        "and not when it is not"
    );
}

/// MO.1 end-to-end: a mode's declared option overrides survive the WASM
/// boundary and land as typed values on the registered `Mode`.
///
/// The unit tests in `mode_host` prove the resolution logic against a
/// hand-built `PluginModeDecl`. This proves the part they cannot: that the WIT
/// record actually carries the data, that bindgen projects it, and that the
/// value a guest wrote as a string comes out the other side as the native enum
/// the resolver will compare. A seam wired end-to-end can still answer nothing,
/// and the only way to know is to drive a real component through it.
///
/// The fixture's major declares TWO overrides — one resolvable, one that cannot
/// be — because "the rest of the set still applies" is the failure behaviour
/// this seam promises, and a set of one cannot demonstrate it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_modes_declared_option_overrides_cross_the_seam_end_to_end() {
    use lattice_config::{ConfigRegistry, FoldMethodOption};
    use lattice_core::FoldMethod;

    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: modes fixture guest not built (add the wasm32-wasip2 target)");
        return;
    };
    let dir = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(dir.path().join("cache"), dir.path().join("data"))
        .expect("host builds");
    let component = host
        .compile(&std::fs::read(wasm).unwrap())
        .expect("compile modes fixture");
    let manifest = PluginManifest::new(PLUGIN_ID, Vec::new(), CapabilitySet::empty());

    let config = ConfigRegistry::new();
    config.init_from_linkme();
    let mut registry = ModeRegistry::default();
    let mut commands = CommandRegistry::new();
    let _ = lattice_grammar::ex_commands::populate(&mut commands);
    let keymap = KeymapHandle::new();

    host.spawn_mode_plugin(
        &component,
        &manifest,
        TrustTier::Bundled,
        PluginBudget::event(),
        &mut registry,
        &commands,
        &keymap,
        Some(&config),
    )
    .await
    .expect("spawn mode plugin");

    let major = registry
        .get(ModeId::new("fixture-lang-mode"))
        .expect("the major registered");
    let set = major.options();

    assert_eq!(
        set.len(),
        1,
        "the resolvable override crossed and the unresolvable one was dropped"
    );
    let ov = set.iter().next().expect("one override");
    assert_eq!(
        ov.option_type_id,
        std::any::TypeId::of::<FoldMethodOption>(),
        "resolved to the native option identity"
    );
    assert_eq!(
        ov.downcast_value::<FoldMethod>(),
        Some(&FoldMethod::Syntax),
        "and the guest's string became the native enum"
    );

    // A mode that declared no options still has none — an empty list must not
    // acquire anything from the mode beside it in the same drain.
    assert!(
        registry
            .get(ModeId::new("git-blame-mode"))
            .expect("registered")
            .options()
            .is_empty(),
        "a mode that declared nothing gets nothing"
    );

    // And the global value is untouched: an override is a resolution layer.
    assert_eq!(
        *config.get_typed::<FoldMethodOption>().expect("registered"),
        FoldMethod::Manual,
        "the user's global foldmethod is not written by a mode declaring one"
    );
}
