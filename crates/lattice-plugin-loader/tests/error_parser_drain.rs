//! CM.6b end-to-end: a plugin declaring `error-parser` in its `provides` has
//! its parser factory registered into the compilation parser-factory registry
//! during the ordinary load, and unloading removes it again.
//!
//! CM.6 proved the seam through a real guest at the *host* layer. What this
//! adds is the half that was missing until CM.6b: that `load_path` actually
//! routes an `error-parser` guest somewhere a compilation run can reach, and
//! that the parsers minted from that registration behave like the natives —
//! one per pipe reader, with independent pending state.
//!
//! Skips when the fixture wasn't built.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_compilation::{CompilationParserFactories, CompilationParserFactoriesHandle};
use lattice_config::ConfigRegistry;
use lattice_grammar::CommandRegistryHandle;
use lattice_grammar::registry::CommandRegistry;
use lattice_keymap::KeymapHandle;
use lattice_mode::{
    ContextSourceRegistry, GutterDecorationSourceRegistry, ModeRegistry, ModeRegistryHandle,
    PluginMetaSink,
};
use lattice_picker::source::PickerRegistry;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader, PluginLoaderHandle};
use lattice_runtime::EventBus;

/// The CM.6 fixture guest, or `None` when it wasn't built (skip).
///
/// Read by path rather than through `ERROR_PARSER_GUEST_WASM`: that variable
/// is exported by the *host* crate's build.rs and is not visible to this
/// crate's compilation, so `option_env!` would silently be `None` here and
/// every real assertion would pass by skipping. Same resolution the sibling
/// loader tests (`require_drain.rs`, `init_config.rs`) use.
fn guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/error-parser-guest",
        "/target/wasm32-wasip2/release/error_parser_guest.wasm"
    );
    std::fs::read(path).ok()
}

#[derive(Default)]
struct Sink;
impl PluginMetaSink for Sink {
    fn register_plugin(&self, _id: u32, _name: String, _doc: String) {}
    fn unregister_plugin(&self, _id: u32) {}
}

/// A plugin dir declaring the `error-parser` seam.
fn write_plugin_dir(dir: &std::path::Path, wasm: &[u8]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        "id = \"q-parser\"\nprovides = [\"error-parser\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("q-parser.wasm"), wasm).unwrap();
}

/// A fully-wired loader — every registry present, so `run_teardown` runs its
/// reversal rather than logging a partial-unload skip.
fn loader(base: &std::path::Path, parsers: CompilationParserFactoriesHandle) -> PluginLoaderHandle {
    let host = Arc::new(PluginHost::with_dirs(base.join("cache"), base.join("data")).unwrap());
    let mut commands = CommandRegistry::new();
    let _ = lattice_grammar::ex_commands::populate(&mut commands);
    Arc::new(PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            picker_registry: Some(Arc::new(arc_swap::ArcSwap::from_pointee(
                PickerRegistry::new(),
            ))),
            command_registry: Some(
                Arc::new(arc_swap::ArcSwap::from_pointee(commands)) as CommandRegistryHandle
            ),
            mode_registry: Some(
                Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()))
                    as ModeRegistryHandle,
            ),
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
            meta_sink: Some(Arc::new(Sink) as Arc<dyn PluginMetaSink>),
            parser_factories: Some(parsers),
            ..Default::default()
        },
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loading_an_error_parser_plugin_registers_its_factory() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: error-parser fixture guest not built");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugin_dir = base.path().join("q-parser");
    write_plugin_dir(&plugin_dir, &wasm);

    let parsers = CompilationParserFactories::new_handle();
    assert!(parsers.load().is_empty(), "nothing registered before load");

    let loader = loader(base.path(), parsers.clone());
    loader
        .load_path(&plugin_dir, TrustTier::Bundled)
        .await
        .expect("the error-parser plugin loads");

    assert_eq!(
        parsers.load().len(),
        1,
        "the load should have registered exactly one parser factory"
    );
}

/// The registered factory produces working parsers — and a *different* one per
/// call, which is what the two pipe readers of a compilation run each do.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_registered_factory_parses_the_fixture_format() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: error-parser fixture guest not built");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugin_dir = base.path().join("q-parser");
    write_plugin_dir(&plugin_dir, &wasm);

    let parsers = CompilationParserFactories::new_handle();
    let loader = loader(base.path(), parsers.clone());
    loader
        .load_path(&plugin_dir, TrustTier::Bundled)
        .await
        .expect("the error-parser plugin loads");

    // Exactly what `read_parsed_pipe` does at the top of each reader.
    let mut out_side = parsers.load().create_all();
    let mut err_side = parsers.load().create_all();
    assert_eq!(out_side.len(), 1);
    assert_eq!(err_side.len(), 1);

    // The stdout reader primes a diagnostic…
    assert!(
        out_side[0].feed("ERR the build broke").is_empty(),
        "a header alone completes nothing"
    );
    // …and the stderr reader must not be able to complete it.
    assert!(
        err_side[0].feed("  at other.q:1:1").is_empty(),
        "the two readers' parsers share pending state"
    );
    let entries = out_side[0].feed("  at src/thing.q:12:5");
    assert_eq!(entries.len(), 1, "got {entries:?}");
    assert_eq!(entries[0].path, std::path::PathBuf::from("src/thing.q"));
    assert_eq!(entries[0].message, "the build broke");
}

/// Unload reverses the registration — by provenance, with no recorded token.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unloading_removes_the_factory() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: error-parser fixture guest not built");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugin_dir = base.path().join("q-parser");
    write_plugin_dir(&plugin_dir, &wasm);

    let parsers = CompilationParserFactories::new_handle();
    let loader = loader(base.path(), parsers.clone());
    loader
        .load_path(&plugin_dir, TrustTier::Bundled)
        .await
        .expect("the error-parser plugin loads");
    assert_eq!(parsers.load().len(), 1);

    let report = loader.unload("q-parser").expect("the plugin was loaded");
    assert_eq!(
        report.parser_factories, 1,
        "the report should account for the removed factory"
    );
    assert!(
        parsers.load().is_empty(),
        "a compilation run started after the unload must not mint the plugin's parser"
    );

    // Idempotent: the plugin is gone, so a second unload finds nothing.
    assert!(loader.unload("q-parser").is_none());
}
