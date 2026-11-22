//! OB.3c end-to-end — `host-services.buffer-text`, through the guest that asks.
//!
//! A job a mode started on its buffer finishes in `on-event`, which has no
//! `document`; this is how the plugin reads the buffer back to put the result
//! in the right place. It is gated on the plugin's OWN modes being active
//! there, so every arm below is a reason a read must be refused, plus the one
//! that must not be.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::HashMap;
use std::sync::Arc;

use lattice_core::BufferId;
use lattice_grammar::{Args, CommandInvocation, CommandRegistry, GrammarEnv};
use lattice_keymap::KeymapHandle;
use lattice_mode::{ActiveModesQuery, BufferStore, CapabilitySet, ModeId, ModeRegistry};
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};
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

/// The editor's answer to "which modes are on this buffer", fixed per test.
#[derive(Debug)]
struct Modes(HashMap<u32, Vec<&'static str>>);

impl ActiveModesQuery for Modes {
    fn active_on(&self, buffer: BufferId) -> Vec<ModeId> {
        self.0
            .get(&buffer.0)
            .map(|ids| ids.iter().map(|m| ModeId::new(m)).collect())
            .unwrap_or_default()
    }
}

/// Load the fixture's modes (so the host knows they are `multiseam`'s) and its
/// grammar seam, wire `modes` as what is active where, and ask for `buffer`.
fn read_via_guest(modes: Option<Vec<(u32, Vec<&'static str>)>>, buffer: u32) -> Option<String> {
    let wasm = guest_wasm()?;
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let registry: lattice_grammar::CommandRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
    let handle = lattice_runtime::spawn_document(
        BufferId(7),
        lattice_core::Document::from_text("#+begin_src sh\necho hi\n#+end_src\n"),
        registry,
    );
    host.set_buffer_store(lattice_mode::BufferStoreHandle::new(Arc::new(
        OneDocument {
            handle: Arc::new(handle),
        },
    )));
    if let Some(modes) = modes {
        host.set_active_modes_query(Arc::new(Modes(modes.into_iter().collect())));
    }

    let component = host.compile(&std::fs::read(wasm).unwrap()).unwrap();
    let manifest = PluginManifest::new("multiseam", Vec::new(), CapabilitySet::empty());
    let bus = Arc::new(lattice_runtime::EventBus::new());
    let grammar_set = host
        .instantiate_grammar_plugin(&component, &manifest, TrustTier::Bundled, &bus, None, None)
        .expect("grammar drain instantiates");
    let mut commands = CommandRegistry::new();
    grammar_set.register_all(&mut commands);

    // The modes seam is async; drive it on its own runtime so the sync
    // grammar call below runs outside one, as it does in the editor.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut mode_registry = ModeRegistry::default();
    let keymap = KeymapHandle::new();
    rt.block_on(host.spawn_mode_plugin(
        &component,
        &manifest,
        TrustTier::Bundled,
        PluginBudget::event(),
        lattice_plugin_host::mode_host::ModeTargets {
            modes: &mut mode_registry,
            commands: &commands,
            keymap: &keymap,
            config: None,
        },
    ))
    .expect("modes drain instantiates");

    let id = commands.id_by_name("multiseam-buffer-text").unwrap();
    let mut document = lattice_core::Document::from_text("x\n");
    let invocation = CommandInvocation::of(id).with_args(Args::String(buffer.to_string()));
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

/// The plugin's own mode is on the buffer: the whole text, as the editor has it.
#[test]
fn a_buffer_the_plugins_mode_runs_in_is_readable() {
    let Some(text) = read_via_guest(Some(vec![(7, vec!["text-mode", "multiseam-mode"])]), 7) else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    assert_eq!(text, "#+begin_src sh\necho hi\n#+end_src\n");
}

/// Modes are on the buffer, but none of them is this plugin's.
#[test]
fn a_buffer_with_only_other_modes_is_not() {
    let Some(text) = read_via_guest(Some(vec![(7, vec!["text-mode", "other-plugin-mode"])]), 7)
    else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    assert_eq!(text, "none");
}

/// The plugin's mode is active, but the store does not hold the buffer — it
/// closed between the mode set's publish and the read.
#[test]
fn a_closed_buffer_is_not() {
    let Some(text) = read_via_guest(Some(vec![(8, vec!["multiseam-mode"])]), 8) else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    assert_eq!(text, "none");
}

/// No active-modes query wired: no way to tell whose buffer it is, so no read.
#[test]
fn an_unwired_host_refuses_every_read() {
    let Some(text) = read_via_guest(None, 7) else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    assert_eq!(text, "none");
}
