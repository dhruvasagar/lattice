//! HB.2b — `excerpt-source`, asked by a real guest on the sync grammar seam.
//!
//! The seam had tests at every layer and answered `none` in practice. The
//! reason none of them could catch that is the shape worth naming: the
//! resolver's own tests (`lattice-multibuffer`'s `excerpt_source_tests`) call
//! the resolver directly, and the host's projection is a three-line forward.
//! Between them sits the thing that actually decides the answer — whether the
//! **store the guest runs in** carries a resolver at all.
//!
//! `new_store` stamps it onto every store, and `instantiate_grammar_plugin`
//! strips `ui` from that same store on purpose. So "every store gets it" is a
//! claim about a line that another line deliberately contradicts for a
//! neighbouring field, on the one seam a chord reaches. That is worth an
//! assertion rather than a reading.
//!
//! The guest half is the fixture's `multiseam-excerpt-source`: it calls the
//! seam with the ids from **its own context**, not ones the test supplied, and
//! echoes the answer — so `none` is as loud as a hit.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_core::{BufferId, ExcerptSource, ExcerptSourceResolver};
use lattice_grammar::{CommandInvocation, CommandRegistry, GrammarEnv};
use lattice_mode::CapabilitySet;
use lattice_plugin_host::{PluginHost, PluginManifest, TrustTier};
use lattice_protocol::CancellationToken;
use lattice_protocol::position::Position;

/// The view id the fixture is told it is standing in, and the source behind
/// its row. Concrete values so a wrong-id answer reads differently from a
/// right-id one rather than both collapsing to `none`.
const VIEW: BufferId = BufferId(41);
const SOURCE: BufferId = BufferId(42);

/// A resolver shaped like the multibuffer's: it answers for ONE view, and
/// `none` for everything else — which is what makes a wrong buffer id visible.
///
/// A stub rather than the real `MultibufferExcerptSource` because
/// `lattice-plugin-host` cannot depend on `lattice-multibuffer` (the layering
/// that put the trait in `lattice-core` in the first place). The real
/// resolver's behaviour against a real scan view is pinned in that crate; what
/// this file adds is that a guest reaches whatever is wired.
#[derive(Debug)]
struct OneViewResolver;

impl ExcerptSourceResolver for OneViewResolver {
    fn excerpt_source(&self, buffer: BufferId, line: u32) -> Option<ExcerptSource> {
        (buffer == VIEW).then(|| ExcerptSource {
            source: SOURCE,
            path: std::path::PathBuf::from("/org/notes.org"),
            // Composed row N is source row N + 10 here, so a seam that
            // forwarded the composed line unchanged would be visible.
            line: line + 10,
        })
    }

    fn source_line(&self, source: BufferId, line: u32) -> Option<String> {
        (source == SOURCE).then(|| format!("source line {line}"))
    }
}

fn guest_wasm() -> Option<&'static str> {
    let path = env!("MULTISEAM_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// Dispatch the fixture's probe action in `buffer` at `line` and return what
/// it echoed.
fn ask(host: &PluginHost, component: &wasmtime::component::Component, buffer: BufferId) -> String {
    let manifest = PluginManifest::new("multiseam", Vec::new(), CapabilitySet::empty());
    let bus = Arc::new(lattice_runtime::EventBus::new());
    let grammar_set = host
        .instantiate_grammar_plugin(component, &manifest, TrustTier::Bundled, &bus, None, None)
        .expect("grammar drain instantiates");
    let mut commands = CommandRegistry::new();
    grammar_set.register_all(&mut commands);
    let id = commands.id_by_name("multiseam-excerpt-source").unwrap();

    let mut document = lattice_core::Document::from_text("* TODO write it\n* TODO and it\n");
    let effect = lattice_grammar::execute_with_env(
        &commands,
        &mut document,
        buffer,
        Position { line: 1, byte: 0 },
        CommandInvocation::of(id),
        &CancellationToken::never(),
        GrammarEnv::default(),
    )
    .expect("the probe dispatches through the sync trampoline");

    match effect {
        lattice_grammar::effect::Effect::Echo { text, .. } => text,
        other => panic!("expected an Echo carrying the seam's answer, got {other:?}"),
    }
}

fn host_with_resolver(dirs: &tempfile::TempDir) -> PluginHost {
    let host = bare_host(dirs);
    host.set_excerpt_source_resolver(Arc::new(OneViewResolver));
    host
}

fn bare_host(dirs: &tempfile::TempDir) -> PluginHost {
    PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).expect("host builds")
}

/// The case the seam exists for: a guest on a composed row learns the file.
#[test]
fn a_grammar_guest_learns_the_source_behind_its_row() {
    let Some(path) = guest_wasm() else {
        eprintln!("skipping: multiseam guest wasm not built");
        return;
    };
    let dirs = tempfile::tempdir().unwrap();
    let host = host_with_resolver(&dirs);
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();

    assert_eq!(
        ask(&host, &component, VIEW),
        format!(
            "excerpt-source({},1)=/org/notes.org@11 in {}",
            VIEW.0, SOURCE.0
        ),
        "the guest must reach the wired resolver; `none` here means the store \
         it runs in has no resolver, and every layer below would still pass"
    );
}

/// And the ordinary `none`: a buffer that is not a view. Asserted so the test
/// above is known to be measuring the resolver rather than a stub that answers
/// the same thing for everything.
#[test]
fn a_guest_in_an_ordinary_buffer_is_told_none() {
    let Some(path) = guest_wasm() else {
        eprintln!("skipping: multiseam guest wasm not built");
        return;
    };
    let dirs = tempfile::tempdir().unwrap();
    let host = host_with_resolver(&dirs);
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();

    assert_eq!(
        ask(&host, &component, BufferId(7)),
        "excerpt-source(7,1)=none",
    );
}

/// A host with nothing wired answers `none` rather than failing — the
/// degradation the seam promises, and the state every test that never calls
/// `set_excerpt_source_resolver` is silently in.
#[test]
fn an_unwired_host_answers_none_rather_than_trapping() {
    let Some(path) = guest_wasm() else {
        eprintln!("skipping: multiseam guest wasm not built");
        return;
    };
    let dirs = tempfile::tempdir().unwrap();
    let host = bare_host(&dirs);
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();

    assert_eq!(
        ask(&host, &component, VIEW),
        format!("excerpt-source({},1)=none", VIEW.0),
    );
}
