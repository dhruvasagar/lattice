//! CD.6b end-to-end — `host-services.clamp-position`, from the seam that asks.
//!
//! A capture remembers where it was started and writes a link back there when
//! it is filed. By then the caller may be shorter, and `apply-edit` refuses a
//! position that is not there with nothing but a host log line. So the commit,
//! a grammar action on the SYNC linker, clamps first, and reads `none` as "the
//! caller has closed".
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_core::BufferId;
use lattice_grammar::{Args, CommandInvocation, CommandRegistry, GrammarEnv};
use lattice_mode::{BufferStore, CapabilitySet};
use lattice_plugin_host::{PluginHost, PluginManifest, TrustTier};
use lattice_protocol::CancellationToken;
use lattice_protocol::position::Position;

fn guest_wasm() -> Option<&'static str> {
    let path = env!("MULTISEAM_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// One real document, as buffer 7.
struct OneDocument {
    handle: Arc<dyn lattice_runtime::Document>,
}

impl BufferStore for OneDocument {
    fn find_by_name(&self, _name: &str) -> Option<BufferId> {
        None
    }
    fn handle_for(&self, id: BufferId) -> Option<Arc<dyn lattice_runtime::Document>> {
        (id.0 == 7).then(|| self.handle.clone())
    }
    fn name_for(&self, _id: BufferId) -> Option<String> {
        None
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

/// Ask the guest to clamp `(line, byte)` into `buffer`, with buffer 7 holding
/// `text` — or with no store wired at all when `text` is `None`.
fn clamp_via_guest(text: Option<&str>, buffer: u32, line: u32, byte: u32) -> Option<String> {
    let wasm = guest_wasm()?;
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    if let Some(text) = text {
        let registry: lattice_grammar::CommandRegistryHandle =
            Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
        let handle = lattice_runtime::spawn_document(
            BufferId(7),
            lattice_core::Document::from_text(text),
            registry,
        );
        host.set_buffer_store(lattice_mode::BufferStoreHandle::new(Arc::new(
            OneDocument {
                handle: Arc::new(handle),
            },
        )));
    }
    let component = host.compile(&std::fs::read(wasm).unwrap()).unwrap();
    let manifest = PluginManifest::new("multiseam", Vec::new(), CapabilitySet::empty());
    let bus = Arc::new(lattice_runtime::EventBus::new());
    let grammar_set = host
        .instantiate_grammar_plugin(&component, &manifest, TrustTier::Bundled, &bus, None, None)
        .expect("grammar drain instantiates");
    let mut commands = CommandRegistry::new();
    grammar_set.register_all(&mut commands);
    let id = commands.id_by_name("multiseam-clamp-position").unwrap();

    let mut document = lattice_core::Document::from_text("x\n");
    let invocation =
        CommandInvocation::of(id).with_args(Args::String(format!("{buffer} {line} {byte}")));
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

/// A position that exists comes back unchanged.
#[test]
fn a_position_inside_the_buffer_is_kept() {
    let Some(text) = clamp_via_guest(Some("hello\nworld\n"), 7, 1, 3) else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    assert_eq!(text, "1:3");
}

/// The caller got shorter: a line past the end is the last line, and a byte
/// past a line's end stops before its newline.
#[test]
fn a_position_past_the_end_is_pulled_back() {
    let Some(past_line) = clamp_via_guest(Some("hello\nwor"), 7, 40, 2) else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    assert_eq!(past_line, "1:2", "the last line, byte kept");

    let past_byte = clamp_via_guest(Some("hello\nworld\n"), 7, 0, 99).unwrap();
    assert_eq!(
        past_byte, "0:5",
        "the end of `hello`, not after its newline"
    );

    let both = clamp_via_guest(Some("ab\n"), 7, 9, 9).unwrap();
    assert_eq!(both, "1:0", "the empty line after a trailing newline");
}

/// A buffer the store does not hold — the caller was closed.
#[test]
fn an_unknown_buffer_is_none() {
    let Some(text) = clamp_via_guest(Some("hello\n"), 8, 0, 0) else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    assert_eq!(text, "none");
}

/// No store wired: every buffer is absent, rather than a guess.
#[test]
fn an_unwired_host_answers_none() {
    let Some(text) = clamp_via_guest(None, 7, 0, 0) else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    assert_eq!(text, "none");
}
