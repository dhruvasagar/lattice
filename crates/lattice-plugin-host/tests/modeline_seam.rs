//! OC.3 / ML.6 end-to-end — a plugin owns a modeline element.
//!
//! `modeline.md` §6 is the contract: whoever registers an element owns it end to
//! end, and the host exposes only generic primitives. So this file asserts three
//! things, and the second is the one the slice's design turned on.
//!
//! 1. **An async seam registers a descriptor and pushes content** — and the
//!    content goes on the **bus**, not straight into the service. Writing the
//!    service directly would leave the store correct and the screen stale until
//!    the next keystroke; the repaint comes from the bus forwarder waking the
//!    actor. A test that only checked the store would pass on that broken
//!    version, so the bus subscription here is the load-bearing assertion.
//!
//! 2. **The same component's grammar seam reaches nothing.** The plan for this
//!    slice said `ui` would be "wired on the async linker only, so the modeline
//!    is structurally unreachable from the keystroke path". That mechanism does
//!    not survive the Component Model — a component's imports are fixed for the
//!    whole artefact and org, the plugin this seam exists for, provides `grammar`
//!    too, so an import absent from the grammar linker fails the WHOLE plugin
//!    (org has already been broken exactly this way by one `logging::log` call).
//!    `ui` is therefore on both linkers and the guarantee moved one layer in:
//!    `instantiate_grammar_plugin` clears the store's modeline context. That is a
//!    weaker *mechanism* and a stronger *test* — a linker omission cannot be
//!    checked at all, and this can.
//!
//! 3. **Unload reverses it by namespace**, so no orphan descriptor survives —
//!    which matters because the renderer iterates descriptors, and an orphan
//!    renders the plugin's last segment forever with nobody left to update it.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_core::BufferId;
use lattice_grammar::{CommandInvocation, CommandRegistry, GrammarEnv};
use lattice_mode::modeline::{ElementId, ModelineElementUpdate, ModelineKey, Zone};
use lattice_mode::{CapabilitySet, ModelineService, ModelineServiceHandle};
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};
use lattice_protocol::CancellationToken;
use lattice_protocol::position::Position;
use lattice_runtime::EventBus;

/// The fixture registers `clock`; the host namespaces it with the manifest id.
const ELEMENT: &str = "multiseam.clock";

