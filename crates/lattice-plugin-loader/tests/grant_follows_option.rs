//! PMF.4: a plugin whose file access follows one of its options is reloaded
//! when that option's value moves the grant — and only then.
//!
//! Uses the `config-guest` fixture, loaded as `config-fixture`, whose string
//! option `config-fixture.label` (default `hello`) stands in for a directory
//! option such as `org.directory`. Its manifest grants
//! `fs:read:{config-fixture.label|<tmp>/default}`.
//!
//! Every reload is observed as a `PluginLoaded` event, the way the editor sees
//! it; no test hook into the loader.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use lattice_config::ConfigRegistry;
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_protocol::{Event, EventKind};
use lattice_runtime::{EventBus, EventFilter, SubscriptionTarget};

fn config_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/config-guest/target/wasm32-wasip2/release/config_guest.wasm"
    );
    std::fs::read(path).ok()
}

/// Waits for a reload to finish: a `PluginUnloaded` followed by a
/// `PluginLoaded`. `false` if `deadline` passes first.
///
/// A fixed sleep is not this. It measures how long the reload took on the
/// machine that wrote it; on a slower one the count lands in the reload's own
/// gap — unloaded, not yet loaded again — and reads zero copies (CI did).
async fn reloaded(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Event>,
    deadline: Duration,
) -> bool {
    let until = tokio::time::Instant::now() + deadline;
    let mut unloaded = false;
    loop {
        match tokio::time::timeout_at(until, rx.recv()).await {
            Ok(Some(Event::PluginUnloaded { .. })) => unloaded = true,
            Ok(Some(Event::PluginLoaded { .. })) if unloaded => return true,
            Ok(Some(_)) => {}
            _ => return false,
        }
    }
}

