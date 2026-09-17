//! PC.4 — the `project` bundled plugin remembers a project when a file in it
//! opens, driven through the real component.
//!
//! Design: `docs/dev/architecture/project-commands.md` §4. Slice plan:
//! `docs/dev/operations/slice-plans/archive/project-commands.md` PC.4.
//!
//! ## Why this test exists in this shape
//!
//! The guest's list semantics — order, dedupe, bounds, encoding — are unit
//! tested inside the plugin (`plugins/project/src/projects.rs`), and those tests
//! prove nothing at all about whether the seams fire. This one asserts the part
//! that only a real host can: that `register-events` actually subscribed, that a
//! published `DocumentOpened` reaches the guest's `on-event`, that
//! `project.root-for-buffer` answers from inside WASM, and that `store-put`
//! lands under the plugin's `state:write` grant.
//!
//! Every one of those is a seam that can be wired end to end and still answer
//! nothing — the failure mode this repo has hit repeatedly. A test that only
//! exercised the list logic would pass against a plugin that never subscribed.
//!
//! Delivery is drained DETERMINISTICALLY (publish → unsubscribe → `actor.run()`),
//! the `event_source.rs` harness. No sleeping, so the test cannot be flaky under
//! a loaded machine.
//!
//! Skips when the component was not built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lattice_mode::{BufferStoreHandle, CapabilitySet};
use lattice_plugin_host::{Capability, PluginBudget, PluginHost, PluginManifest, TrustTier};
use lattice_protocol::Event;
use lattice_protocol::ids::DocumentId;
use lattice_runtime::EventBus;
use tempfile::TempDir;

/// Must match `plugins/project/plugin.toml`'s `id` — the store is keyed by it,
/// so a mismatch would read an empty store and the test would fail for the
/// wrong reason.
const PLUGIN_ID: &str = "project";

/// The store key the guest writes. Mirrors `STORE_KEY` in the plugin; the host
/// never interprets the value, so this test owns the decode.
const STORE_KEY: &str = "projects";

fn plugin_wasm() -> Option<&'static str> {
    let path = env!("PROJECT_PLUGIN_WASM");
    (!path.is_empty()).then_some(path)
}

/// A buffer store answering one buffer with one path — what
/// `project.root-for-buffer` walks up from.
struct OneBuffer {
    path: PathBuf,
}

impl lattice_mode::BufferStore for OneBuffer {
    // `path_for` is the only method the project resolver reads. The rest are
    // stubs for the `project_seam.rs` reason: faking `handle_for` would mean
    // standing up a Document for a test that never reads one.
    fn find_by_name(&self, _name: &str) -> Option<lattice_core::BufferId> {
        None
    }
    fn handle_for(
        &self,
        _id: lattice_core::BufferId,
    ) -> Option<Arc<dyn lattice_runtime::Document>> {
        None
    }
    /// **`None`, because that is what a file-backed buffer answers.**
    ///
    /// `name` is the SYNTHETIC-name slot — `*magit:status*`, `*messages*` — and
    /// the trait says so: "`None` when the buffer is unnamed (the default for
    /// path-less scratch documents)". A buffer opened from a file is identified
    /// by its path and carries no name at all.
    ///
    /// This stub used to answer `Some("the-buffer")` so that
    /// `root_for_buffer`'s `name_for(id)?` existence check would pass. That made
    /// the whole suite green against a production path where it can never pass,
    /// and the feature was dead in the real editor: every `document-opened` for
    /// a real file resolved to `none`, nothing was ever remembered, and
    /// `:project-switch` said "no projects remembered yet" forever.
    fn name_for(&self, _id: lattice_core::BufferId) -> Option<String> {
        None
    }
    /// The existence oracle the resolver actually asks. One buffer, so every id
    /// is this one — the revisit test opens ids 1, 2 and 3 in one project.
    fn contains_buffer(&self, _id: lattice_core::BufferId) -> bool {
        true
    }
    /// Every id answers the same path — the revisit test opens ids 1, 2 and 3
    /// in ONE project on purpose, so gating on a single id would make it assert
    /// nothing.
    fn path_for(&self, _id: lattice_core::BufferId) -> Option<PathBuf> {
        Some(self.path.clone())
    }
    fn insert_document_buffer(
        &self,
        _id: lattice_core::BufferId,
        _kind: lattice_core::BufferKind,
        _handle: Arc<dyn lattice_runtime::Document>,
        _flags: lattice_core::BufferFlags,
        _name: Option<String>,
    ) {
    }
}

/// A temp repo: a directory carrying a `.git` marker, and a file inside it.
fn repo_with_file() -> (TempDir, PathBuf, PathBuf) {
    let dir = TempDir::new().unwrap();
    // `canonicalize` because macOS hands back `/var/...` for a tempdir and the
    // resolver reports `/private/var/...`; comparing the two would fail for a
    // reason that has nothing to do with this feature.
    let root = dir.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let file = root.join("src").join("main.rs");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "fn main() {}\n").unwrap();
    (dir, root, file)
}

