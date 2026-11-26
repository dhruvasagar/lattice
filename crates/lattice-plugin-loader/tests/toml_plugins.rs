//! Release plugins declared in `lattice.toml` reach the same install queue as
//! `init.rs`'s `require-release`, and install with no `init.rs` at all.
//!
//! Driven at the two seams the boot task uses: `declare_from_config_file`
//! (what boot calls with the user's `lattice.toml`) and `run_required_in`
//! (the drain + install + reconcile, with the network replaced by a map).
//! `RequirePhase::Boot` installs without loading, so the served "component"
//! only has to carry its manifest.
//!
//! The `init.rs` precedence test needs the plugin-manager fixture guest and
//! skips when it was not built.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lattice_config::ConfigRegistry;
use lattice_grammar::CommandRegistryHandle;
use lattice_grammar::registry::CommandRegistry;
use lattice_mode::{ModeRegistry, ModeRegistryHandle, PluginMetaSink};
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::resolve::{release_asset_name, release_asset_url};
use lattice_plugin_loader::{
    ComponentBuilder, ConfigBuildStatus, Fetcher, GitRunner, InstallEnv, LoaderServices,
    PluginLoader, PluginLoaderHandle, PluginSource, RequirePhase, RequiredSpec, run_required_in,
};
use lattice_runtime::EventBus;
use sha2::Digest;

#[derive(Default)]
struct Sink;
impl PluginMetaSink for Sink {
    fn register_plugin(&self, _id: u32, _name: String, _doc: String) {}
    fn unregister_plugin(&self, _id: u32) {}
}

fn commands_with_builtins() -> CommandRegistryHandle {
    let mut r = CommandRegistry::new();
    let _ = lattice_grammar::ex_commands::populate(&mut r);
    Arc::new(arc_swap::ArcSwap::from_pointee(r))
}

fn loader(base: &Path) -> PluginLoaderHandle {
    loader_with(base, None)
}

/// A loader that installs through `install` rather than the system's git,
/// network and plugin root.
fn loader_with(base: &Path, install: Option<InstallEnv>) -> PluginLoaderHandle {
    let host = Arc::new(PluginHost::with_dirs(base.join("cache"), base.join("data")).unwrap());
    Arc::new(PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            command_registry: Some(commands_with_builtins()),
            mode_registry: Some(
                Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()))
                    as ModeRegistryHandle,
            ),
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            meta_sink: Some(Arc::new(Sink) as Arc<dyn PluginMetaSink>),
            install,
            ..Default::default()
        },
    ))
}

fn write_config(dir: &Path, text: &str) -> PathBuf {
    let path = dir.join("lattice.toml");
    std::fs::write(&path, text).unwrap();
    path
}

/// A component carrying `manifest` as its `lattice-manifest` section: all the
/// release installer reads of it.
fn component(manifest: &str) -> Vec<u8> {
    let name = b"lattice-manifest";
    let mut bytes = b"\0asm\x0d\x00\x01\x00".to_vec();
    let body_len = 1 + name.len() + manifest.len();
    assert!(body_len < 128, "one-byte LEB in this fixture");
    bytes.push(0);
    bytes.push(body_len as u8);
    bytes.push(name.len() as u8);
    bytes.extend_from_slice(name);
    bytes.extend_from_slice(manifest.as_bytes());
    bytes
}

/// Serves fixed bodies by URL and counts every request, served or not.
#[derive(Clone, Default)]
struct Server {
    files: Arc<HashMap<String, Vec<u8>>>,
    fetches: Arc<AtomicUsize>,
}

impl Server {
    /// A release of `name` at `tag` from `repo`, with its checksum file.
    fn release(repo: &str, name: &str, tag: &str) -> Self {
        let wasm = component(&format!("id = \"{name}\"\n"));
        let url = release_asset_url(repo, tag, &release_asset_name(name, tag));
        let digest = format!("{:x}  {name}.wasm\n", sha2::Sha256::digest(&wasm));
        Self {
            files: Arc::new(HashMap::from([
                (format!("{url}.sha256"), digest.into_bytes()),
                (url, wasm),
            ])),
            fetches: Arc::default(),
        }
    }

    fn fetches(&self) -> usize {
        self.fetches.load(Ordering::SeqCst)
    }
}

impl Fetcher for Server {
    fn fetch(&self, url: &str, dest: &Path) -> Result<(), String> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        let body = self
            .files
            .get(url)
            .ok_or_else(|| format!("GET {url}: HTTP 404"))?;
        std::fs::write(dest, body).map_err(|e| e.to_string())
    }
}

/// A release install never runs git.
struct NoGit;
impl GitRunner for NoGit {
    fn run(&self, _cwd: &Path, args: &[&str]) -> Result<String, String> {
        panic!("a release install ran git {args:?}")
    }
}

/// Nor a toolchain: the point of declaring in `lattice.toml`.
struct NoToolchain;
impl ComponentBuilder for NoToolchain {
    fn build(&self, source_dir: &Path) -> Result<PathBuf, String> {
        panic!("a release install built {}", source_dir.display())
    }
}

