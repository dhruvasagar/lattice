//! CD.3 / CD.3b end-to-end — `host-services.delete-file` and
//! `can-write-file`, from the seam that cannot touch files on its own.
//!
//! `new_uuid_seam.rs`'s argument, for a delete: discarding a saved capture
//! draft is a **grammar action**, which runs on the grammar seam's
//! *synchronous* linker, where a guest's `std::fs::remove_file` goes through
//! `wasmtime-wasi`'s sync shim and takes the plugin down. The assertion that
//! matters is that the call **returns at all from the sync trampoline** — and
//! then that the grant decides what it may touch.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lattice_core::BufferId;
use lattice_grammar::{Args, CommandInvocation, CommandRegistry, GrammarEnv};
use lattice_mode::CapabilitySet;
use lattice_plugin_host::{Capability, PluginHost, PluginManifest, TrustTier};
use lattice_protocol::CancellationToken;
use lattice_protocol::position::Position;

fn guest_wasm() -> Option<&'static str> {
    let path = env!("MULTISEAM_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// Run `<action> <path>` through the real sync trampoline with `caps`
/// granted, and return what the guest echoed.
fn run_via_guest(action: &str, caps: Vec<Capability>, path: &Path) -> Option<String> {
    let wasm = guest_wasm()?;
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let component = host.compile(&std::fs::read(wasm).unwrap()).unwrap();
    let manifest = PluginManifest::new("multiseam", caps, CapabilitySet::empty());
    let bus = Arc::new(lattice_runtime::EventBus::new());
    let grammar_set = host
        .instantiate_grammar_plugin(&component, &manifest, TrustTier::Bundled, &bus, None, None)
        .expect("grammar drain instantiates");
    let mut commands = CommandRegistry::new();
    grammar_set.register_all(&mut commands);
    let id = commands.id_by_name(action).unwrap();

    let mut document = lattice_core::Document::from_text("x\n");
    let cancel = CancellationToken::never();
    let invocation =
        CommandInvocation::of(id).with_args(Args::String(path.to_string_lossy().into_owned()));
    let effect = lattice_grammar::execute_with_env(
        &commands,
        &mut document,
        BufferId(1),
        Position { line: 0, byte: 0 },
        invocation,
        &cancel,
        GrammarEnv::default(),
    )
    .expect("the action dispatches through the sync trampoline");
    match effect {
        lattice_grammar::effect::Effect::Echo { text, .. } => Some(text),
        other => panic!("expected an Echo, got {other:?}"),
    }
}

fn delete_via_guest(caps: Vec<Capability>, path: &Path) -> Option<String> {
    run_via_guest("multiseam-delete-file", caps, path)
}

fn check_via_guest(caps: Vec<Capability>, path: &Path) -> Option<String> {
    run_via_guest("multiseam-can-write-file", caps, path)
}

fn writable(dir: &Path) -> Vec<Capability> {
    vec![Capability::FsWrite(dir.to_path_buf())]
}

/// **The test that matters.** The delete happens on the SYNC grammar linker.
#[test]
fn the_grammar_seam_can_delete_a_file_it_may_write() {
    let dir = tempfile::tempdir().unwrap();
    let draft = dir.path().join("a3f9c1.org");
    std::fs::write(&draft, "* TODO draft\n").unwrap();

    let Some(text) = delete_via_guest(writable(dir.path()), &draft) else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    assert_eq!(text, "deleted");
    assert!(!draft.exists());
}

/// Discarding a capture that was never saved: nothing there, and that is the
/// outcome asked for.
#[test]
fn deleting_a_file_that_is_not_there_is_ok() {
    let dir = tempfile::tempdir().unwrap();
    let never: PathBuf = dir.path().join("never-saved.org");
    let Some(text) = delete_via_guest(writable(dir.path()), &never) else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    assert_eq!(text, "deleted");
}

/// Outside the grant the host refuses with its own message, and the file
/// survives.
#[test]
fn a_path_outside_the_grant_is_refused() {
    let granted = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let theirs = other.path().join("theirs.org");
    std::fs::write(&theirs, "keep\n").unwrap();

    let Some(text) = delete_via_guest(writable(granted.path()), &theirs) else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    assert!(
        text.starts_with("error: fs delete denied"),
        "the host's boundary message reaches the guest: {text}"
    );
    assert!(theirs.exists());
}

/// A read grant is not a delete grant.
#[test]
fn a_read_only_grant_cannot_delete() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("read-only.org");
    std::fs::write(&file, "keep\n").unwrap();

    let Some(text) = delete_via_guest(vec![Capability::FsRead(dir.path().to_path_buf())], &file)
    else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    assert!(text.starts_with("error: fs delete denied"), "{text}");
    assert!(file.exists());
}

/// CD.3b: the query answers from the grammar seam, where capture asks it at
/// open — and says `writable` for a file that does not exist yet.
#[test]
fn the_grammar_seam_can_ask_whether_a_write_would_land() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("inbox.org");
    let Some(text) = check_via_guest(writable(dir.path()), &target) else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    assert_eq!(text, "writable");
    assert!(!target.exists(), "asking creates nothing");
}

#[test]
fn a_write_the_boundary_would_deny_is_reported_before_it_is_tried() {
    let granted = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let Some(text) = check_via_guest(writable(granted.path()), &other.path().join("x.org")) else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    assert!(text.starts_with("error: write denied"), "{text}");
}
