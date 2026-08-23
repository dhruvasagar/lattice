//! OM.0 — a plugin's seams drain in dependency order, whatever order its
//! manifest declares them in.
//!
//! `mode-keymap-binding` resolves `command` against the `CommandRegistry` **at
//! registration**, so a mode binding a chord to the plugin's OWN grammar action
//! only works if the `grammar` seam drained first. Before this slice the drain
//! loop walked `manifest.provides` verbatim, which made correctness a property
//! of a guest-authored TOML file — and both bundled multi-seam manifests
//! carried a hand-written comment warning the author to get the order right.
//! `provides` is guest input; an invariant enforced by prose inside it is
//! enforced by discipline, not structurally.
//!
//! The failure is silent in the way that matters: the binding is skipped with a
//! log, the plugin loads "successfully", and the user's chord simply does
//! nothing.
//!
//! Uses the canonical `multiseam-guest` fixture (grammar + modes + config from
//! ONE component), whose `multiseam-mode` binds Normal `x` to its own
//! `multiseam-declines` action. Skips when the component was not built.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_grammar::CommandRegistryHandle;
use lattice_grammar::registry::CommandRegistry;
use lattice_keymap::{BindingMode, KeymapHandle, LookupResult};
use lattice_mode::{ModeId, ModeRegistry, ModeRegistryHandle};
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_runtime::EventBus;

fn multiseam_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/multiseam-guest/target/wasm32-wasip2/release/multiseam_guest.wasm"
    );
    std::fs::read(path).ok()
}

/// Write a plugin dir whose `provides` list is given verbatim — the point of
/// this test is that the ORDER of that list must not matter.
fn write_plugin_dir(root: &std::path::Path, id: &str, provides: &[&str], wasm: &[u8]) {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let list = provides
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        dir.join("plugin.toml"),
        format!("id = \"{id}\"\nprovides = [{list}]\n"),
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

fn temp_host(base: &std::path::Path) -> Arc<PluginHost> {
    Arc::new(PluginHost::with_dirs(base.join("cache"), base.join("data")).expect("host builds"))
}

fn empty_mode_registry() -> ModeRegistryHandle {
    Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()))
}

fn command_registry_with_builtins() -> CommandRegistryHandle {
    let mut commands = CommandRegistry::new();
    let _ = lattice_grammar::ex_commands::populate(&mut commands);
    Arc::new(arc_swap::ArcSwap::from_pointee(commands))
}

/// Load the multiseam fixture with `provides` in the given order and report
/// whether the mode's binding to its own grammar action resolved.
async fn binding_resolves_with_provides(provides: &[&str]) -> bool {
    let Some(wasm) = multiseam_guest_wasm() else {
        panic!("caller must check the fixture exists first");
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "multiseam-fixture", provides, &wasm);

    let mode_registry = empty_mode_registry();
    let keymap = KeymapHandle::new();

    let loader = PluginLoader::with_services(
        temp_host(base.path()),
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            command_registry: Some(command_registry_with_builtins()),
            mode_registry: Some(mode_registry.clone()),
            keymap: Some(keymap.clone()),
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            ..Default::default()
        },
    );

    let n = loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "the plugin loads whatever the provides order");
    assert!(
        mode_registry
            .load()
            .is_registered(ModeId::new("multiseam-mode")),
        "the mode itself registers regardless of order — only its BINDING \
         depends on the grammar drain, which is what makes the bug silent"
    );

    let chord = lattice_protocol::parse_chord_sequence("x").expect("chord parses");
    let mode = ModeId::new("multiseam-mode");
    matches!(
        keymap.lookup_with_context(BindingMode::Normal, &chord, &[mode]),
        LookupResult::Bound { .. }
    )
}

/// The order the bundled manifests hand-write today. This passed before the
/// slice too — it is the control.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grammar_before_modes_binds_the_plugins_own_action() {
    if multiseam_guest_wasm().is_none() {
        eprintln!("skipping: multiseam-guest wasm not built (no wasm32-wasip2 target)");
        return;
    }
    assert!(
        binding_resolves_with_provides(&["grammar", "modes", "config"]).await,
        "with grammar declared first the mode's binding to its own action resolves"
    );
}

/// The regression. A plugin author who lists `modes` first gets a mode whose
/// chords silently do nothing; the drain must sort rather than trust the list.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn modes_declared_before_grammar_still_binds() {
    if multiseam_guest_wasm().is_none() {
        eprintln!("skipping: multiseam-guest wasm not built (no wasm32-wasip2 target)");
        return;
    }
    assert!(
        binding_resolves_with_provides(&["modes", "grammar", "config"]).await,
        "drain order must be a property of the seam set, not of the order a \
         guest-authored manifest happens to list them in"
    );
}

/// Order-independence is the claim, so assert it over more than the one
/// interesting pair: config last, modes first, grammar buried in the middle.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_third_permutation_binds_too() {
    if multiseam_guest_wasm().is_none() {
        eprintln!("skipping: multiseam-guest wasm not built (no wasm32-wasip2 target)");
        return;
    }
    assert!(
        binding_resolves_with_provides(&["modes", "config", "grammar"]).await,
        "every permutation binds"
    );
}
