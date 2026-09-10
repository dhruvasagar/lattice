//! PC.5 — the `projects` picker, driven through the real component.
//!
//! Design: `docs/dev/architecture/project-commands.md` §5. Slice plan:
//! `docs/dev/operations/slice-plans/project-commands.md` PC.5.
//!
//! ## The assertion that matters is cross-seam
//!
//! The remembered list is written on the **events** seam (`document-opened`) and
//! read on the **picker** seam. Those are two guest instances of one component:
//! a `thread_local` written in one is invisible in the other, which this repo
//! has been bitten by before. The list survives the crossing only because it
//! lives in the HOST-side store, which is precisely why the design put it there.
//!
//! So these tests populate through the event seam and read through the picker
//! seam, on one host. A test that populated and read on the same seam would pass
//! against a guest-memory implementation and prove nothing about the design.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;

use lattice_core::Buffer;
use lattice_mode::{BufferStoreHandle, CapabilitySet};
use lattice_picker::context::{ActiveBufferSnapshot, PickerContext};
use lattice_picker::outcome::PickerAcceptOutcome;
use lattice_picker::{PickerInitResult, PickerSourceGenerator, RoutingPayload};
use lattice_plugin_host::{
    Capability, PluginBudget, PluginHost, PluginManifest, TrustTier, WasmPickerSource,
};
use lattice_protocol::Event;
use lattice_protocol::ids::DocumentId;
use lattice_protocol::position::Position;
use lattice_runtime::EventBus;
use tempfile::TempDir;

const PLUGIN_ID: &str = "project";

fn plugin_wasm() -> Option<&'static str> {
    let path = env!("PROJECT_PLUGIN_WASM");
    (!path.is_empty()).then_some(path)
}

fn manifest() -> PluginManifest {
    PluginManifest::new(
        PLUGIN_ID,
        vec![Capability::StateWrite],
        CapabilitySet::empty(),
    )
}

/// Many buffers, one per remembered file — the real editor's shape, and the one
/// that lets a single resolver serve every project in the test.
///
/// `name_for` must answer `Some`: the host uses it as the EXISTENCE oracle in
/// `root_for_buffer` and short-circuits on `None`, which silently makes every
/// resolution fail and every list come back empty.
struct Buffers {
    paths: Vec<PathBuf>,
}

impl Buffers {
    /// Ids are 1-based, matching the `DocumentOpened` ids published below.
    fn path(&self, id: lattice_core::BufferId) -> Option<&PathBuf> {
        self.paths.get((id.0 as usize).checked_sub(1)?)
    }
}

impl lattice_mode::BufferStore for Buffers {
    fn find_by_name(&self, _name: &str) -> Option<lattice_core::BufferId> {
        None
    }
    fn handle_for(
        &self,
        _id: lattice_core::BufferId,
    ) -> Option<Arc<dyn lattice_runtime::Document>> {
        None
    }
    fn name_for(&self, id: lattice_core::BufferId) -> Option<String> {
        self.path(id).map(|_| format!("buffer-{}", id.0))
    }
    fn path_for(&self, id: lattice_core::BufferId) -> Option<PathBuf> {
        self.path(id).cloned()
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

fn marker_repo(parent: &std::path::Path, name: &str) -> (PathBuf, PathBuf) {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join(".git")).unwrap();
    let file = root.join("src").join("main.rs");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "fn main() {}\n").unwrap();
    (root, file)
}

/// Remember each of `files` (in order) by publishing a `document-opened` for it
/// through the real events seam.
///
/// ONE host, ONE resolver, ONE plugin instance — the editor's actual shape.
/// Re-spawning a seam per file and re-pointing the project context each time
/// silently remembered only the first, which is a property of the harness and
/// not of the plugin.
async fn remember_through_events(host: &PluginHost, files: &[PathBuf]) {
    let resolver: lattice_core::ProjectResolverHandle =
        Arc::new(lattice_core::MarkerResolver::with_default_markers(
            files[0].parent().unwrap().to_path_buf(),
        ));
    let store = BufferStoreHandle::new(Arc::new(Buffers {
        paths: files.to_vec(),
    }));
    host.set_project_context(resolver, store);

    let component = host
        .compile(&std::fs::read(plugin_wasm().unwrap()).unwrap())
        .unwrap();
    let bus = Arc::new(EventBus::new());
    let (sub_ids, actor) = host
        .spawn_event_plugin(
            &component,
            &manifest(),
            TrustTier::Bundled,
            PluginBudget::event(),
            &bus,
            None,
        )
        .await
        .unwrap();
    assert_eq!(sub_ids.len(), 1, "the plugin subscribed document-opened");
    for (n, file) in files.iter().enumerate() {
        bus.publish(Event::DocumentOpened {
            id: DocumentId::new(n as u64 + 1),
            path: Some(file.clone()),
            version: 0,
            text: String::new(),
        });
    }
    for id in sub_ids {
        bus.unsubscribe(id);
    }
    actor.run().await;
}

/// Connect the picker seam of the same component, on the same host.
async fn connect_picker(host: &PluginHost) -> WasmPickerSource {
    let component = host
        .compile(&std::fs::read(plugin_wasm().unwrap()).unwrap())
        .unwrap();
    let (client, actor) = host
        .spawn_picker_source(
            &component,
            &manifest(),
            TrustTier::Bundled,
            PluginBudget::default(),
            &Arc::new(EventBus::new()),
            None,
        )
        .await
        .unwrap();
    tokio::spawn(actor.run());
    WasmPickerSource::connect_all(client)
        .await
        .expect("registration reaches the guest")
        .into_iter()
        .next()
        .expect("the plugin declares the projects source")
}

