//! PR.6 — the `project` guest→host seam, through a real guest.
//!
//! Design: `docs/dev/architecture/project-resolution.md` §6.
//!
//! Instantiates the `project-guest` fixture (a `wasm32-wasip2` base
//! `plugin`-world component) against a host wired with a real
//! `MarkerResolver` and a buffer store, calls `activate`, and reads the
//! answers back out of the `PluginTracer` — the guest reports through
//! `logging.log` because the base world's `activate` returns nothing.
//!
//! Host-side unit tests of the conversion function cannot prove this:
//! what is under test is that a real component can *reach* the import,
//! that the linker satisfies it, and that an id crossing the boundary
//! resolves to the same answer the native side would give.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;

use lattice_core::BufferId;
use lattice_mode::{BufferStore, BufferStoreHandle};
use lattice_plugin_host::{PluginHost, PluginTracer, PluginTracerHandle, TraceLevel};

fn guest_wasm() -> Option<&'static str> {
    let path = env!("PROJECT_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// A buffer store that knows exactly one buffer, id 1, with a path.
///
/// Deliberately minimal: the seam needs `name_for` (does this buffer
/// exist?) and `path_for` (where is it?), and nothing else. Faking
/// `handle_for` would mean standing up a Document for a test that never
/// reads one.
struct OneBuffer {
    path: PathBuf,
}

impl BufferStore for OneBuffer {
    fn find_by_name(&self, _name: &str) -> Option<BufferId> {
        None
    }
    fn handle_for(&self, _id: BufferId) -> Option<Arc<dyn lattice_runtime::Document>> {
        None
    }
    fn name_for(&self, id: BufferId) -> Option<String> {
        (id.0 == 1).then(|| "the-buffer".to_string())
    }
    fn path_for(&self, id: BufferId) -> Option<PathBuf> {
        (id.0 == 1).then(|| self.path.clone())
    }
    fn insert_document_buffer(
        &self,
        _id: BufferId,
        _kind: lattice_core::BufferKind,
        _handle: Arc<dyn lattice_runtime::Document>,
        _flags: lattice_core::BufferFlags,
        _name: Option<String>,
    ) {
    }
}

/// One reported line from the guest, split back out of its
/// `tag|some\|none|root|kind|marker` encoding.
#[derive(Debug)]
struct Reported {
    tag: String,
    present: bool,
    root: String,
    kind: String,
    marker: String,
}

fn reports(tracer: &PluginTracerHandle, id: u32) -> Vec<Reported> {
    tracer
        .snapshot_plugin(id)
        .into_iter()
        .filter_map(|r| r.detail.clone())
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('|').collect();
            (parts.len() == 5).then(|| Reported {
                tag: parts[0].to_string(),
                present: parts[1] == "some",
                root: parts[2].to_string(),
                kind: parts[3].to_string(),
                marker: parts[4].to_string(),
            })
        })
        .collect()
}

/// Build a repo, wire a host against it, run the guest, return what it saw.
async fn run_guest(repo: &PathBuf, file: &PathBuf) -> (Vec<Reported>, PathBuf) {
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();

    let tracer: PluginTracerHandle = Arc::new(PluginTracer::new(TraceLevel::Info, 100));
    host.set_tracer(tracer.clone());

    let resolver: lattice_core::ProjectResolverHandle = Arc::new(
        lattice_core::MarkerResolver::with_default_markers(repo.clone()),
    );
    let store = BufferStoreHandle::new(Arc::new(OneBuffer { path: file.clone() }));
    host.set_project_context(resolver, store);

    let component = host
        .compile(&std::fs::read(guest_wasm().unwrap()).unwrap())
        .unwrap();
    let mut plugin = host.instantiate(&component).await.unwrap();
    let id = plugin.id().0;
    plugin.activate().await.unwrap();
    (reports(&tracer, id), repo.clone())
}

fn repo_with_file() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let repo = std::fs::canonicalize(dir.path()).unwrap().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let deep = repo.join("crates/thing/src");
    std::fs::create_dir_all(&deep).unwrap();
    let file = deep.join("lib.rs");
    std::fs::write(&file, "\n").unwrap();
    (dir, repo, file)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_guest_resolves_a_buffers_project_across_the_boundary() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: project fixture guest not built");
        return;
    }
    let (_dir, repo, file) = repo_with_file();
    let (got, _) = run_guest(&repo, &file).await;

    let known = got
        .iter()
        .find(|r| r.tag == "buffer-known")
        .expect("the guest reported buffer-known");
    assert!(known.present, "a known buffer must resolve: {known:?}");
    assert_eq!(
        known.root,
        repo.display().to_string(),
        "the guest should see the repository root, not the file's directory"
    );
    assert_eq!(known.kind, "marker");
    assert_eq!(
        known.marker, ".git",
        "the deciding marker crosses the boundary too"
    );
}

/// A buffer id the host never issued is untrusted input, and must be
/// `none` — distinct from "buffer exists but has no path", which is a
/// real answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_buffer_id_is_none_rather_than_a_wrong_answer() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: project fixture guest not built");
        return;
    }
    let (_dir, repo, file) = repo_with_file();
    let (got, _) = run_guest(&repo, &file).await;

    let unknown = got
        .iter()
        .find(|r| r.tag == "buffer-unknown")
        .expect("the guest reported buffer-unknown");
    assert!(
        !unknown.present,
        "an id the host never issued must not resolve to anything: {unknown:?}"
    );
}

/// `root-for-path` answers for a path with no project: the working
/// directory, flagged `pwd`, with an empty marker. The guest can tell
/// "not in a project" from "no such buffer" — different questions,
/// different shapes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_path_with_no_project_reports_pwd_with_no_marker() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: project fixture guest not built");
        return;
    }
    let (_dir, repo, file) = repo_with_file();
    let (got, _) = run_guest(&repo, &file).await;

    let by_path = got
        .iter()
        .find(|r| r.tag == "path-tmp")
        .expect("the guest reported path-tmp");
    assert!(
        by_path.present,
        "root-for-path answers whenever a resolver is wired: {by_path:?}"
    );
    assert_eq!(
        by_path.kind, "pwd",
        "`/` holds no marker, so this is the pwd fallback: {by_path:?}"
    );
    assert!(
        by_path.marker.is_empty(),
        "a pwd answer carries no marker: {by_path:?}"
    );
}
