//! `host-services.write-file` end-to-end, through a real guest.
//!
//! An export writes its `.tex` and then spawns the compiler, which reads it.
//! An `effect.write-to-file` is applied later, on the editor actor, so the
//! compiler can win that race; `write-file` returns only once the bytes are
//! on disk. Driven from the grammar seam's SYNC linker, the strictest seam
//! (`file_seams_on_the_sync_linker.rs`'s argument), with a real document
//! handle as the buffer store so the unsaved-changes refusal is the one
//! production makes.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lattice_core::BufferId;
use lattice_grammar::{Args, CommandInvocation, CommandRegistry, GrammarEnv};
use lattice_mode::{BufferStore, CapabilitySet};
use lattice_plugin_host::{Capability, PluginHost, PluginManifest, TrustTier};
use lattice_protocol::CancellationToken;
use lattice_protocol::position::Position;

fn guest_wasm() -> Option<&'static str> {
    let path = env!("MULTISEAM_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// One document visiting `path`, as buffer 4.
struct Visiting {
    path: PathBuf,
    handle: Arc<dyn lattice_runtime::Document>,
}

impl BufferStore for Visiting {
    fn find_by_name(&self, _name: &str) -> Option<BufferId> {
        None
    }
    fn handle_for(&self, id: BufferId) -> Option<Arc<dyn lattice_runtime::Document>> {
        (id.0 == 4).then(|| self.handle.clone())
    }
    fn name_for(&self, _id: BufferId) -> Option<String> {
        None
    }
    fn path_for(&self, id: BufferId) -> Option<PathBuf> {
        (id.0 == 4).then(|| self.path.clone())
    }
    fn find_by_path(&self, path: &Path) -> Option<BufferId> {
        (path == self.path).then_some(BufferId(4))
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

/// `path` open in buffer 4, edited first when `unsaved`.
fn open_buffer(path: &Path, unsaved: bool) -> lattice_mode::BufferStoreHandle {
    let registry: lattice_grammar::CommandRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
    let document = lattice_core::Document::open(path).unwrap();
    let handle = lattice_runtime::spawn_document(BufferId(4), document, registry);
    if unsaved {
        let edit = lattice_protocol::edit::Edit::replace(
            lattice_protocol::Range::new(Position::ZERO, Position::ZERO),
            "my edit ".to_string(),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime
            .block_on(handle.apply_edit_batch(vec![edit]))
            .unwrap();
        assert!(handle.dirty(), "the edit left the buffer unsaved");
    }
    lattice_mode::BufferStoreHandle::new(Arc::new(Visiting {
        path: path.to_path_buf(),
        handle: Arc::new(handle),
    }))
}

/// Run `multiseam-write-file` with `caps`, and return what the guest echoed.
fn write_via_guest(
    caps: Vec<Capability>,
    buffers: Option<lattice_mode::BufferStoreHandle>,
    path: &Path,
    text: &str,
) -> Option<String> {
    let wasm = guest_wasm()?;
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    if let Some(buffers) = buffers {
        host.set_buffer_store(buffers);
    }
    let component = host.compile(&std::fs::read(wasm).unwrap()).unwrap();
    let manifest = PluginManifest::new("multiseam", caps, CapabilitySet::empty());
    let bus = Arc::new(lattice_runtime::EventBus::new());
    let grammar_set = host
        .instantiate_grammar_plugin(&component, &manifest, TrustTier::Bundled, &bus, None, None)
        .expect("grammar drain instantiates");
    let mut commands = CommandRegistry::new();
    grammar_set.register_all(&mut commands);
    let id = commands.id_by_name("multiseam-write-file").unwrap();

    let mut document = lattice_core::Document::from_text("x\n");
    let invocation = CommandInvocation::of(id)
        .with_args(Args::String(format!("{}\n{text}", path.to_string_lossy())));
    let effect = lattice_grammar::execute_with_env(
        &commands,
        &mut document,
        BufferId(1),
        Position { line: 0, byte: 0 },
        invocation,
        &CancellationToken::never(),
        GrammarEnv::default(),
    )
    .expect("the action dispatches through the sync trampoline");
    match effect {
        lattice_grammar::effect::Effect::Echo { text, .. } => Some(text),
        other => panic!("expected an Echo, got {other:?}"),
    }
}

fn writable(dir: &Path) -> Vec<Capability> {
    vec![Capability::FsWrite(dir.to_path_buf())]
}

/// **The test that matters.** When the call returns, the file is there: the
/// guest's next step (spawning the compiler) reads what was written.
#[test]
fn ok_means_the_bytes_are_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let tex = dir.path().join("notes.tex");
    let Some(text) = write_via_guest(writable(dir.path()), None, &tex, "\\documentclass{article}")
    else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    assert_eq!(text, "wrote");
    assert_eq!(
        std::fs::read_to_string(&tex).unwrap(),
        "\\documentclass{article}"
    );
}

/// The host's boundary message reaches the guest, and nothing is written.
#[test]
fn a_path_outside_the_grant_is_refused() {
    let granted = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let theirs = other.path().join("theirs.tex");
    let Some(text) = write_via_guest(writable(granted.path()), None, &theirs, "x") else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    assert!(text.starts_with("error: fs write denied"), "{text}");
    assert!(!theirs.exists());
}

/// A file the user is editing is not overwritten behind their back, and the
/// refusal names the buffer.
#[test]
fn a_file_open_with_unsaved_changes_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let tex = dir.path().join("notes.tex");
    std::fs::write(&tex, "on disk\n").unwrap();
    let buffers = open_buffer(&tex, true);
    let Some(text) = write_via_guest(writable(dir.path()), Some(buffers), &tex, "export\n") else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    assert!(
        text.contains("buffer 4") && text.contains("unsaved changes"),
        "{text}"
    );
    assert_eq!(std::fs::read_to_string(&tex).unwrap(), "on disk\n");
}

/// Open with nothing unsaved: the write lands (autoread reloads the buffer, as
/// for any change made outside the editor).
#[test]
fn a_file_open_with_no_unsaved_changes_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let tex = dir.path().join("notes.tex");
    std::fs::write(&tex, "on disk\n").unwrap();
    let buffers = open_buffer(&tex, false);
    let Some(text) = write_via_guest(writable(dir.path()), Some(buffers), &tex, "export\n") else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    assert_eq!(text, "wrote");
    assert_eq!(std::fs::read_to_string(&tex).unwrap(), "export\n");
}
