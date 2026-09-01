//! OA.14d end-to-end: config can reach an option a plugin reads AT LOAD.
//!
//! The reported defect was `org.todo-keyword-styles` having no effect. The
//! cause is not org's: org reads `org.todo-keywords` inside
//! `register-theme-elements`, a load-time export, and neither config home can
//! reach it — `lattice.toml` is applied before org registers the option (so the
//! value is dropped as unknown), and `init.rs`'s `on-plugin-loaded` fires after
//! the export has already run. `pre-plugin-loaded` is the seam that closes it.
//!
//! `preload-guest` is org reduced to the two seams that make the bug possible:
//! it declares an option from `config` and reads it back from `theme`,
//! registering an element NAMED after the value it saw. So the assertion is on
//! what the guest held at the moment it consumed the option — not on what the
//! option holds afterwards, which would pass even if the handler lost the race
//! by a microsecond.
//!
//! Three properties, and each has failed a draft of this feature:
//!
//!   1. the handler's value reaches the load-time read (the barrier holds),
//!   2. without a handler the guest sees its compiled default (the fixture is
//!      capable of showing the bug, so property 1 is not vacuous),
//!   3. the handler discriminates by name — a plugin nobody configured is
//!      loaded untouched.
//!
//! Skips when either fixture wasn't built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_grammar::{CommandRegistry, CommandRegistryHandle};
use lattice_keymap::KeymapHandle;
use lattice_mode::{ModeRegistry, ModeRegistryHandle};
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_runtime::EventBus;
use lattice_theme::{ElementName, ThemeRegistryHandle};

fn init_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/init-guest/target/wasm32-wasip2/release/init_guest.wasm"
    );
    std::fs::read(path).ok()
}

fn preload_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/preload-guest/target/wasm32-wasip2/release/preload_guest.wasm"
    );
    std::fs::read(path).ok()
}

fn write_plugin_dir(root: &std::path::Path, id: &str, provides: &str, wasm: &[u8]) {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        format!("id = \"{id}\"\nprovides = [{provides}]\n"),
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

struct Rig {
    loader: PluginLoader,
    theme: ThemeRegistryHandle,
    config: Arc<ConfigRegistry>,
}

fn rig(base: &std::path::Path) -> Rig {
    let theme: ThemeRegistryHandle = Arc::new(lattice_theme::InMemoryThemeRegistry::new(
        lattice_theme::default_palette(),
    ));
    let config = Arc::new(ConfigRegistry::default());
    let commands: CommandRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
    let modes: ModeRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()));
    let host = Arc::new(
        PluginHost::with_dirs(base.join("cache"), base.join("data")).expect("host builds"),
    );
    let loader = PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            command_registry: Some(commands),
            mode_registry: Some(modes),
            config_registry: Some(config.clone()),
            theme_registry: Some(theme.clone()),
            keymap: Some(KeymapHandle::new()),
            ..Default::default()
        },
    );
    Rig {
        loader,
        theme,
        config,
    }
}

