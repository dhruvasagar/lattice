//! PH.4 — the `project` plugin documents its pickers, and `<C-h>` can find
//! the pages.
//!
//! `<C-h>` finds a plugin picker's page by asking for a topic ending
//! `.picker-<id>` registered by the plugin that OWNS the source
//! (`Editor::do_picker_help`, pinned host-side in
//! `lattice-host/tests/picker_help.rs` against a fixture). That fixture sets
//! both ids by hand, so it cannot catch the two halves disagreeing in
//! production. They DO differ — a plugin is instantiated once per seam and
//! each instance gets its own id, so the help seam stamps one number and
//! `WasmPickerSource::owner_plugin` reports another. What must hold is that
//! both resolve to the same plugin through the seam aliases the loader
//! reports (`PluginMetaSink::register_seam_ids`), which is the resolution the
//! host's `<C-h>` uses. This loads the REAL plugin and asserts exactly that.
//!
//! Skips when the plugin was not built for `wasm32-wasip2`.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lattice_config::ConfigRegistry;
use lattice_grammar::{CommandRegistry, CommandRegistryHandle};
use lattice_help::topics::HelpTopicRegistryHandle;
use lattice_keymap::KeymapHandle;
use lattice_mode::{
    ContextSourceRegistry, GutterDecorationSourceRegistry, ModeRegistry, ModeRegistryHandle,
    PluginMetaSink,
};
use lattice_picker::PickerRegistryHandle;
use lattice_picker::source::PickerRegistry;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_runtime::EventBus;

fn plugin_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../plugins/project/target/wasm32-wasip2/release/project.wasm"
    );
    std::fs::read(path).ok()
}

/// Resolves a seam id to its plugin's primary id, as the host's
/// `PluginMetaRegistry::primary_of` does.
#[derive(Default)]
struct AliasSink {
    aliases: Mutex<HashMap<u32, u32>>,
}

impl AliasSink {
    fn plugin_of(&self, id: u64) -> u64 {
        let seam = u32::try_from(id).unwrap();
        u64::from(
            self.aliases
                .lock()
                .unwrap()
                .get(&seam)
                .copied()
                .unwrap_or(seam),
        )
    }
}

impl PluginMetaSink for AliasSink {
    fn register_plugin(&self, _id: u32, _name: String, _doc: String) {}
    fn unregister_plugin(&self, _id: u32) {}
    fn register_seam_ids(&self, primary: u32, seam_ids: &[u32]) {
        let mut aliases = self.aliases.lock().unwrap();
        for &s in seam_ids.iter().filter(|&&s| s != primary) {
            aliases.insert(s, primary);
        }
    }
}

/// The real manifest, so every seam the plugin `provides` is wired — an
/// unwired seam fails the WHOLE load and this would silently load nothing.
fn write_plugin_dir(root: &std::path::Path, wasm: &[u8]) {
    let dir = root.join("project");
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../plugins/project/plugin.toml"
    ))
    .unwrap();
    std::fs::write(dir.join("plugin.toml"), manifest).unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_projects_pickers_pages_are_owned_by_the_plugin_that_owns_the_pickers() {
    let Some(wasm) = plugin_wasm() else {
        eprintln!("skipping: plugins/project not built for wasm32-wasip2");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(&plugins_dir, &wasm);

    let help: HelpTopicRegistryHandle = lattice_help::topics::builtin_topics().into_handle();
    let pickers: PickerRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(PickerRegistry::new()));
    let commands: CommandRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
    let modes: ModeRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()));
    let host = Arc::new(
        PluginHost::with_dirs(base.path().join("cache"), base.path().join("data"))
            .expect("host builds"),
    );
    let sink = Arc::new(AliasSink::default());
    let loader = PluginLoader::with_services(
        host,
        LoaderServices {
            help_topics: Some(help.clone()),
            parser_factories: Some(lattice_compilation::CompilationParserFactories::new_handle()),
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            picker_registry: Some(pickers.clone()),
            command_registry: Some(commands),
            mode_registry: Some(modes),
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            keymap: Some(KeymapHandle::new()),
            decoration_registry: Some(Arc::new(arc_swap::ArcSwap::from_pointee(
                GutterDecorationSourceRegistry::new(),
            ))),
            context_registry: Some(Arc::new(arc_swap::ArcSwap::from_pointee(
                ContextSourceRegistry::new(),
            ))),
            theme_registry: Some(Arc::new(lattice_theme::InMemoryThemeRegistry::new(
                lattice_theme::default_palette(),
            ))),
            transient_registry: Some(Arc::new(lattice_picker::TransientSourceRegistry::new())),
            tracer: None,
            meta_sink: Some(sink.clone() as Arc<dyn PluginMetaSink>),
            ..Default::default()
        },
    );
    let n = loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(n, 1, "the project plugin loaded");

    let topics = help.load();
    let registry = pickers.load();
    for source in ["projects", "project-buffers"] {
        let owner = registry
            .entry(source)
            .and_then(|e| e.generator.as_ref())
            .and_then(|g| g.owner_plugin())
            .unwrap_or_else(|| panic!("`{source}` is registered and knows its plugin"));

        let name = format!("project.picker-{source}");
        let topic = topics.lookup(&name).unwrap_or_else(|| {
            panic!(
                "no `{name}` page; plugin topics registered: {:?}",
                topics
                    .names()
                    .filter(|n| n.starts_with("project"))
                    .collect::<Vec<_>>()
            )
        });
        let page_plugin = sink.plugin_of(topic.plugin_id.expect("a plugin page names its plugin"));
        assert_eq!(
            page_plugin,
            sink.plugin_of(owner),
            "`{name}` and `{source}` must resolve to the same plugin, or \
             `<C-h>` rejects the plugin's own page as another plugin's"
        );
        assert!(
            topic.body.render().contains("## Keys in this picker"),
            "`{name}` leads with its keys table"
        );
    }
}