fn guest_wasm() -> Option<&'static str> {
    let path = env!("MULTISEAM_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

struct Harness {
    _dirs: tempfile::TempDir,
    host: PluginHost,
    component: lattice_plugin_host::Component,
    manifest: PluginManifest,
    modeline: ModelineServiceHandle,
    bus: Arc<EventBus>,
}

/// A host with the modeline wired, as `install` wires it.
fn harness() -> Option<Harness> {
    let path = guest_wasm()?;
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let modeline: ModelineServiceHandle = Arc::new(ModelineService::new());
    let bus = Arc::new(EventBus::new());
    host.set_modeline(Arc::clone(&modeline), Arc::clone(&bus));
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();
    Some(Harness {
        _dirs: dirs,
        host,
        component,
        manifest: PluginManifest::new("multiseam", Vec::new(), CapabilitySet::empty()),
        modeline,
        bus,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_async_seam_registers_a_segment_and_pushes_it_onto_the_bus() {
    let Some(h) = harness() else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    // Subscribe BEFORE the drain: the bus is fire-and-forget, so a late
    // subscriber cannot observe a past push.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ModelineElementUpdate>();
    h.bus.subscribe_typed(tx);

    let config_registry = Arc::new(ConfigRegistry::new());
    h.host
        .spawn_config_plugin(
            &h.component,
            &h.manifest,
            TrustTier::Bundled,
            PluginBudget::event(),
            &config_registry,
        )
        .await
        .expect("config drain instantiates");

    // The DESCRIPTOR went straight into the registry — it is not per-frame
    // state and has no repaint to earn.
    let snap = h.modeline.snapshot();
    let el = snap
        .registry
        .get(&ElementId::new(ELEMENT))
        .expect("the plugin's descriptor registered under its namespaced id");
    assert_eq!(el.zone, Zone::Right);
    assert_eq!(el.priority, 7);

    // The CONTENT went on the bus. This is the assertion that fails on the
    // plausible-but-wrong implementation that writes the service directly.
    let update = rx.try_recv().expect(
        "content must reach the BUS — a direct service write leaves the store \
         correct and the screen stale until the next keystroke",
    );
    assert_eq!(update.id.as_str(), ELEMENT);
    assert!(matches!(update.key, ModelineKey::Global));
    assert_eq!(update.content.spans.len(), 1);
    assert_eq!(update.content.spans[0].text, "◷ 0:14");
}

#[test]
fn a_grammar_action_cannot_reach_the_modeline() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    }
    let h = harness().unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ModelineElementUpdate>();
    h.bus.subscribe_typed(tx);

    // The SAME component, the SAME wired host — only the seam differs.
    let grammar_set = h
        .host
        .instantiate_grammar_plugin(
            &h.component,
            &h.manifest,
            TrustTier::Bundled,
            &h.bus,
            None,
            None,
        )
        .expect("grammar drain instantiates: `ui` IS on the sync linker");
    let mut commands = CommandRegistry::new();
    grammar_set.register_all(&mut commands);
    let id = commands.id_by_name("multiseam-modeline").unwrap();

    let mut document = lattice_core::Document::from_text("x\n");
    let cancel = CancellationToken::never();
    let effect = lattice_grammar::execute_with_env(
        &commands,
        &mut document,
        BufferId(1),
        Position { line: 0, byte: 0 },
        CommandInvocation::of(id),
        &cancel,
        GrammarEnv::default(),
    )
    .expect("the action itself runs — refusal is an answer, not a trap");

    match effect {
        lattice_grammar::effect::Effect::Echo { text, .. } => {
            assert_eq!(
                text, "registered:false",
                "the guest must be TOLD it was refused, so a plugin author sees \
                 the seam is unavailable here rather than silently pushing into a void"
            );
        }
        other => panic!("expected an Echo reporting the refusal, got {other:?}"),
    }
    assert!(
        h.modeline
            .snapshot()
            .registry
            .get(&ElementId::new("multiseam.grammar-clock"))
            .is_none(),
        "no descriptor from the keystroke path"
    );
    assert!(
        rx.try_recv().is_err(),
        "and no content either — `emit-segment` is dropped, not queued"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unload_removes_the_plugins_element_by_namespace() {
    let Some(h) = harness() else {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    };
    let config_registry = Arc::new(ConfigRegistry::new());
    h.host
        .spawn_config_plugin(
            &h.component,
            &h.manifest,
            TrustTier::Bundled,
            PluginBudget::event(),
            &config_registry,
        )
        .await
        .expect("config drain instantiates");
    // Apply the pushed content, as the actor's drain would, so the test can
    // check BOTH halves of the reversal.
    h.modeline.update(
        ModelineKey::Global,
        ElementId::new(ELEMENT),
        lattice_mode::modeline::ElementContent {
            spans: vec![lattice_mode::modeline::Span::new(
                "◷ 0:14",
                lattice_mode::modeline::ModelineRole::new("modeline.mode_item"),
            )],
        },
    );
    assert!(
        h.modeline
            .snapshot()
            .content
            .contains_key(&(ModelineKey::Global, ElementId::new(ELEMENT))),
        "precondition: content is in the store"
    );

    let mut teardown = lattice_plugin_host::PluginTeardown::new(lattice_plugin_host::PluginId(1));
    // The namespace, not a token list — a plugin may register a segment at any
    // point in its life, so there is deliberately nothing to have collected.
    teardown.modeline_namespace = Some("multiseam".to_string());
    let report = unload(&teardown, &h.modeline, &h.bus);
    assert_eq!(report.modeline_elements, 1);

    let snap = h.modeline.snapshot();
    assert!(
        snap.registry.get(&ElementId::new(ELEMENT)).is_none(),
        "the descriptor is gone — the renderer iterates descriptors, so an \
         orphan would render the plugin's last segment forever"
    );
    assert!(
        !snap
            .content
            .contains_key(&(ModelineKey::Global, ElementId::new(ELEMENT))),
        "and the content with it — an orphan entry is invisible but leaks on \
         every :plugin-reload"
    );

    // Idempotent, like every other surface `unload` reverses.
    assert_eq!(unload(&teardown, &h.modeline, &h.bus).modeline_elements, 0);
}

/// Run `unload` against a minimal registry bundle — everything but the modeline
/// is a fresh empty registry, so the report isolates the modeline reversal.
fn unload(
    teardown: &lattice_plugin_host::PluginTeardown,
    modeline: &ModelineServiceHandle,
    bus: &EventBus,
) -> lattice_plugin_host::TeardownReport {
    let mut commands = CommandRegistry::new();
    let mut pickers = lattice_picker::PickerRegistry::new();
    let mut modes = lattice_mode::ModeRegistry::new();
    let keymap = lattice_keymap::KeymapHandle::new();
    let config = ConfigRegistry::new();
    let mut decorations = lattice_mode::GutterDecorationSourceRegistry::new();
    let mut contexts = lattice_mode::ContextSourceRegistry::new();
    let theme_reg = lattice_theme::InMemoryThemeRegistry::new(lattice_theme::default_palette());
    let parsers = lattice_compilation::CompilationParserFactories::new_handle();
    let mut reg = lattice_plugin_host::TeardownRegistries {
        media: &mut Default::default(),
        agenda: &mut Default::default(),
        commands: &mut commands,
        pickers: &mut pickers,
        modes: &mut modes,
        keymap: &keymap,
        config: &config,
        bus,
        decorations: &mut decorations,
        contexts: &mut contexts,
        theme: &theme_reg,
        modeline: Some(modeline),
        parsers: &parsers,
    };
    teardown.unload(&mut reg)
}
