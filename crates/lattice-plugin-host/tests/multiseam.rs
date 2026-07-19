//! AP.1 spike — proves ONE `.wasm` component can `provide` the SYNC grammar seam
//! AND the async `modes` + `config` seams at once (the `auto-pair` shape).
//!
//! The multi-seam question: a component's import set is fixed, and the grammar
//! seam instantiates against a **sync** linker while modes/config instantiate
//! against the **async** linker — so a combined component could only load if
//! BOTH linkers satisfy its full (all-sync) import set. This test compiles the
//! `multiseam-guest` fixture ONCE and drains each seam from the same component:
//!   - grammar via `instantiate_grammar_plugin` (sync, grammar linker),
//!   - config  via `spawn_config_plugin`        (async, async linker),
//!   - modes   via `spawn_mode_plugin`          (async, async linker).
//! All three registering from the one artifact is the feasibility proof for
//! shipping auto-pair as a single multi-seam plugin.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_grammar::CommandRegistry;
use lattice_keymap::KeymapHandle;
use lattice_mode::{CapabilitySet, ModeId, ModeRegistry};
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};

fn guest_wasm() -> Option<&'static str> {
    let path = env!("MULTISEAM_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_component_provides_grammar_modes_and_config() {
    let Some(path) = guest_wasm() else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();

    // Compile the SAME component once; every drain below instantiates *this*
    // artifact — with its one fixed (all-sync) import set — against a different
    // linker.
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();
    let manifest = PluginManifest::new("multiseam", Vec::new(), CapabilitySet::empty());
    let bus = Arc::new(lattice_runtime::EventBus::new());

    // 1. GRAMMAR — SYNC instantiation against the grammar linker. The component
    //    ALSO imports modes/config; this succeeds only because the grammar linker
    //    is a superset carrying those (sync) register funcs.
    let grammar_set = host
        .instantiate_grammar_plugin(&component, &manifest, TrustTier::Bundled, &bus, None)
        .expect("grammar drain instantiates the combined component (sync linker)");
    let mut commands = CommandRegistry::new();
    grammar_set.register_all(&mut commands);
    assert!(
        commands.id_by_name("multiseam-read").is_some(),
        "the grammar action registered from the combined component"
    );

    // 2. CONFIG — ASYNC instantiation against the async linker, SAME component.
    //    Succeeds only because the async linker is a superset carrying the
    //    grammar + buffer imports the component also declares.
    let config_registry = Arc::new(ConfigRegistry::default());
    let (_cid, options) = host
        .spawn_config_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::event(),
            &config_registry,
        )
        .await
        .expect("config drain instantiates the combined component (async linker)");
    assert!(
        options.iter().any(|o| o == "multiseam.style"),
        "the config option registered from the combined component"
    );

    // 3. MODES — ASYNC instantiation against the async linker, SAME component.
    let mut mode_registry = ModeRegistry::default();
    let keymap = KeymapHandle::new();
    let (_mid, modes) = host
        .spawn_mode_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::event(),
            &mut mode_registry,
            &commands,
            &keymap,
        )
        .await
        .expect("modes drain instantiates the combined component (async linker)");
    assert!(
        modes.contains(&ModeId::new("multiseam-mode")),
        "the minor mode registered from the combined component"
    );

    // All three seams registered from the ONE artifact — a single multi-seam
    // plugin (auto-pair) is feasible on the current two-linker architecture.
}