/// The next `PluginLoaded`, or `None` if none arrives in `wait`.
async fn next_load(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Event>,
    wait: Duration,
) -> Option<String> {
    loop {
        match tokio::time::timeout(wait, rx.recv()).await {
            Ok(Some(Event::PluginLoaded { name, .. })) => return Some(name),
            Ok(Some(_)) => continue,
            _ => return None,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_plugin_reloads_when_its_grant_bearing_option_moves() {
    let Some(wasm) = config_guest_wasm() else {
        eprintln!("skipping: config-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins = base.path().join("plugins");
    let dir = plugins.join("config-fixture");
    std::fs::create_dir_all(&dir).unwrap();
    // A literal string ('…') for the path: a Windows temp path's backslashes
    // are escapes in a basic one, and the manifest would not parse.
    std::fs::write(
        dir.join("plugin.toml"),
        format!(
            "id = \"config-fixture\"\nprovides = [\"config\"]\n\
             capabilities = ['fs:read:{{config-fixture.label|{}}}']\n",
            base.path().join("default").display()
        ),
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), &wasm).unwrap();

    // The editor's wiring: option changes reach the bus.
    let bus = Arc::new(EventBus::new());
    let config = Arc::new(ConfigRegistry::default());
    let publish_to = bus.clone();
    config.set_event_publisher(Arc::new(move |event| publish_to.publish(event)));
    let (tx, mut loads) = tokio::sync::mpsc::unbounded_channel::<Event>();
    bus.subscribe(
        EventFilter::kind(EventKind::PluginLoaded),
        SubscriptionTarget::Channel(tx),
    );

    let loader = Arc::new(PluginLoader::with_services(
        Arc::new(
            PluginHost::with_dirs(base.path().join("cache"), base.path().join("data")).unwrap(),
        ),
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(bus.clone()),
            config_registry: Some(config.clone()),
            ..Default::default()
        },
    ));
    loader.subscribe_grant_options();

    assert_eq!(
        loader.discover_and_load(&plugins, TrustTier::Bundled).await,
        1
    );
    let wait = Duration::from_secs(5);
    assert_eq!(
        next_load(&mut loads, wait).await.as_deref(),
        Some("config-fixture")
    );

    // 1. Registration gave the option its own value (`hello`), which differs
    //    from the manifest default the grant started on — so it follows it.
    assert_eq!(
        next_load(&mut loads, wait).await.as_deref(),
        Some("config-fixture"),
        "the grant moves onto the registered value once the option exists"
    );

    // 2. The user moves it.
    let notes = base.path().join("notes");
    config
        .parse_and_set_command(&format!("config-fixture.label={}", notes.display()))
        .unwrap();
    assert_eq!(
        next_load(&mut loads, wait).await.as_deref(),
        Some("config-fixture"),
        "`:set` on the option reloads the plugin with the new grant"
    );

    // 3. Setting it to what it already is moves nothing, so nothing reloads —
    //    which is also what stops a reload re-registering its options from
    //    reloading it again.
    config
        .parse_and_set_command(&format!("config-fixture.label={}", notes.display()))
        .unwrap();
    assert_eq!(
        next_load(&mut loads, Duration::from_millis(800)).await,
        None,
        "the same value is not a reason to reload"
    );
}

/// Boot's two paths into one plugin directory. `require` loads the plugin
/// (`load_path`); the user's `init.rs` sets a grant-bearing option during that
/// load, so the grant moves and a reload is scheduled; then the user-root scan
/// walks the same directory. The scan used to land in the reload's gap —
/// unloaded, not yet loaded again — see nothing loaded, and load a second
/// copy: two entries in `:plugins`, or one and a failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scan_during_a_grant_reload_does_not_load_a_second_copy() {
    let Some(wasm) = config_guest_wasm() else {
        eprintln!("skipping: config-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };
    // The window is a few milliseconds wide; a handful of boots finds it
    // reliably when it is open.
    for round in 0..8 {
        let base = tempfile::tempdir().unwrap();
        let plugins = base.path().join("plugins");
        let dir = plugins.join("config-fixture");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("plugin.toml"),
            "id = \"config-fixture\"\nprovides = [\"config\"]\n\
             capabilities = [\"fs:read:{config-fixture.label|hello}\"]\n",
        )
        .unwrap();
        std::fs::write(dir.join("component.wasm"), &wasm).unwrap();

        let bus = Arc::new(EventBus::new());
        let config = Arc::new(ConfigRegistry::default());
        let publish_to = bus.clone();
        config.set_event_publisher(Arc::new(move |event| publish_to.publish(event)));
        let (tx, mut lifecycle) = tokio::sync::mpsc::unbounded_channel::<Event>();
        for kind in [EventKind::PluginLoaded, EventKind::PluginUnloaded] {
            bus.subscribe(
                EventFilter::kind(kind),
                SubscriptionTarget::Channel(tx.clone()),
            );
        }
        // `init.rs` on `pre-plugin-loaded`: point the option somewhere else.
        let init_config = config.clone();
        let init_value = format!(
            "config-fixture.label={}",
            base.path().join("notes").display()
        );
        bus.subscribe(
            EventFilter::kind(EventKind::PrePluginLoaded),
            SubscriptionTarget::Plugin {
                plugin: 0,
                handler: 1,
                sink: Arc::new(move |_event, _ack| {
                    let _ = init_config.parse_and_set_command(&init_value);
                    true
                }),
            },
        );
        let loader = Arc::new(PluginLoader::with_services(
            Arc::new(
                PluginHost::with_dirs(base.path().join("cache"), base.path().join("data")).unwrap(),
            ),
            LoaderServices {
                runtime: Some(tokio::runtime::Handle::current()),
                bus: Some(bus.clone()),
                config_registry: Some(config.clone()),
                ..Default::default()
            },
        ));
        loader.subscribe_grant_options();

        // `require`, then the scan — boot's order.
        loader
            .load_path(&dir, TrustTier::UserInstalled)
            .await
            .unwrap();
        loader
            .discover_and_load(&plugins, TrustTier::UserInstalled)
            .await;
        // The scan has returned, so any second copy it made is loaded already.
        // Wait for the reload — its unload, then its load — and the count is
        // final either way.
        assert!(
            reloaded(&mut lifecycle, Duration::from_secs(30)).await,
            "round {round}: the grant's reload finished"
        );

        let copies = loader
            .plugin_status()
            .into_iter()
            .filter(|s| s.name == "config-fixture")
            .count();
        assert_eq!(copies, 1, "round {round}: one copy loaded, not {copies}");
        assert!(
            loader.failed_loads().is_empty(),
            "round {round}: no second copy failing beside it: {:?}",
            loader.failed_loads()
        );
    }
}