/// Install into `base/plugins` through `server`, with no git or toolchain.
fn env(base: &Path, server: &Server) -> InstallEnv {
    InstallEnv {
        user_root: base.join("plugins"),
        cache_root: base.join("sources"),
        git: Arc::new(NoGit),
        fetcher: Arc::new(server.clone()),
        builder: Arc::new(NoToolchain),
    }
}

async fn install(loader: &PluginLoader, base: &Path, server: &Server) -> Vec<(String, String)> {
    run_required_in(loader, RequirePhase::Boot, env(base, server)).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_declaration_is_queued_as_a_release_spec() {
    let base = tempfile::tempdir().unwrap();
    let config = write_config(
        base.path(),
        "[plugins]\nshort = \"owner/short\"\n\
         [plugins.org]\nrelease = \"owner/org\"\nversion = \"v0.2.0\"\nenable-mode = \"org-mode\"\n",
    );
    let loader = loader(base.path());
    loader.declare_from_config_file(&config);

    let specs = loader.take_required();
    assert_eq!(
        specs,
        vec![
            RequiredSpec {
                name: "org".into(),
                source: PluginSource::Release {
                    repo: "owner/org".into(),
                    version: Some("v0.2.0".into()),
                    sha256: None,
                },
                enable_mode: Some("org-mode".into()),
                pinned: false,
            },
            RequiredSpec {
                name: "short".into(),
                source: PluginSource::Release {
                    repo: "owner/short".into(),
                    version: None,
                    sha256: None,
                },
                enable_mode: None,
                pinned: false,
            },
        ]
    );
    assert!(loader.failed_loads().is_empty());
}

/// A rejected entry is not silent: `:plugins` lists it under its id, located
/// at the file that declared it, with the parser's reason.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejected_declaration_is_recorded_where_plugins_shows_it() {
    let base = tempfile::tempdir().unwrap();
    let config = write_config(
        base.path(),
        "[plugins]\n\"../escape\" = \"owner/escape\"\ngood = \"owner/good\"\n\
         [plugins.built]\ngit = \"https://example.com/built\"\n",
    );
    let loader = loader(base.path());
    loader.declare_from_config_file(&config);

    let names: Vec<_> = loader.take_required().into_iter().map(|s| s.name).collect();
    assert_eq!(names, ["good"]);

    let failed = loader.failed_loads();
    let by_name = |n: &str| failed.iter().find(|f| f.name == n).unwrap();
    assert_eq!(failed.len(), 2, "{failed:?}");
    assert!(by_name("built").error.contains("declare it in init.rs"));
    assert!(by_name("../escape").error.contains("path component"));
    assert!(failed.iter().all(|f| f.dir == config));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_config_file_declares_nothing() {
    let base = tempfile::tempdir().unwrap();
    let loader = loader(base.path());
    loader.declare_from_config_file(&base.path().join("lattice.toml"));
    assert!(loader.take_required().is_empty());
    assert!(loader.failed_loads().is_empty());
}

/// The toolchain-free path end to end: no `init.rs`, a `lattice.toml`
/// declaring a release, and the boot drain installs it, asks for its mode, and
/// never touches git or cargo. The second boot finds it installed and does no
/// network I/O at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_no_init_rs_a_declared_release_installs_and_a_warm_boot_fetches_nothing() {
    let base = tempfile::tempdir().unwrap();
    let config = write_config(
        base.path(),
        "[plugins]\nbroken = \"not a repo@v1\"\n\
         [plugins.demo]\nrelease = \"example/demo\"\nversion = \"v1.0.0\"\n\
         enable-mode = \"demo-mode\"\n",
    );
    let server = Server::release("example/demo", "demo", "v1.0.0");

    let loader = loader(base.path());
    loader.declare_from_config_file(&config);
    let modes = install(&loader, base.path(), &server).await;

    assert_eq!(modes, [("demo".to_string(), "demo-mode".to_string())]);
    assert!(base.path().join("plugins/demo/demo.wasm").is_file());
    assert_eq!(server.fetches(), 2, "the checksum and the component");
    // The broken entry cost only itself, and says why.
    let failed = loader.failed_loads();
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0].name, "broken");

    // The next boot: same declarations, everything already on disk.
    let warm = Server::release("example/demo", "demo", "v1.0.0");
    let loader = self::loader(base.path());
    loader.declare_from_config_file(&config);
    let modes = install(&loader, base.path(), &warm).await;
    assert_eq!(modes, [("demo".to_string(), "demo-mode".to_string())]);
    assert_eq!(warm.fetches(), 0, "a warm boot does no network I/O");
}

/// The plugin-manager fixture guest, or `None` when it wasn't built (skip).
fn guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/plugin-manager-guest",
        "/target/wasm32-wasip2/release/plugin_manager_guest.wasm"
    );
    std::fs::read(path).ok()
}