fn with_ctx<R>(f: impl FnOnce(&PickerContext<'_>) -> R) -> R {
    let buffer = Buffer::empty();
    let ctx = PickerContext {
        active_buffer: ActiveBufferSnapshot {
            buffer_id: 0,
            path: None,
            language: None,
            cursor: Position::new(0, 0),
            selection: None,
            buffer: &buffer,
            syntax_symbols: Vec::new(),
            syntax_highlights: Vec::new(),
        },
        workspace_root: "/ws".into(),
        recent_files: &[],
        position_history: Vec::new(),
        buffers: Vec::new(),
        marks: Vec::new(),
        registers: Vec::new(),
        yank_ring: Vec::new(),
        active_modes: Vec::new(),
        command_history: Vec::new(),
        search_history: Vec::new(),
        pane_buffer_history: Vec::new(),
    };
    f(&ctx)
}

/// The whole point: two projects remembered on the EVENT seam show up as rows on
/// the PICKER seam, most-recently-visited first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remembered_projects_become_picker_rows() {
    let Some(_) = plugin_wasm() else {
        eprintln!("SKIP: project plugin not built (add the wasm32-wasip2 target)");
        return;
    };
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().canonicalize().unwrap();
    let (alpha, alpha_file) = marker_repo(&workspace, "alpha");
    let (beta, beta_file) = marker_repo(&workspace, "beta");

    let host = PluginHost::with_dirs(tmp.path().join("cache"), tmp.path().join("data")).unwrap();
    remember_through_events(&host, &[alpha_file, beta_file]).await;

    let source = connect_picker(&host).await;
    assert_eq!(source.spec().id, "projects");
    assert!(!source.spec().live, "the list cannot change while open");

    let init = with_ctx(|ctx| source.init(ctx, &[])).expect("init returns a result");
    let batch = match init {
        PickerInitResult::Future(fut) => fut.await.expect("the guest produced rows"),
        other => panic!("expected Future, got {other:?}"),
    };

    assert_eq!(batch.len(), 2, "one row per remembered project");
    assert_eq!(
        batch[0].0.display, "beta",
        "most-recently-visited first — the ordering is the reason the store keeps \
         a list rather than a set"
    );
    assert_eq!(batch[1].0.display, "alpha");
    // The path is part of the MATCHED text, not only the annotation:
    // annotations are shown, never matched, so a path-only-in-annotation row
    // would make two same-named checkouts indistinguishable to the matcher.
    assert!(
        batch[0].0.text.contains(beta.to_str().unwrap()),
        "the full path is searchable: {}",
        batch[0].0.text
    );
    assert!(
        !batch[0].0.annotations.is_empty(),
        "and shown as the annotation column"
    );
    assert!(batch[1].0.text.contains(alpha.to_str().unwrap()));
}

/// An accepted row routes into `:project-find-file <root>` carrying the root the
/// picker already resolved — the ex-command must not have to re-read the store
/// to learn a path the row was built from.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepting_a_row_routes_to_find_file_with_the_root() {
    let Some(_) = plugin_wasm() else {
        return;
    };
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().canonicalize().unwrap();
    let (alpha, alpha_file) = marker_repo(&workspace, "alpha");

    let host = PluginHost::with_dirs(tmp.path().join("cache"), tmp.path().join("data")).unwrap();
    remember_through_events(&host, &[alpha_file]).await;
    let source = connect_picker(&host).await;

    let init = with_ctx(|ctx| source.init(ctx, &[])).unwrap();
    let batch = match init {
        PickerInitResult::Future(fut) => fut.await.unwrap(),
        other => panic!("expected Future, got {other:?}"),
    };
    let routing = batch[0].1.clone();
    match &routing {
        RoutingPayload::InvokeCommand { id, .. } => {
            assert_eq!(id, "project-find-file");
        }
        other => panic!("expected an invoke-command routing, got {other:?}"),
    }

    let fut = with_ctx(|ctx| source.accept_async(ctx, &routing)).expect("accept_async");
    let outcome = fut.await.expect("the guest resolved the routing");
    match outcome {
        PickerAcceptOutcome::InvokeCommand { id, args } => {
            assert_eq!(id, "project-find-file");
            let args = format!("{args:?}");
            assert!(
                args.contains(alpha.to_str().unwrap()),
                "the root travels as the argument: {args}"
            );
        }
        other => panic!("expected invoke-command, got {other:?}"),
    }
}

/// An empty list is an `err` the host echoes, not an empty picker.
///
/// "Nothing remembered yet" and "the feature is broken" look identical in an
/// empty picker and have entirely different fixes — the `roam_find` rule.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_list_says_so_rather_than_opening_empty() {
    let Some(_) = plugin_wasm() else {
        return;
    };
    let tmp = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(tmp.path().join("cache"), tmp.path().join("data")).unwrap();
    let source = connect_picker(&host).await;

    let init = with_ctx(|ctx| source.init(ctx, &[])).expect("init result");
    let err = match init {
        PickerInitResult::Future(fut) => fut.await.expect_err("an empty list is an error"),
        other => panic!("expected Future, got {other:?}"),
    };
    assert!(
        err.contains("no projects remembered") && err.contains("project-remember"),
        "the message names the fix, not just the symptom: {err}"
    );
}
