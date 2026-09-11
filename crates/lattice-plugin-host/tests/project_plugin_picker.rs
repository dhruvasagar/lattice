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
/// `name_for` answers `None`, because that is what a file-backed buffer does —
/// `name` is the SYNTHETIC-name slot. This harness used to answer `Some` and
/// carried a comment calling that the host's "existence oracle"; it was not,
/// and the lie hid a defect where every real `document-opened` resolved to
/// `none` and the plugin remembered nothing in the actual editor. The oracle
/// is `contains_buffer`, below.
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
    fn name_for(&self, _id: lattice_core::BufferId) -> Option<String> {
        None
    }
    /// The real existence oracle: does this store know the id at all.
    fn contains_buffer(&self, id: lattice_core::BufferId) -> bool {
        self.path(id).is_some()
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

    assert_eq!(
        batch.len(),
        3,
        "one row per remembered project, plus PC.12's `\u{2026} (choose a dir)`"
    );
    assert_eq!(
        batch[2].0.display, "\u{2026} (choose a dir)",
        "pinned LAST, after every project — a row that could sort above a real \
         one would put 'go browsing' in front of 'the thing you already told \
         me about'"
    );
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

/// An accepted row routes into `:project-switch-to <root>` carrying the root the
/// picker already resolved — the ex-command must not have to re-read the store
/// to learn a path the row was built from.
///
/// **PC.6 changed what it routes INTO**, and this test is what caught it: the
/// accept opened find-file directly until the switch-commands menu existed, and
/// now opens the menu. Two hops either way, because `picker-accept-outcome` has
/// no arm for opening a transient and should not grow one — an accept resolves
/// to a typed outcome, and opening a menu is an effect.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepting_a_row_routes_to_the_switch_menu_with_the_root() {
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
            assert_eq!(id, "project-switch-to");
        }
        other => panic!("expected an invoke-command routing, got {other:?}"),
    }

    let fut = with_ctx(|ctx| source.accept_async(ctx, &routing)).expect("accept_async");
    let outcome = fut.await.expect("the guest resolved the routing");
    match outcome {
        PickerAcceptOutcome::InvokeCommand { id, args } => {
            assert_eq!(id, "project-switch-to");
            let args = format!("{args:?}");
            assert!(
                args.contains(alpha.to_str().unwrap()),
                "the root travels as the argument: {args}"
            );
        }
        other => panic!("expected invoke-command, got {other:?}"),
    }
}

/// **PC.12 reversed this test, and the rule it was built on is still right.**
///
/// It used to assert that an empty list is an `err` the host echoes rather
/// than an empty picker, on the `roam_find` rule: "nothing remembered yet" and
/// "the feature is broken" look identical in an empty picker and have
/// entirely different fixes.
///
/// That rule is about a picker with nothing to OFFER. This one now always
/// carries `… (choose a dir)`, and a fresh install is exactly when that row is
/// the whole point — refusing to open put the escape hatch behind the wall it
/// exists to get through, telling the user to run a command instead of handing
/// them the thing that runs it.
///
/// So the assertion flips but the principle does not: an empty list is still
/// self-describing. It shows no projects, and one row offering to find one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_list_opens_on_the_row_that_fixes_it() {
    let Some(_) = plugin_wasm() else {
        return;
    };
    let tmp = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(tmp.path().join("cache"), tmp.path().join("data")).unwrap();
    let source = connect_picker(&host).await;

    let init = with_ctx(|ctx| source.init(ctx, &[])).expect("init result");
    let batch = match init {
        PickerInitResult::Future(fut) => fut.await.expect("an empty list is not an error"),
        other => panic!("expected Future, got {other:?}"),
    };

    assert_eq!(
        batch.len(),
        1,
        "no projects, and exactly one row — the way to get one"
    );
    assert_eq!(batch[0].0.display, "\u{2026} (choose a dir)");
    match &batch[0].1 {
        RoutingPayload::InvokeCommand { id, .. } => assert_eq!(
            id, "project-choose-dir",
            "and it is wired to the command that opens the directory picker — \
             a row naming an unregistered command is one that silently does \
             nothing, which is what this whole flow exists to avoid"
        ),
        other => panic!("expected invoke-command, got {other:?}"),
    }
}