/// `init.rs` and `lattice.toml` naming the same plugin: one spec reaches the
/// install, and it is `init.rs`'s, whole. The guest declares `release-demo`
/// from `example/release-demo`; the file names a different repository.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_rs_wins_a_plugin_both_files_declare() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: plugin-manager fixture guest not built");
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let init_dir = base.path().join("init");
    std::fs::create_dir_all(&init_dir).unwrap();
    std::fs::write(
        init_dir.join("plugin.toml"),
        "id = \"init\"\nprovides = [\"plugin-manager\"]\n",
    )
    .unwrap();
    std::fs::write(init_dir.join("init.wasm"), &wasm).unwrap();
    let config = write_config(
        base.path(),
        "[plugins]\nrelease-demo = \"toml/release-demo@v9.9.9\"\ntoml-only = \"owner/toml-only\"\n",
    );

    let loader = loader(base.path());
    // Boot's order: the file first, then init.rs.
    loader.declare_from_config_file(&config);
    loader
        .load_path(&init_dir, TrustTier::Bundled)
        .await
        .expect("init loads");

    let specs = loader.take_required();
    let release_demo: Vec<_> = specs.iter().filter(|s| s.name == "release-demo").collect();
    assert_eq!(release_demo.len(), 1, "one spec per plugin: {specs:?}");
    assert_eq!(
        release_demo[0].source,
        PluginSource::Release {
            repo: "example/release-demo".into(),
            version: Some("v0.2.0".into()),
            sha256: Some("ab".repeat(32)),
        },
        "init.rs's spec, not the file's"
    );
    assert!(specs.iter().any(|s| s.name == "toml-only"));
    // What `:plugins` will say declared each one follows the same precedence.
    use lattice_plugin_loader::DeclaredBy;
    assert_eq!(loader.declared_by("release-demo"), Some(DeclaredBy::InitRs));
    assert_eq!(
        loader.declared_by("toml-only"),
        Some(DeclaredBy::LatticeToml)
    );
    assert_eq!(loader.declared_by("never-declared"), None);
    assert_eq!(
        specs.len(),
        5,
        "the guest's four, plus the file's other one"
    );
}

/// A user configured by `lattice.toml` alone adds a plugin and runs
/// `:reload-config`: it installs, nothing is reported about an `init.rs` they
/// never wrote, and a second reload with nothing changed fetches nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reload_config_with_no_init_rs_installs_a_newly_declared_plugin() {
    let base = tempfile::tempdir().unwrap();
    let no_init = base.path().join("init");
    let config = write_config(base.path(), "tabstop = 4\n");
    let server = Server::release("example/demo", "demo", "v1.0.0");
    let loader = loader_with(base.path(), Some(env(base.path(), &server)));

    // Boot: the file declares nothing yet.
    loader.declare_from_config_file(&config);
    assert!(install(&loader, base.path(), &server).await.is_empty());

    std::fs::write(
        &config,
        "[plugins.demo]\nrelease = \"example/demo\"\nversion = \"v1.0.0\"\n",
    )
    .unwrap();
    let report = loader.reload_config_at(&no_init).await.unwrap();
    assert_eq!(report.build, ConfigBuildStatus::NoInit);
    assert_eq!(report.id, None);
    assert!(report.applied_new_config());
    assert!(base.path().join("plugins/demo/demo.wasm").is_file());
    assert_eq!(server.fetches(), 2);
    assert!(
        !loader.failed_loads().iter().any(|f| f.name == "init"),
        "no init.rs is not an init failure: {:?}",
        loader.failed_loads()
    );

    loader.reload_config_at(&no_init).await.unwrap();
    assert_eq!(
        server.fetches(),
        2,
        "an unchanged declaration fetches nothing"
    );
}

/// A rejected entry the user then fixes, or deletes, stops being listed as
/// broken on the next read; a still-broken one stays.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reread_replaces_what_the_previous_read_rejected() {
    let base = tempfile::tempdir().unwrap();
    let no_init = base.path().join("init");
    let config = write_config(base.path(), "[plugins]\nfixed = 1\ngone = 2\nstill = 3\n");
    let server = Server::default();
    let loader = loader_with(base.path(), Some(env(base.path(), &server)));
    loader.declare_from_config_file(&config);
    let names = |l: &PluginLoader| -> Vec<String> {
        l.failed_loads().into_iter().map(|f| f.name).collect()
    };
    assert_eq!(names(&loader), ["fixed", "gone", "still"]);

    std::fs::write(&config, "[plugins]\nfixed = \"owner/fixed\"\nstill = 3\n").unwrap();
    loader.reload_config_at(&no_init).await.unwrap();
    let failed = names(&loader);
    assert!(failed.contains(&"still".to_string()), "{failed:?}");
    assert!(!failed.contains(&"gone".to_string()), "{failed:?}");
    // `fixed` is now a valid declaration; whatever its install did, it is no
    // longer the parser's rejection.
    assert!(
        !loader
            .failed_loads()
            .iter()
            .any(|f| f.name == "fixed" && f.dir == config),
        "{:?}",
        loader.failed_loads()
    );
}
