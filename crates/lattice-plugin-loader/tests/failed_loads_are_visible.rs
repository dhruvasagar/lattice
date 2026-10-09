//! WT.4 — a plugin that fails to load is *reported*, not merely absent.
//!
//! Design: [`wit-ownership.md`](../../../docs/dev/architecture/wit-ownership.md) §4.
//!
//! The failure this exists for: three WIT changes landed in a day, the user's
//! `init.wasm` stopped instantiating, the `require("org")` inside it never ran,
//! org never loaded — and nothing anywhere said so. The editor opened, the file
//! opened, and org was simply gone: no language, no highlighting, no folds, no
//! chords. **A plugin that failed to load was indistinguishable from one that
//! had never been installed**, which is why finding it took a debugging session
//! instead of a glance at `:plugins`.
//!
//! Mostly fixture-free: a component the host cannot compile is enough to drive
//! the recording paths, and invalid bytes rather than a real `.wasm` means those
//! run on a machine with no `wasm32-wasip2` target. The one exception is the
//! *clearing* test, which needs a load that genuinely succeeds and so skips
//! without the canonical `modes-guest` fixture — a test-only clear hook would
//! have proved the bookkeeping while leaving the wiring untested, and the wiring
//! is the half that breaks.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_grammar::CommandRegistryHandle;
use lattice_grammar::registry::CommandRegistry;
use lattice_keymap::KeymapHandle;
use lattice_mode::{ModeRegistry, ModeRegistryHandle};
use lattice_plugin_host::{PluginHost, TrustTier};
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_runtime::EventBus;

fn host(base: &std::path::Path) -> Arc<PluginHost> {
    Arc::new(PluginHost::with_dirs(base.join("cache"), base.join("data")).expect("host builds"))
}

/// The lifecycle-spine loader: enough to compile and instantiate, which is all
/// a component that fails to compile ever reaches.
fn loader(base: &std::path::Path) -> PluginLoader {
    PluginLoader::new(host(base))
}

/// Wired for the `modes` seam — needed only by the test that drives a real
/// successful load through to the clear.
fn seam_loader(base: &std::path::Path) -> PluginLoader {
    let mut commands = CommandRegistry::new();
    let _ = lattice_grammar::ex_commands::populate(&mut commands);
    let modes: ModeRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()));
    PluginLoader::with_services(
        host(base),
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            command_registry: Some(
                Arc::new(arc_swap::ArcSwap::from_pointee(commands)) as CommandRegistryHandle
            ),
            mode_registry: Some(modes),
            keymap: Some(KeymapHandle::new()),
            ..Default::default()
        },
    )
}

fn modes_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/modes-guest/target/wasm32-wasip2/release/modes_guest.wasm"
    );
    std::fs::read(path).ok()
}

/// A plugin directory whose component will not compile.
fn write_broken_plugin(root: &std::path::Path, id: &str) -> std::path::PathBuf {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plugin.toml"), format!("id = \"{id}\"\n")).unwrap();
    std::fs::write(dir.join("component.wasm"), b"not a component at all").unwrap();
    dir
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugin_that_fails_to_load_is_recorded_with_its_reason() {
    let base = tempfile::tempdir().unwrap();
    let plugins = base.path().join("plugins");
    let dir = write_broken_plugin(&plugins, "org");

    let loader = loader(base.path());
    let loaded = loader
        .discover_and_load(&plugins, TrustTier::UserInstalled)
        .await;

    assert_eq!(loaded, 0, "it did not load");
    let failed = loader.failed_loads();
    assert_eq!(failed.len(), 1, "and it did not vanish: {failed:?}");
    assert_eq!(failed[0].name, "org", "named, so the user knows which one");
    assert_eq!(
        failed[0].dir, dir,
        "with the copy on disk to go and look at"
    );
    assert!(
        !failed[0].error.is_empty(),
        "and a reason, which is the half a bare `absent` never had"
    );
}

/// The lie this guards against is a stale one: a plugin the user has since
/// fixed must not keep a "failed" row, or the report becomes noise that gets
/// skipped — which is exactly how the original message failed to be read.
///
/// Drives the *real* clear path — a genuine successful load — rather than
/// reaching for a test-only hook, so it needs the canonical `modes-guest`
/// fixture and skips without it. A hook would have proved the bookkeeping and
/// not the wiring, and the wiring is the part that goes wrong.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failure_is_cleared_when_the_plugin_later_loads() {
    let Some(wasm) = modes_guest_wasm() else {
        eprintln!("skipping: modes-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins = base.path().join("plugins");
    let dir = write_broken_plugin(&plugins, "modes-fixture");
    std::fs::write(
        dir.join("plugin.toml"),
        "id = \"modes-fixture\"\nprovides = [\"modes\"]\n",
    )
    .unwrap();

    let loader = seam_loader(base.path());
    loader
        .discover_and_load(&plugins, TrustTier::UserInstalled)
        .await;
    assert_eq!(
        loader.failed_loads().len(),
        1,
        "the broken component is on record"
    );

    // The user fixes it: a component that actually compiles.
    std::fs::write(dir.join("component.wasm"), &wasm).unwrap();
    let loaded = loader
        .discover_and_load(&plugins, TrustTier::UserInstalled)
        .await;

    assert_eq!(loaded, 1, "it loads now");
    assert!(
        loader.failed_loads().is_empty(),
        "and the failed row goes with it — a fixed plugin leaves no residue"
    );
}

/// Retrying a broken plugin must leave ONE row describing what is wrong now,
/// not a growing pile of attempts. `:plugins` is a description of the current
/// state, not a log.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_failures_do_not_accumulate_rows() {
    let base = tempfile::tempdir().unwrap();
    let plugins = base.path().join("plugins");
    write_broken_plugin(&plugins, "org");

    let loader = loader(base.path());
    for _ in 0..3 {
        // `discover_and_load` skips what is already loaded; this one never
        // loads, so each pass genuinely retries it.
        loader
            .discover_and_load(&plugins, TrustTier::UserInstalled)
            .await;
    }
    assert_eq!(
        loader.failed_loads().len(),
        1,
        "one row, replaced — not three appended"
    );
}