/// PC.12: run one of the plugin's ex-commands through the REAL grammar seam on
/// `host`, so it sees the same store and the same project context the rest of
/// the test wired up.
///
/// The grammar seam is a third instance of the same component, beside the
/// events and picker seams above — which is the point: `remember_root` writes
/// through the HOST store, so a value written here is visible to the picker
/// seam. A helper that reached into guest memory would prove nothing.
fn apply_ex(host: &PluginHost, name: &str, arg: &str) -> lattice_grammar::Effect {
    let component = host
        .compile(&std::fs::read(plugin_wasm().unwrap()).unwrap())
        .unwrap();
    let set = host
        .instantiate_grammar_plugin(
            &component,
            &manifest(),
            TrustTier::Bundled,
            &Arc::new(EventBus::new()),
            None,
            None,
        )
        .expect("instantiate + register-grammar");
    let mut registry = lattice_grammar::CommandRegistry::new();
    set.register_all(&mut registry);
    let id = registry.id_by_name(name).unwrap_or_else(|| {
        panic!(
            "`{name}` is registered — an unregistered command is a row that silently does nothing"
        )
    });
    let mut document = lattice_core::Document::from_text("");
    lattice_grammar::dispatcher::execute(
        &registry,
        &mut document,
        lattice_core::BufferId(1),
        Position::new(0, 0),
        lattice_grammar::CommandInvocation::of(id)
            .with_args(lattice_grammar::Args::String(arg.to_string())),
        &lattice_protocol::CancellationToken::never(),
    )
    .expect("the command dispatches")
}

/// PC.12 — the first hop: `:project-choose-dir` opens `dir-pick`, and names
/// where the answer goes.
///
/// The `fill-action` is asserted, not just the open. A sub-picker that opens
/// without one is worse than one that does not open: the user browses, picks a
/// directory, and gets `picker: nothing was waiting for a value` — a dead end
/// one hop further in, where it is much harder to recognise as the same bug.
///
/// The other half of this hop — the host actually applying the effect — is
/// `lattice-host`'s `choose_a_dir_reaches_its_picker`. It has to be a separate
/// test because the guest returning the right effect and the host applying it
/// are different claims, and for a while only the first one was true.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn choose_dir_opens_the_directory_picker_naming_where_the_answer_goes() {
    let Some(_) = plugin_wasm() else {
        eprintln!("SKIP: project plugin not built (add the wasm32-wasip2 target)");
        return;
    };
    let tmp = TempDir::new().unwrap();
    let host = PluginHost::with_dirs(tmp.path().join("cache"), tmp.path().join("data")).unwrap();

    let effects = apply_ex(&host, "project-choose-dir", "");

    let rendered = format!("{effects:?}");
    assert!(
        rendered.contains("OpenPicker") && rendered.contains("dir-pick"),
        "the row opens the directory sub-picker. Got: {rendered}"
    );
    assert!(
        rendered.contains("project-remember-and-switch"),
        "…naming the command the picked directory is handed to (PC.11's \
         fill-action — the only destination a guest can own). Got: {rendered}"
    );
}