/// The remembered list as the guest stored it — one absolute path per line,
/// most-recently-visited first.
fn remembered(host: &PluginHost) -> Vec<String> {
    host.plugin_store_get(PLUGIN_ID, STORE_KEY)
        .map(|bytes| {
            String::from_utf8_lossy(&bytes)
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn opened(id: u64, path: &Path) -> Event {
    Event::DocumentOpened {
        id: DocumentId::new(id),
        path: Some(path.to_path_buf()),
        version: 0,
        text: String::new(),
    }
}

/// The whole seam chain: subscribe → deliver → resolve → store.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opening_a_file_remembers_its_project() {
    let Some(wasm) = plugin_wasm() else {
        eprintln!("SKIP: project plugin not built (add the wasm32-wasip2 target)");
        return;
    };
    let (_repo_guard, root, file) = repo_with_file();
    let dir = TempDir::new().unwrap();
    let data_base = dir.path().join("data");
    let host = PluginHost::with_dirs(dir.path().join("cache"), &data_base).expect("host builds");

    let resolver: lattice_core::ProjectResolverHandle = Arc::new(
        lattice_core::MarkerResolver::with_default_markers(root.clone()),
    );
    let store = BufferStoreHandle::new(Arc::new(OneBuffer { path: file.clone() }));
    host.set_project_context(resolver, store);

    let component = host
        .compile(&std::fs::read(wasm).unwrap())
        .expect("compile the project plugin");
    // `state:write` is the plugin's ONLY capability, and it is what the store
    // write below is gated on. Declared here exactly as `plugin.toml` does, so
    // a manifest that stopped requesting it would fail this test rather than
    // silently stop persisting.
    let manifest = PluginManifest::new(
        PLUGIN_ID,
        vec![Capability::StateWrite],
        CapabilitySet::empty(),
    );
    let bus = Arc::new(EventBus::new());

    let (sub_ids, actor) = host
        .spawn_event_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::event(),
            &bus,
            None,
        )
        .await
        .expect("spawn the project plugin's event seam");
    assert_eq!(
        sub_ids.len(),
        1,
        "the plugin subscribes exactly one handler — document-opened. Zero here \
         means `register-events` never ran, which is the failure a list-logic \
         test cannot see"
    );

    bus.publish(opened(1, &file));
    for id in sub_ids {
        bus.unsubscribe(id);
    }
    actor.run().await;

    assert_eq!(
        remembered(&host),
        vec![root.to_string_lossy().to_string()],
        "opening a file inside a project remembers that project's ROOT — not the \
         file, and not its directory"
    );
}

/// A buffer with no path on disk resolves to `kind = pwd`, and "not in a
/// project" must not enter a list of projects — or the working directory would
/// sit in front of the user forever.
///
/// **A real open is published FIRST, deliberately.** Asserting only that the
/// store is empty after a pathless open would pass against a plugin that never
/// subscribed, never resolved, or could not write at all — an assertion carried
/// entirely by a `None` path proves nothing. Remembering one project and then
/// declining the pathless one asserts the refusal is a DECISION rather than a
/// dead seam.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pathless_buffer_is_not_remembered() {
    let Some(wasm) = plugin_wasm() else {
        eprintln!("SKIP: project plugin not built (add the wasm32-wasip2 target)");
        return;
    };
    let (_repo_guard, root, file) = repo_with_file();
    let dir = TempDir::new().unwrap();
    let data_base = dir.path().join("data");
    let host = PluginHost::with_dirs(dir.path().join("cache"), &data_base).expect("host builds");

    let resolver: lattice_core::ProjectResolverHandle = Arc::new(
        lattice_core::MarkerResolver::with_default_markers(root.clone()),
    );
    let file_for_open = file.clone();
    let store = BufferStoreHandle::new(Arc::new(OneBuffer { path: file }));
    host.set_project_context(resolver, store);

    let component = host.compile(&std::fs::read(wasm).unwrap()).unwrap();
    let manifest = PluginManifest::new(
        PLUGIN_ID,
        vec![Capability::StateWrite],
        CapabilitySet::empty(),
    );
    let bus = Arc::new(EventBus::new());
    let (sub_ids, actor) = host
        .spawn_event_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::event(),
            &bus,
            None,
        )
        .await
        .unwrap();

    bus.publish(opened(1, &file_for_open));
    bus.publish(Event::DocumentOpened {
        id: DocumentId::new(7),
        path: None,
        version: 0,
        text: String::new(),
    });
    for id in sub_ids {
        bus.unsubscribe(id);
    }
    actor.run().await;

    assert_eq!(
        remembered(&host),
        vec![root.to_string_lossy().to_string()],
        "the real open was remembered and the pathless one was declined — one \
         entry, not two and not zero"
    );
}

/// Re-visiting MOVES a project to the front rather than duplicating it. The
/// property is unit tested in the guest; this asserts it survives the round trip
/// through the store, which is where a load-modify-save could lose it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revisiting_does_not_duplicate() {
    let Some(wasm) = plugin_wasm() else {
        eprintln!("SKIP: project plugin not built (add the wasm32-wasip2 target)");
        return;
    };
    let (_repo_guard, root, file) = repo_with_file();
    let dir = TempDir::new().unwrap();
    let data_base = dir.path().join("data");
    let host = PluginHost::with_dirs(dir.path().join("cache"), &data_base).expect("host builds");

    let resolver: lattice_core::ProjectResolverHandle = Arc::new(
        lattice_core::MarkerResolver::with_default_markers(root.clone()),
    );
    let store = BufferStoreHandle::new(Arc::new(OneBuffer { path: file.clone() }));
    host.set_project_context(resolver, store);

    let component = host.compile(&std::fs::read(wasm).unwrap()).unwrap();
    let manifest = PluginManifest::new(
        PLUGIN_ID,
        vec![Capability::StateWrite],
        CapabilitySet::empty(),
    );
    let bus = Arc::new(EventBus::new());
    let (sub_ids, actor) = host
        .spawn_event_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::event(),
            &bus,
            None,
        )
        .await
        .unwrap();

    bus.publish(opened(1, &file));
    bus.publish(opened(2, &file));
    bus.publish(opened(3, &file));
    for id in sub_ids {
        bus.unsubscribe(id);
    }
    actor.run().await;

    assert_eq!(
        remembered(&host).len(),
        1,
        "three opens in one project are one entry"
    );
}