/// The element the fixture registers records the option value it read during
/// its own load. `<plugin>.saw-<value>`.
fn saw(plugin: &str, value: &str) -> ElementName {
    ElementName::from(format!("{plugin}.saw-{value}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_init_handler_reaches_an_option_the_plugin_reads_at_load() {
    let (Some(init_wasm), Some(preload_wasm)) = (init_guest_wasm(), preload_guest_wasm()) else {
        eprintln!("skipping: init-guest and/or preload-guest wasm not built");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    // init.rs is loaded by path, so its manifest + wasm sit directly in the dir.
    let init_dir = base.path().join("init");
    std::fs::create_dir_all(&init_dir).unwrap();
    std::fs::write(
        init_dir.join("plugin.toml"),
        "id = \"init\"\nprovides = [\"events\"]\n",
    )
    .unwrap();
    std::fs::write(init_dir.join("component.wasm"), &init_wasm).unwrap();

    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(
        &plugins_dir,
        "preload-fixture",
        "\"config\", \"theme\"",
        &preload_wasm,
    );

    let rig = rig(base.path());

    // 1. init.rs first, so its `pre-plugin-loaded` subscription is live before
    //    anything can fire the event (CI.2's ordering, which OA.14d relies on:
    //    a handler registered after the fact would simply never be called, and
    //    the failure would look exactly like the bug it is meant to fix).
    rig.loader
        .load_path(&init_dir, TrustTier::Bundled)
        .await
        .expect("init.rs loads");

    // 2. Then the plugin whose load-time read the handler is racing.
    let n = rig
        .loader
        .discover_and_load(&plugins_dir, TrustTier::UserInstalled)
        .await;
    assert_eq!(n, 1, "the preload fixture loads");

    // The load-time read saw the init handler's value. No polling: the loader
    // awaited the handler before draining `theme`, so by the time
    // `discover_and_load` returned this is already settled — a test that had to
    // wait here would be testing a race rather than a barrier.
    assert!(
        rig.theme.id(&saw("preload-fixture", "from-init")).is_some(),
        "the guest read the value init.rs set from its pre-plugin-loaded handler"
    );
    assert!(
        rig.theme
            .id(&saw("preload-fixture", "compiled-default"))
            .is_none(),
        "…and did NOT fall back to its compiled default, which is the reported bug"
    );

    // The option itself also holds the value — necessary but not sufficient,
    // which is why it is the second assertion rather than the only one.
    assert_eq!(
        rig.config
            .lookup("preload-fixture.keywords")
            .map(|o| o.get_formatted()),
        Some("from-init".to_string()),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_handler_the_guest_reads_its_compiled_default() {
    // The control. Without it, the test above proves only that SOME value
    // reached the read — it would pass against a fixture that hardcoded
    // `from-init` and never consulted the option at all.
    let Some(preload_wasm) = preload_guest_wasm() else {
        eprintln!("skipping: preload-guest wasm not built");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(
        &plugins_dir,
        "preload-fixture",
        "\"config\", \"theme\"",
        &preload_wasm,
    );

    let rig = rig(base.path());
    let n = rig
        .loader
        .discover_and_load(&plugins_dir, TrustTier::UserInstalled)
        .await;
    assert_eq!(n, 1, "the preload fixture loads");

    assert!(
        rig.theme
            .id(&saw("preload-fixture", "compiled-default"))
            .is_some(),
        "with nobody subscribed, the guest reads the option's declared default"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_theme_seams_guest_can_read_an_option_at_all() {
    // The OTHER half of the reported bug, isolated from the event entirely.
    //
    // Ordering was never the whole story: the `theme` store carried no config
    // registry, so `get-option` inside `register-theme-elements` answered
    // `none` no matter what anyone had configured or when. org's per-keyword
    // colours came from the compiled default by construction, and no event
    // could have changed that. This test would pass with `pre-plugin-loaded`
    // deleted and fail with the registry unwired, which is what makes it a
    // separate test rather than a second assertion above.
    let Some(preload_wasm) = preload_guest_wasm() else {
        eprintln!("skipping: preload-guest wasm not built");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    write_plugin_dir(
        &plugins_dir,
        "preload-fixture",
        "\"config\", \"theme\"",
        &preload_wasm,
    );

    let rig = rig(base.path());
    // Pre-register the option the guest is about to declare. Registration is
    // idempotent (an existing name is left alone), so this stands in for any
    // value that reached the registry before the plugin loaded.
    assert!(lattice_plugin_host::config_host::register_plugin_option(
        &rig.config,
        "preload-fixture.keywords",
        lattice_plugin_host::config_host::PluginOptionKind::String,
        "set-before-the-load",
        "pre-registered by the test",
    ));

    let n = rig
        .loader
        .discover_and_load(&plugins_dir, TrustTier::UserInstalled)
        .await;
    assert_eq!(n, 1, "the preload fixture loads");

    assert!(
        rig.theme
            .id(&saw("preload-fixture", "set-before-the-load"))
            .is_some(),
        "the theme seam's guest read the registry, rather than answering `none`"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_handler_configures_only_the_plugin_it_names() {
    // The event fires for EVERY plugin — a handler must therefore discriminate,
    // and carrying the name is what lets it. Here the same init.rs handler sees
    // a load it has no config for, and the plugin loads exactly as it would
    // have with no init.rs at all.
    let (Some(init_wasm), Some(preload_wasm)) = (init_guest_wasm(), preload_guest_wasm()) else {
        eprintln!("skipping: init-guest and/or preload-guest wasm not built");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let init_dir = base.path().join("init");
    std::fs::create_dir_all(&init_dir).unwrap();
    std::fs::write(
        init_dir.join("plugin.toml"),
        "id = \"init\"\nprovides = [\"events\"]\n",
    )
    .unwrap();
    std::fs::write(init_dir.join("component.wasm"), &init_wasm).unwrap();

    let plugins_dir = base.path().join("plugins");
    // Same component, a DIFFERENT manifest id — so the handler's name check is
    // the only thing that can decide, and its options land in another namespace.
    write_plugin_dir(
        &plugins_dir,
        "some-other-plugin",
        "\"config\", \"theme\"",
        &preload_wasm,
    );

    let rig = rig(base.path());
    rig.loader
        .load_path(&init_dir, TrustTier::Bundled)
        .await
        .expect("init.rs loads");
    let n = rig
        .loader
        .discover_and_load(&plugins_dir, TrustTier::UserInstalled)
        .await;
    assert_eq!(n, 1, "the unrelated plugin loads");

    assert!(
        rig.theme
            .id(&saw("some-other-plugin", "compiled-default"))
            .is_some(),
        "a plugin the handler does not name is loaded untouched"
    );
    assert!(
        rig.config
            .lookup("preload-fixture.keywords")
            .is_none_or(|o| o.get_formatted() != "from-init"),
        "the handler did not set an option in a namespace nothing registered"
    );
}