/// A directory that is not a plugin at all is NOT a failed load. `:plugin-load`
/// on a wrong path is a mistake the user made and gets told about directly;
/// recording it would put a permanent row in `:plugins` for a plugin that does
/// not exist.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_path_that_is_not_a_plugin_is_not_recorded_as_a_failure() {
    let base = tempfile::tempdir().unwrap();
    let empty = base.path().join("just-a-folder");
    std::fs::create_dir_all(&empty).unwrap();

    let loader = loader(base.path());
    assert!(
        loader
            .load_path(&empty, TrustTier::UserInstalled)
            .await
            .is_err(),
        "the caller is told directly"
    );
    assert!(
        loader.failed_loads().is_empty(),
        "but nothing is filed against a plugin that does not exist"
    );
}

/// `:plugin-load` on a real-but-broken plugin DOES record — the trigger does not
/// change whether a plugin that should be here is missing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_explicit_load_of_a_broken_plugin_is_recorded() {
    let base = tempfile::tempdir().unwrap();
    let dir = write_broken_plugin(base.path(), "org");

    let loader = loader(base.path());
    assert!(
        loader
            .load_path(&dir, TrustTier::UserInstalled)
            .await
            .is_err()
    );

    let failed = loader.failed_loads();
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0].name, "org");
}

// ---------------------------------------------------------------------------
// A plugin whose source lives in its own directory is built by the scan, and a
// build that fails is reported — for ANY plugin, not only `init`.
// ---------------------------------------------------------------------------

/// A builder that fails the way rustc does, and counts how often it was asked.
struct FailingBuilder {
    calls: std::sync::atomic::AtomicUsize,
}

impl FailingBuilder {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
        })
    }
    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

const COMPILER_ERROR: &str = "cargo build failed (exit status: 101)\n\
    error[E0425]: cannot find value `Nope` in this scope\n  --> src/lib.rs:47:30";

impl lattice_plugin_loader::ComponentBuilder for FailingBuilder {
    fn build(&self, _source_dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(COMPILER_ERROR.to_string())
    }
}

/// What `--scaffold-plugin` leaves behind before its first build: a manifest
/// and a cargo project, and no component.
fn write_source_plugin(root: &std::path::Path, id: &str) -> std::path::PathBuf {
    let dir = root.join(id);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("plugin.toml"), format!("id = \"{id}\"\n")).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    std::fs::write(dir.join("src/lib.rs"), "// does not compile\n").unwrap();
    dir
}

/// The case this whole area was blind to. The scan used to build nothing and
/// log "skipping malformed plugin dir" — so the plugin a user was in the
/// middle of writing was the one whose build error `:plugins` could not show.
#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_whose_build_fails_is_reported_with_the_compiler_error() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let dir = write_source_plugin(&plugins, "mine");
    let loader = loader(tmp.path());
    let builder = FailingBuilder::new();

    let loaded = loader
        .discover_and_load_with(&plugins, TrustTier::UserInstalled, builder.clone())
        .await;

    assert_eq!(loaded, 0);
    assert_eq!(builder.calls(), 1, "the scan builds an in-place project");
    let failed = loader.failed_loads();
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0].name, "mine");
    assert_eq!(failed[0].dir, dir);
    // The CAUSE, not its consequence. "no .wasm in this directory" is true
    // and useless; the compiler error is what the author needs.
    assert!(
        failed[0].error.contains("error[E0425]"),
        "{}",
        failed[0].error
    );
    assert!(failed[0].error.contains("src/lib.rs:47:30"));
}

/// A manifest with nothing to load and nothing to build from is still a
/// plugin that should be here and is not.
#[tokio::test(flavor = "multi_thread")]
async fn a_plugin_directory_with_no_component_is_reported_not_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    let dir = plugins.join("empty");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plugin.toml"), "id = \"empty\"\n").unwrap();
    let loader = loader(tmp.path());
    let builder = FailingBuilder::new();

    loader
        .discover_and_load_with(&plugins, TrustTier::UserInstalled, builder.clone())
        .await;

    assert_eq!(builder.calls(), 0, "no cargo project, so nothing to build");
    let failed = loader.failed_loads();
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0].name, "empty");
    assert!(!failed[0].error.is_empty());
}

/// A prebuilt plugin — a manifest and a component, no cargo project — must
/// not wake the toolchain. That is every installed plugin on every boot.
#[tokio::test(flavor = "multi_thread")]
async fn a_prebuilt_plugin_is_never_built() {
    let tmp = tempfile::tempdir().unwrap();
    let plugins = tmp.path().join("plugins");
    write_broken_plugin(&plugins, "prebuilt");
    let loader = loader(tmp.path());
    let builder = FailingBuilder::new();

    loader
        .discover_and_load_with(&plugins, TrustTier::UserInstalled, builder.clone())
        .await;

    assert_eq!(builder.calls(), 0);
}