/// PC.12 — **choosing a directory must remember it AND open the menu.**
///
/// Both halves asserted together, deliberately. Remembering without the menu
/// and the menu without remembering are each half the feature, and each looks
/// perfectly fine on its own: the first leaves you staring at the project you
/// came from with a silently-updated list, the second drops you into a menu
/// for a project that will be gone next launch. Only asserting both catches
/// either.
///
/// This drives the guest's `apply-ex-command` for
/// `project-remember-and-switch` — the command PC.11's `fill-action` names, so
/// this is the hop the directory picker's value actually lands on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn choosing_a_directory_remembers_it_and_opens_the_switch_menu() {
    let Some(_) = plugin_wasm() else {
        eprintln!("SKIP: project plugin not built (add the wasm32-wasip2 target)");
        return;
    };
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().canonicalize().unwrap();
    let (gamma, gamma_file) = marker_repo(&workspace, "gamma");
    let host = PluginHost::with_dirs(tmp.path().join("cache"), tmp.path().join("data")).unwrap();

    // Nothing remembered, and the project context wired the way the editor
    // wires it — so `project-of-path` can actually resolve.
    let resolver: lattice_core::ProjectResolverHandle = Arc::new(
        lattice_core::MarkerResolver::with_default_markers(workspace.clone()),
    );
    host.set_project_context(
        resolver,
        BufferStoreHandle::new(Arc::new(Buffers { paths: Vec::new() })),
    );
    assert!(
        host.plugin_store_get(PLUGIN_ID, "projects").is_none(),
        "precondition: nothing remembered yet"
    );

    // A path INSIDE the project, not its root — the case that separates this
    // command from `project-switch-to`. A directory walk lands you wherever
    // you stopped, and storing that verbatim would put a subdirectory in the
    // project list and root every later switch one level too deep.
    let inside = gamma_file.parent().unwrap().to_path_buf();
    let effects = apply_ex(
        &host,
        "project-remember-and-switch",
        &inside.to_string_lossy(),
    );

    let remembered = host
        .plugin_store_get(PLUGIN_ID, "projects")
        .map(|b| String::from_utf8_lossy(&b).trim().to_string())
        .unwrap_or_default();
    assert_eq!(
        remembered,
        gamma.to_string_lossy(),
        "the project ROOT is remembered, not the subdirectory that was chosen"
    );

    let rendered = format!("{effects:?}");
    assert!(
        rendered.contains("OpenTransient") && rendered.contains("project-switch"),
        "and the switch-commands menu opens in the same breath — one hop, which \
         is project.el's shape: it does not hand you back to the project list \
         to confirm a directory you just chose. Got: {rendered}"
    );
}

/// A directory with no root marker above it is refused, and says why. The
/// picker cannot know what is a project — the plugin holds no `fs:` grant — so
/// the refusal has to happen here, at the one place that can ask the host.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_directory_that_is_not_a_project_is_refused_with_a_reason() {
    let Some(_) = plugin_wasm() else {
        return;
    };
    let tmp = TempDir::new().unwrap();
    let workspace = tmp.path().canonicalize().unwrap();
    let orphan = workspace.join("not-a-project");
    std::fs::create_dir_all(&orphan).unwrap();
    let host = PluginHost::with_dirs(tmp.path().join("cache"), tmp.path().join("data")).unwrap();

    // A resolver rooted at a directory with no marker anywhere above it.
    let resolver: lattice_core::ProjectResolverHandle =
        Arc::new(lattice_core::MarkerResolver::new(
            vec!["never-a-marker-xyz".to_string()],
            workspace.clone(),
        ));
    host.set_project_context(
        resolver,
        BufferStoreHandle::new(Arc::new(Buffers { paths: Vec::new() })),
    );

    let effects = apply_ex(
        &host,
        "project-remember-and-switch",
        &orphan.to_string_lossy(),
    );

    let rendered = format!("{effects:?}");
    assert!(
        rendered.contains("not inside a project"),
        "the refusal names the reason: {rendered}"
    );
    assert!(
        !rendered.contains("OpenTransient"),
        "and nothing opens — a menu for a project that was not remembered is \
         the worse half of a half-working feature: {rendered}"
    );
    assert!(
        host.plugin_store_get(PLUGIN_ID, "projects").is_none(),
        "and nothing is stored"
    );
}
