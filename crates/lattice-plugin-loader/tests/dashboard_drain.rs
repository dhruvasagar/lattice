//! CR.4 end-to-end: a dashboard plugin discovered on disk loads and its
//! sections become reachable through the SAME `DashboardRegistry` the
//! built-in sections live in — ordered by the same rules, composed by the
//! same call, and reversed on unload.
//!
//! The load-bearing case is the plugin section that REPLACES a builtin.
//! Section ids are not namespaced (replacing is a supported capability), so
//! unload has to give the builtin back rather than leave a hole. CR.2 made
//! the registry shadow rather than overwrite exactly so that fall out of a
//! `retain`; this is the test that proves it end to end.
//!
//! Skips when the `dashboard-guest` fixture wasn't built.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lattice_config::ConfigRegistry;
use lattice_dashboard::{DashboardCtx, DashboardRegistryHandle, SectionSelection};
use lattice_grammar::{CommandRegistry, CommandRegistryHandle};
use lattice_keymap::KeymapHandle;
use lattice_mode::{ModeRegistry, ModeRegistryHandle, PluginMetaSink};
use lattice_picker::PickerRegistryHandle;
use lattice_picker::source::PickerRegistry;
use lattice_plugin_host::TrustTier;
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_runtime::EventBus;

fn dashboard_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/dashboard-guest/target/wasm32-wasip2/release/dashboard_guest.wasm"
    );
    std::fs::read(path).ok()
}

#[derive(Default)]
struct RecordingSink {
    registered: Mutex<Vec<(u32, String)>>,
}

impl PluginMetaSink for RecordingSink {
    fn register_plugin(&self, id: u32, name: String, _doc: String) {
        self.registered.lock().unwrap().push((id, name));
    }
    fn unregister_plugin(&self, id: u32) {
        self.registered.lock().unwrap().retain(|(i, _)| *i != id);
    }
}

fn write_plugin_dir(root: &std::path::Path, id: &str, provides: &str, wasm: &[u8]) {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        format!("id = \"{id}\"\nprovides = [\"{provides}\"]\n"),
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

struct Rig {
    loader: PluginLoader,
    dashboard: DashboardRegistryHandle,
}

/// Seeded with the REAL builtin section set: the replace/restore assertions
/// only mean something if `getting-started` actually exists to be displaced.
fn rig(base: &std::path::Path) -> Rig {
    let dashboard: DashboardRegistryHandle = lattice_dashboard::builtin_registry().into_handle();
    let commands: CommandRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
    let pickers: PickerRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(PickerRegistry::new()));
    let modes: ModeRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()));
    let sink: Arc<RecordingSink> = Arc::new(RecordingSink::default());
    let host = Arc::new(
        lattice_plugin_host::PluginHost::with_dirs(base.join("cache"), base.join("data"))
            .expect("host builds"),
    );
    let loader = PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            picker_registry: Some(pickers),
            command_registry: Some(commands),
            mode_registry: Some(modes),
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            keymap: Some(KeymapHandle::new()),
            meta_sink: Some(sink.clone() as Arc<dyn PluginMetaSink>),
            dashboard_sections: Some(dashboard.clone()),
            ..Default::default()
        },
    );
    Rig { loader, dashboard }
}

/// Compose the default page and return one string per rendered row.
fn composed(rig: &Rig) -> Vec<String> {
    rig.dashboard
        .load()
        .compose(&DashboardCtx::default(), &SectionSelection::Default)
        .iter()
        .flat_map(|f| f.rows.iter().map(|r| r.text()))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugin_section_composes_alongside_the_builtins() {
    let Some(wasm) = dashboard_guest_wasm() else {
        eprintln!("skipping: dashboard-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "dashboard-guest", "dashboard", &wasm);

    let rig = rig(base.path());
    let n = rig
        .loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "the dashboard plugin loads");

    let rows = composed(&rig);
    assert!(
        rows.iter().any(|r| r.contains("project-one")),
        "the plugin's added section is composed into the page: {rows:?}"
    );
    // Ordered by the same `order()` rule builtins use — `recent` is 15, so it
    // lands before `getting-started` (20) rather than being appended.
    let recent = rows.iter().position(|r| r.contains("project-one"));
    let replaced = rows.iter().position(|r| r.contains("REPLACED-BY-PLUGIN"));
    assert!(
        recent < replaced,
        "plugin sections sort by order() like builtins: {rows:?}"
    );
}

/// The case CR.2's shadow stack exists for.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unloading_restores_the_builtin_section_the_plugin_replaced() {
    let Some(wasm) = dashboard_guest_wasm() else {
        eprintln!("skipping: dashboard-guest wasm not built");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, "dashboard-guest", "dashboard", &wasm);

    let rig = rig(base.path());

    // What the builtin `getting-started` renders before any plugin loads.
    let builtin_ids_before = rig.dashboard.load().ids().len();
    let builtin_rows = composed(&rig);
    assert!(
        !builtin_rows
            .iter()
            .any(|r| r.contains("REPLACED-BY-PLUGIN")),
        "sanity: the builtin page has no plugin content yet"
    );

    rig.loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;

    let loaded = composed(&rig);
    assert!(
        loaded.iter().any(|r| r.contains("REPLACED-BY-PLUGIN")),
        "the plugin section displaced the builtin while loaded"
    );

    let report = rig
        .loader
        .unload("dashboard-guest")
        .expect("unload succeeds");
    assert_eq!(
        report.dashboard_sections, 2,
        "the teardown report counts both sections it withdrew"
    );

    let after = composed(&rig);
    assert!(
        !after.iter().any(|r| r.contains("REPLACED-BY-PLUGIN")),
        "the plugin's replacement is gone"
    );
    assert!(
        !after.iter().any(|r| r.contains("project-one")),
        "and so is its added section"
    );
    // The displaced builtin is BACK — not dropped, and in the same slot.
    assert_eq!(
        after, builtin_rows,
        "unload restores the page byte-for-byte, including the builtin the \
         plugin had replaced"
    );
    assert_eq!(rig.dashboard.load().ids().len(), builtin_ids_before);
}
