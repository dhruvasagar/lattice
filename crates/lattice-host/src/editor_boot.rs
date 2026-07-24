//! Phase 5.7.B.1: `Editor::boot` -- the renderer-agnostic
//! editor boot routine.
//!
//! Moved out of `lattice-ui-tui::app::boot::App::new` so the
//! TUI peer and the future GPUI peer can produce a fully-
//! constructed [`Editor`] from a [`Document`] through the same
//! entry point. Each peer's `App::new` wrapper then:
//!
//! 1. calls [`Editor::boot`] to build the renderer-neutral state,
//! 2. wraps the result alongside its renderer-specific caches
//!    (`theme`, `pane_render_registry`, ...),
//! 3. runs the post-boot derived-cache + activation helpers
//!    (theme mirror, option cache, major mode activation,
//!    `Event::DocumentOpened` publish, eager subsystem buffer
//!    seeding).
//!
//! Three module-private helpers (`build_lsp_subsystem`,
//! `built_in_picker_registry`, `register_mode_toggle_commands`)
//! came along with the body and live here too -- they're called
//! only from `Editor::boot`.

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use lattice_completion::CompletionRegistry;
use lattice_config::ConfigRegistry;
use lattice_core::{BufferKind, Document};
use lattice_grammar::CommandRegistry;
use lattice_grammar::builtins::populate as grammar_builtins_populate;
use lattice_lsp::{
    ApplyEditBus, ConfigurationBus, DiagnosticsLayer, InboundApplyEdit, InboundShowMessageRequest,
    LspLogger, LspSupervisor, LspSupervisorHandle, ShowDocumentBus, ShowMessageRequestBus,
};
use lattice_mode::{ModeRegistry, ServiceRegistry, SubsystemBoot};
use lattice_picker::PickerRegistry;
use lattice_protocol::position::Position;
use lattice_runtime::{EventBus, MessagesRing, spawn_document};
use lattice_snippet::SnippetRegistry;
use lattice_syntax::{Lang, LangRegistry, Syntax, SyntaxHandle};

use crate::boot_context::BootContext;
use crate::buffer_registry::{BufferData, BufferEntry, BufferRegistry, DocumentEntry};
use crate::buffers::{BufferFlags, BufferId};
use crate::editor::Editor;
use crate::pane::{PaneId, PaneState, PaneTree};

/// Build a fresh LSP subsystem. Returns the supervisor handle
/// + cloned handles to the diagnostics layer + logger so the
/// renderer's per-frame reads can skip the supervisor lock,
/// plus the four server-initiated channel rx ends.
///
/// `event_bus` is wired in pre-spawn so the supervisor task is
/// born already knowing about it; subsequent actor spawns get
/// their per-actor edit fan-in (via `lattice_lsp::fan_in`) for
/// free. The explicit `runtime_handle` removes the silent-fail
/// footgun of `Handle::try_current()` -- `Editor::boot` runs
/// before any main loop has entered a tokio context.
fn build_lsp_subsystem(
    event_bus: Arc<EventBus>,
    runtime_handle: &tokio::runtime::Handle,
    // BC.8b/BC.8c: the configuration + show-document buses are now the generic
    // `InboundBus` (wired via `boot.inbound` in Phase A so their `send` wakes
    // the editor + their drain runs the mode-owned handler). Passed in
    // pre-spawn so the supervisor fans them out to its (lazily-spawned) actors.
    // `logger` is likewise created in Phase A (so the show-document handler can
    // capture a clone — LspLogger is Arc-backed, clones share rings) and passed
    // in. BC.8d/e: apply-edit + show-message-request are the host-drained
    // `InboundBus` (wake-baked sender + host-owned receiver via
    // `boot.inbound_raw`) — all four server-initiated buses now ride the generic
    // primitive; no bespoke bus remains.
    logger: LspLogger,
    configuration_bus: ConfigurationBus,
    show_document_bus: ShowDocumentBus,
    apply_edit_bus: ApplyEditBus,
    show_message_request_bus: ShowMessageRequestBus,
) -> (LspSupervisorHandle, DiagnosticsLayer) {
    let mut sup = LspSupervisor::new(logger.clone());
    sup.set_configs(lattice_lsp::builtin_servers());
    let diagnostics = sup.diagnostics().clone();
    // BC.8d: the apply-edit bus is the generic (host-drained) `InboundBus`
    // passed in (no bespoke `ApplyEditBus::new()`); the host owns the matching
    // receiver, seated on the Editor + drained in `run_tick_pending`.
    sup.set_apply_edit_bus(apply_edit_bus);
    // BC.8b: the configuration bus is the generic `InboundBus` passed in (no
    // bespoke `ConfigurationBus::new()` / host-drained `rx`).
    sup.set_configuration_bus(configuration_bus);
    // BC.8c: the show-document bus is the generic `InboundBus` passed in (no
    // bespoke `ShowDocumentBus::new()` / host-drained `rx`).
    sup.set_show_document_bus(show_document_bus);
    // BC.8e: the show-message-request bus is the generic (host-drained)
    // `InboundBus` passed in (no bespoke `ShowMessageRequestBus::new()`); the
    // host owns the matching receiver, seated on the Editor + drained in
    // `run_tick_pending` (the picker routing is irreducibly `&mut Editor`).
    sup.set_show_message_request_bus(show_message_request_bus);
    sup.set_event_bus(event_bus.clone());
    let handle = sup.spawn(runtime_handle);
    // M-async.5: LSP attach driver is gone; modes drive
    // `open_buffer` directly via the supervisor handle pulled
    // from `ctx.service::<...>()`. `event_bus` stays bound to
    // the supervisor for the per-actor edit fan-in; the keep
    // is intentional -- no other consumer in this function.
    let _ = &event_bus;
    (handle, diagnostics)
}

/// Boot-time registration of the first-party picker sources
/// the `:picker <source>` ex-command dispatches to. Each
/// source is registered with its `PickerSourceGenerator` impl
/// so dispatch resolves through `gen.init()` / `gen.accept()`.
/// Feature-crate sources (snippet today; LSP / DAP / ... later)
/// register through their own entry points called from inside
/// this helper -- the host wires the dependency direction so
/// `lattice-picker` itself never has to know about feature
/// crates.
fn built_in_picker_registry(
    command_registry: lattice_grammar::CommandRegistryHandle,
    config: Arc<ConfigRegistry>,
    keybinding_reverse: Arc<dyn lattice_completion::KeymapReverseLookup>,
    grep_highlighter: Option<Arc<dyn lattice_picker::picker_sources::GrepPreviewHighlighter>>,
    snippet_registry: Arc<ArcSwap<SnippetRegistry>>,
    theme_registry: lattice_theme::ThemeRegistryHandle,
) -> PickerRegistry {
    let mut reg = PickerRegistry::new();
    for generator in lattice_picker::picker_sources::first_party_generators(
        command_registry,
        config,
        keybinding_reverse,
        grep_highlighter,
    ) {
        reg.register_generator(generator);
    }
    lattice_snippet::picker_sources::register(&mut reg, snippet_registry);
    // T.12a: the live-preview theme picker (`:colorscheme` no-arg).
    // Holds a clone of the host's `ThemeRegistryHandle` so it can
    // enumerate registered theme names + drive live preview.
    reg.register_generator(Arc::new(crate::host_generators::ThemePickerSource::new(
        theme_registry,
    )));
    reg
}

/// M.5.1: register a `:<mode-name>` toggle ex-command for every
/// mode in `mode_registry`. The command id is the mode name (no
/// `ex:` prefix; the ex-command resolver tries direct registry-
/// name lookup before alias expansion, so `:lsp-mode` resolves
/// directly).
///
/// Toggle apply-fn returns
/// [`lattice_grammar::Effect::ToggleMode { mode_name }`]; each
/// renderer's effect dispatcher routes that to the App-level
/// `toggle_mode_by_name`.
fn register_mode_toggle_commands(cmd_registry: &mut CommandRegistry, mode_registry: &ModeRegistry) {
    let mut names: Vec<String> = mode_registry
        .iter_meta()
        .map(|(id, _kind)| id.to_string())
        .collect();
    // Sort for deterministic registration order (HashMap iteration
    // is hash-randomized; deterministic boot keeps `:describe-*`
    // and tests stable).
    names.sort();
    for name in names {
        // The toggle spec is shared with the plugin modes-seam drain (loader
        // `drain_mode`) so native + plugin modes get an IDENTICAL `:<mode>`
        // toggle command. Native modes register under Builtin provenance here;
        // plugin modes register under `Plugin(id)` so unload reverses them.
        cmd_registry.register_ex_command(
            &name,
            lattice_grammar::registry::MODE_TOGGLE_COMMAND_DOC,
            lattice_grammar::registry::mode_toggle_ex_command_spec(&name),
        );
    }
}

impl Editor {
    /// Build a fully-wired renderer-neutral [`Editor`] from an
    /// initial [`Document`]. Phase 5.7.B.1 extraction of the
    /// boot body from `lattice-ui-tui::app::boot::App::new`.
    ///
    /// What happens here: event bus + LSP subsystem (with all
    /// four server-initiated channels) + every per-feature LSP
    /// rx subscription; grammar registry populated with builtins
    /// + ex-commands + auto-generated `:<mode-name>` toggles;
    /// mode registry with foundation / syntax / lsp-log /
    /// lsp-completion / oil / file-tree / snippet / buffer-kind
    /// modes; completion registry with builtins + the seven
    /// host-side completion generators; config registry +
    /// linkme init + LSP-logger seeding; tree-sitter
    /// `LangRegistry` + initial seeded `SyntaxHandle`;
    /// `spawn_document`; buffer registry seeded with typed
    /// buffer-locals.
    ///
    /// What does NOT happen here (each renderer's `App::new`
    /// runs them afterwards): renderer-specific theme cache
    /// rebuild; `rebuild_option_cache` /
    /// `sync_host_theme_from_config` (host fns but their
    /// renderer-signal fan-out is per-renderer);
    /// `activate_major_for_buffer_kind` (returns signals);
    /// `publish_document_opened_for_active` /
    /// `ensure_named_synthetic_document` /
    /// `ensure_messages_buffer` (App-side helpers).
    pub fn boot(document: Document) -> Self {
        // §5.10 event bus. Built before `build_lsp_subsystem`
        // because the supervisor wires its per-actor edit fan-
        // in (lattice_lsp::fan_in) at spawn time using this
        // bus, and the post-spawn handle does not expose
        // `set_event_bus`.
        let event_bus = Arc::new(EventBus::new());
        // Canonical LSP runtime handle: a process-wide singleton
        // lazily initialised on first call so every later caller
        // (per-feature `spawn_on_lsp_runtime` for hover /
        // definition / etc., the attach driver, every test that
        // exercises the LSP write path) reuses the same instance.
        let runtime_handle = lattice_runtime::runtime::lsp_runtime().handle().clone();

        // ── Phase A (boot-composition BC.3a): generic primitives ──────────
        // The host's generic-primitive surface, built up front and bundled
        // into a `BootContext`. Each subsystem's command / mode / service
        // registration runs through `boot` (BC.3a); BC.3b+ collapse each
        // subsystem's scattered wiring into one `install(boot)` call. These
        // bindings used to be created mid-boot (`async_landed` ~706,
        // `tick_callbacks` ~1153, `render_state` ~821, `buffers` ~767,
        // `buffer_store` / `diag_query` inside the `services:` block);
        // hoisting them here is a mechanical `let`-reorder that PRESERVES Arc
        // identity (the workers + Editor fields + service registrations below
        // still clone these exact Arcs — never re-`Arc::new`).
        //
        // Slice B.1: `async_landed` seats the reparse-worker `on_publish`
        // wake + the `Editor::async_landed` field; the actor loop awaits it.
        let async_landed: Arc<tokio::sync::Notify> = Arc::default();
        // IDE-protocol I1.1: the per-tick drain-closure registry (one Arc for
        // the editor's lifetime; modes add drains from `on_activate`).
        let tick_callbacks: lattice_mode::TickCallbackRegistryHandle =
            Arc::new(lattice_mode::TickCallbackRegistry::new());
        // Phase 5.8.AF.5 / Slice 3a: the published render-state cell. The
        // overlay / cells / virtual-rows workers (spawned below) + every
        // `publish_render_state` share this exact Arc identity.
        let render_state_arc: Arc<ArcSwap<crate::render_state::RenderState>> = Arc::new(
            ArcSwap::from_pointee(crate::render_state::RenderState::default()),
        );
        // The buffer registry; created empty here and seeded with the initial
        // document at the spawn site below (`BufferRegistry` is `Clone` via an
        // inner Arc, so the handle handed to `boot` observes that seeding).
        let buffers = BufferRegistry::new();
        // BC.3a read-tool handles derived from the Phase-A cells: the generic
        // buffer-store (over `buffers`) + the diagnostics query (over the
        // render-state cell). Registered as services below + handed to the
        // claude-code read tools; `boot` holds clones for the BC.3b migration.
        let buffer_store_handle = {
            let store: Arc<dyn lattice_mode::BufferStore> = Arc::new(buffers.clone());
            lattice_mode::BufferStoreHandle::new(store)
        };
        let diag_query: lattice_lsp::modes::DiagnosticsQueryHandle = Arc::new(
            crate::diagnostics_query::HostDiagnosticsQuery::new(render_state_arc.clone()),
        );
        // BC.3a (decision 2-b): `boot` owns the three registries during the
        // build phase. Every mode / command / service registration below runs
        // through `boot.modes_mut()` / `boot.commands_mut()` /
        // `boot.register_service()`; the `freeze_*` calls hand back the shared
        // `Arc`s the `Editor` literal seats. The registries are passed empty.
        let mut boot = BootContext::new(
            event_bus.clone(),
            tick_callbacks.clone(),
            async_landed.clone(),
            runtime_handle.clone(),
            buffer_store_handle.clone(),
            CommandRegistry::new(),
            ModeRegistry::new(),
            ServiceRegistry::new(),
        );
        // BC.3b: register the generic diagnostics-query service in Phase A so a
        // subsystem `install(boot)` can reach it via `boot.service::<DiagnosticsQueryHandle>()`
        // (the `SubsystemBoot` surface can't name the lattice-lsp type). Read by
        // claude-code's read tools; registered once here, not in the late block.
        boot.register_service::<lattice_lsp::modes::DiagnosticsQueryHandle>(diag_query.clone());

        // Typed-options registry (DESIGN.md §5.12). Single source of truth
        // for every option's *current value*: each `Option<T>` owns a
        // wait-free `ArcSwap<T>` cell that `:set` parses into, hot-path
        // readers load from, and the (future) customize buffer view edits
        // through.
        //
        // DB.5 hoist (2026-07-03): built + registered here in Phase A —
        // moved up from ~line 700 (right after `event_bus`, its only
        // dependency), the same class of hoist BC.3a already did for
        // `async_landed` / `tick_callbacks` / `buffers`. `ServiceRegistry`
        // only reflects what's registered by the time a reader calls
        // `boot.service::<T>()`; a subsystem's `install(&mut boot)` runs in
        // the Phase-B list further down, so `lattice_dashboard::install`
        // (DB.5's startup-trigger subscription, which reads
        // `dashboard.enabled` via `boot.service::<Arc<ConfigRegistry>>()`)
        // could never observe a registration added later in the same boot
        // call under the old ordering. Mechanical reorder; every later
        // reader still clones this exact `Arc` — never re-`Arc::new`.
        let config = Arc::new(ConfigRegistry::new());
        let bus_for_publisher = event_bus.clone();
        config.set_event_publisher(Arc::new(move |event| {
            bus_for_publisher.publish(event);
        }));
        // M.2.0c: every option (core + renderer-specific) self-
        // registers via the proc-macro-emitted `register_fn`
        // thunks aggregated in `OPTION_DECLS`. One
        // `init_from_linkme()` call boots them all; idempotent
        // if called again.
        config.init_from_linkme();
        // MH.A3 (2026-06-19): expose the ConfigRegistry so extension-crate
        // code (`create_multibuffer_view`) can read global option defaults
        // — e.g. `ui.nerd_fonts` for the rich excerpt-header icon palette —
        // without depending on `lattice-host`'s typed option decls. Read by
        // name (`get_bool_by_name`). Same `Arc<X>` register/lookup pair per
        // the ServiceRegistry Arc/TypeId rule.
        boot.register_service::<Arc<ConfigRegistry>>(config.clone());

        // BC.8b: the merged `lsp.*` config tree is shared (`Arc<ArcSwap>`) so the
        // mode-owned configuration inbound handler reads the *current* tree (the
        // host re-`store`s it on reload). Built empty here; populated by the
        // config loader post-construction (`store`), exactly as the old
        // `toml::Table` field was assigned. The SAME `Arc` is seated in the
        // `Editor.lsp_config_tree` field below (NOT `Editor::default()`'s fresh
        // one), so the handler and the editor observe one tree.
        let lsp_config_tree = std::sync::Arc::new(ArcSwap::from_pointee(toml::Table::new()));
        // BC.8b: wire the `workspace/configuration` bus as the generic inbound
        // primitive — `send` wakes the editor; the per-tick drain runs the
        // mode-owned `make_handler` (a pure read → reply, no Effect). Done in
        // Phase A (pre-spawn) because the supervisor fans the bus out to its
        // actors; the drain token rides `into_registrations` onto the Editor.
        let lsp_configuration_bus = boot.inbound::<lattice_lsp::InboundConfigurationRequest, _>(
            lattice_lsp::configuration::make_handler(lsp_config_tree.clone()),
        );
        // BC.8c: create the LSP logger in Phase A so the mode-owned
        // show-document handler can capture a clone (LspLogger is Arc-backed —
        // every clone shares the same log rings). Wire the show-document bus as
        // the generic inbound primitive: `send` wakes the editor; the per-tick
        // drain runs `make_handler`, which maps each request to a HOST-APPLIED
        // open effect (OpenExternalUri / OpenBufferAtColumn) + an optimistic
        // reply. Host-applied because this bus drains off-keystroke, where
        // peer-applied open effects are not forwarded.
        let lsp_logger = lattice_lsp::LspLogger::with_defaults();
        let lsp_show_document_bus = boot.inbound::<lattice_lsp::InboundShowDocument, _>(
            lattice_lsp::show_document::make_handler(lsp_logger.clone()),
        );
        // BC.8d: the apply-edit bus is the host-drained generic `InboundBus`
        // (wake-baked sender + raw receiver). The host seats the receiver on the
        // Editor (`pending_apply_edit_rx`) and drains it in `run_tick_pending`
        // (the apply is irreducibly `&mut Editor` + `lsp_types`, so it can't be
        // a mode-owned handler); `send` wakes the editor so the edit lands
        // off-keystroke instead of on the next keypress.
        let (lsp_apply_edit_bus, lsp_apply_edit_rx) = boot.inbound_raw::<InboundApplyEdit>();
        // BC.8e: the show-message-request bus is likewise the host-drained
        // generic `InboundBus`. The request is a deferred user choice routed
        // through the host picker primitive, so the host seats the receiver on
        // the Editor (`pending_show_message_request_rx`) and drains it in
        // `run_tick_pending`; `send` wakes the editor so the picker is raised
        // off-keystroke.
        let (lsp_show_message_request_bus, lsp_show_message_request_rx) =
            boot.inbound_raw::<InboundShowMessageRequest>();
        // I4 (Claude Code IDE peer, `openDiff`): the programmatic-diff bus is the
        // host-drained generic `InboundBus`, same shape as BC.8d apply-edit. The
        // host seats the receiver on the Editor (`pending_programmatic_diff_rx`)
        // and drains it in `run_tick_pending` (`open_programmatic_diff` is
        // irreducibly `&mut Editor` + lattice-diff types, so it can't be a
        // mode-owned `Effect` handler). The sender is registered as a Phase-A
        // service so the IDE peer's `install(boot)` reads it via
        // `boot.service::<ProgrammaticDiffBus>()` — keeping the host free of any
        // IDE-peer reference (the bus is a generic diff-subsystem type). This is
        // diff-subsystem residue (alongside the `DiffSubsystem` bind below), not
        // an LSP channel.
        let (programmatic_diff_bus, programmatic_diff_rx) =
            boot.inbound_raw::<lattice_diff::ProgrammaticDiffRequest>();
        boot.register_service::<lattice_diff::ProgrammaticDiffBus>(programmatic_diff_bus);
        let (lsp, lsp_diagnostics) = build_lsp_subsystem(
            event_bus.clone(),
            &runtime_handle,
            lsp_logger.clone(),
            lsp_configuration_bus,
            lsp_show_document_bus,
            lsp_apply_edit_bus,
            lsp_show_message_request_bus,
        );
        // BC.8a: register the supervisor handle as a Phase-A service HERE (moved
        // up from the late service block) so `lattice_lsp::install` below can
        // read it via `boot.service::<LspSupervisorHandle>()` to register
        // `lsp-completion-mode`. The handle is host-created (the supervisor +
        // its four server-initiated buses live in `build_lsp_subsystem`, which
        // produces Editor fields — the diff `DiffSubsystem`-bind residue), so it
        // is registered host-side; `install` only reads it. Same `Arc` identity.
        boot.register_service(lsp.clone());

        // BC.3b: the Claude Code IDE peer is no longer hand-wired here. Its
        // server spawn, ex-commands, mode, service handle, and read/write tools
        // all install through one Phase-B call below
        // (`lattice_claude_code::install(&mut boot)`), against the generic
        // `SubsystemBoot` surface — the mode-ownership acid test.

        let builtins = grammar_builtins_populate(boot.commands_mut());
        // Register the built-in ex-commands as peers of motions /
        // operators / text objects (DESIGN.md §5.2.1). The returned
        // ids aren't held in App state today -- the parser front-
        // end looks them up by name -- but registering them
        // populates the registry so `:`-line parsing can route to
        // them.
        let _ex_builtins = lattice_grammar::ex_commands::populate(boot.commands_mut());

        // SU.3a: register surround operators in the shared CommandRegistry
        // so the surround-mode keymap can resolve its chain-form bindings.
        let surround_operators =
            lattice_mode::modes::surround::register_surround_operators(boot.commands_mut());

        // CSM.5: shared snippet-registry handle. Built before the
        // mode registry so `register_snippet_modes` can capture a
        // clone of the outer Arc -- the same outer Arc the Editor
        // field below holds. `:reload-snippets` updates the inner
        // via `.store()`; the mode + source see the fresh data on
        // the next produce().
        //
        // Constructed empty; the embedded built-in packs + user
        // packs load at the production startup seam via
        // `Editor::load_snippets_at_startup` (called from the TUI /
        // GPUI entry points alongside `load_persistent_config`), NOT
        // here. Keeping content-loading out of the constructor means
        // test `App`s start with an empty registry — completion
        // tests stay isolated from the built-in snippet set unless
        // they opt in via `:reload-snippets`.
        let snippet_registry_handle: Arc<ArcSwap<SnippetRegistry>> =
            Arc::new(ArcSwap::from_pointee(SnippetRegistry::new()));

        // M.5.1 (mode-architecture §9.6.1): build the mode
        // registry first so we can iterate it and register a
        // `:<mode-name>` toggle ex-command per mode. The mode
        // registry is then wrapped in `Arc`.
        // SN.3b: captured out of the registry-build block so boot
        // can fold `snippet.activation` / `snippet.languages` into
        // it (below) and `Editor` can hold the clone the cascade
        // re-folds.
        let snippet_activation_policy;
        // BC.3a: mode registration runs through `boot.modes_mut()` (the
        // registration seam). Order preserved verbatim from the prior
        // `let mr = { … }` block.
        lattice_mode::register_foundation_modes(boot.modes_mut());
        // SU.3a: register surround-mode with the operator handles it owns.
        lattice_mode::modes::surround::register_surround_modes(
            boot.modes_mut(),
            surround_operators,
        );
        lattice_syntax::register_language_modes(boot.modes_mut());
        // BC.8a: the LSP modes (`register_lsp_log_modes` + the
        // supervisor-handle-bound `lsp-completion-mode`) moved into
        // `lattice_lsp::install(boot)` (Phase-B list below). The completion mode
        // reads the supervisor handle via `boot.service::<LspSupervisorHandle>()`
        // (registered in Phase A above), so no host-side handle threading.
        lattice_oil::register_oil_modes(boot.modes_mut());
        lattice_file_tree::register_file_tree_modes(boot.modes_mut());
        snippet_activation_policy = lattice_snippet::register_snippet_modes(
            boot.modes_mut(),
            snippet_registry_handle.clone(),
        );
        // BC.4: terminal-mode registration moved into
        // `lattice_terminal::install` (Phase-B list below).
        crate::modes::register_buffer_kind_modes(boot.modes_mut());
        // PI.2 (preview isolation): `preview-mode` — the read-only minor
        // `mount_preview` activates on a previewed buffer's own stack.
        // Host-owned (no feature crate), registered alongside the other
        // host modes.
        boot.modes_mut()
            .register(crate::preview::PreviewMode)
            .expect("preview-mode must register without conflict");
        // MB.1 (rich minibuffer): `command-line-mode` — the major mode on
        // the synthetic `*command-line*` buffer. Host-owned; its Insert-
        // layer keymap (submit / cancel / history / completion chords)
        // resolves through `translate_mode_keymaps` at boot.
        boot.modes_mut()
            .register(crate::command_line_mode::CommandLineMode)
            .expect("command-line-mode must register without conflict");
        // MB.2: `command-line-expand-mode` — the tier-2 expanded band
        // major mode (same buffer, full-modal). Activated on expand,
        // deactivated on collapse. Owns the expanded band's option
        // overrides and keymap surface independently of tier 1.
        boot.modes_mut()
            .register(crate::command_line_expand_mode::CommandLineExpandMode)
            .expect("command-line-expand-mode must register without conflict");
        // MB.5a (rich minibuffer): `search-line-mode` — the major mode on
        // the synthetic `*search-line*` buffer. Host-owned; its Insert-
        // layer keymap (submit / cancel chords) resolves through
        // `translate_mode_keymaps` at boot.
        boot.modes_mut()
            .register(crate::search_line_mode::SearchLineMode)
            .expect("search-line-mode must register without conflict");
        // BC.7 (2026-06-24): `multibuffer-mode` (+ its `DocumentClosed`
        // cleanup subscriber), `narrow-minor-mode`, and the project-search
        // provider mode moved into `lattice_multibuffer::install(boot)`
        // (Phase-B install list below), alongside its commands + services +
        // the `MultibufferExcerptsReady` wake. The registry handle is now
        // crate-owned (created inside `install`); the host reads it back via
        // `services.get::<MultibufferRegistryHandle>()` in `resolve_narrow_target`.
        // BC.6/DX.7: `diff-mode` registration moved into
        // `lattice_diff::install(boot)` (Phase-B install list below),
        // alongside terminal + claude-code. K.1.c still gates the
        // `do`/`dp` chords on per-buffer diff participation.
        // BC.5: `emacs-keys-mode` is now a `lattice-mode` builtin — registered
        // with the foundation set by `register_foundation_modes` above, not
        // here. The host keeps only its keymap-layer push (keymap block below).
        crate::tutor::register_tutor_modes(boot.modes_mut());

        // ── BC.3b: Phase-B subsystem install list ──────────────────────────
        // One line per subsystem; each `install(boot)` does ALL of its own
        // wiring (modes, commands, services, the off-keystroke inbound bus,
        // event wakes) against the generic `SubsystemBoot` surface — zero host
        // internals (no `Editor::` method, no host `Action`/`Effect` variant).
        // Placed after the inline mode block + before the mode freeze, so an
        // installed mode is present when `register_mode_toggle_commands`
        // enumerates the registry, and while both registries are still open.
        // As each remaining subsystem migrates (terminal → emacs-keys → diff →
        // multibuffer → LSP, newest→oldest) its inline wiring collapses into an
        // `install` here. **BC.final (2026-06-25):** all subsystems are migrated;
        // this list is the single Phase-B touch-point (the acid test — a new
        // subsystem adds ONE `install` line, guarded by the BC.2 pins). NOTE:
        // `editor_boot` is THREE parts, not two — Phase-A primitives, the inline
        // host-native *builtins* (grammar / ex-commands / foundation+language+
        // oil+file-tree+snippet+tutor+buffer-kind modes / host actions, via
        // `boot.{commands,modes}_mut()`), and this Phase-B `install` list. The
        // builtins are not subsystems and register inline by design — the
        // earlier "two-list, `*_mut` removed" goal was falsified on inspection.
        //
        // AI (AI-1b / AG-4): the single `lattice-ai` install wires BOTH agent
        // transports (the AG-4 fold collapsed the former `lattice-claude-code`
        // crate into `lattice_ai::mcp`):
        //   - ACP client: AiLogger + supervisor + AiLogMode + :opencode /
        //     :ai-prompt / :ai-stop + AiClientHandle/AiLogger services. Agent
        //     output streams into per-session *ai:<provider>:<index>* rings
        //     (the :ai-log picker view is 12b).
        //   - MCP IDE peer: server spawn + `:claude-code-*` ex-commands +
        //     `claude-code-mode` + the `ClaudeCodeServerHandle` service + the I2
        //     read tools (buffer-store + diagnostics via `boot.service`) + the I3
        //     write bus (`boot.inbound`, whose drain token rides
        //     `into_registrations` into the Editor below).
        // AUX‑2: create VirtualRowProviderRegistry and register as a service so
        // subsystem installs can register headerline providers.
        let vrp: std::sync::Arc<crate::virtual_rows_worker::VirtualRowProviderRegistry> =
            std::sync::Arc::default();
        boot.register_service::<Arc<dyn lattice_mode::VirtualRowRegistrar>>(
            vrp.clone() as Arc<dyn lattice_mode::VirtualRowRegistrar>
        );
        lattice_ai::install(&mut boot);
        // terminal (BC.4): `terminal-mode` (+ Normal / Insert) registration. Its
        // `TerminalStoreHandle` service is a host-published primitive (in the
        // service block below) and its invocation runner stays host-side (the
        // shared invocation-runner mechanism) — see `lattice_terminal::install`.
        lattice_terminal::install(&mut boot);
        // diff (BC.6/DX.7): `diff-mode` registration. Two touch-points stay
        // host-side and are NOT mode-ownership violations — the `DiffSubsystem`
        // bind (uses the host `BufferRegistryDocumentResolver`; produces the
        // `diff_subsystem` / `diff_subscription_guard` / `diff_forwarders`
        // actor-loop fields below) and the `+N ~M` modeline element (its
        // `ModelineService` is created after this list). The `do`/`dp` keymap
        // is fully mode-owned (MO.x): `DiffMode::keymap()` + the K.2.4 pass —
        // see `lattice_diff::install` for the full rationale.
        lattice_diff::install(&mut boot);
        // multibuffer (BC.7): `multibuffer-mode` + `narrow-minor-mode` + the
        // project-search mode, the excerpt-jump motions + `:multibuffer-*` /
        // `:narrow` / `:widen` / `:search` ex-commands + the `zn` operator SPEC,
        // the `MultibufferRegistryHandle` + project-search services, and the
        // `MultibufferExcerptsReady` off-keystroke wake. The registry handle is
        // crate-owned (no host-state dependency). Residue staying host-side
        // (NOT mode-ownership violations): the universal `zn` operator BINDING
        // at the `Builtin` operator-pending layer (resolved by name below —
        // BC.7 decision A) and the `AppEffect::{Search,Narrow,MultibufferExpand}`
        // dispatch arms (Effect-vocabulary-is-the-host-boundary) — see
        // `lattice_multibuffer::install` for the full rationale.
        lattice_multibuffer::install(&mut boot);
        // CM.1: native compilation subsystem — registers
        // `compilation-mode` (major, ReadOnly + NoFile), the
        // `:compile`/`:recompile`/`:make` ex-commands (return
        // `Effect::AppAction(AppEffect::CompileRun)`, applied by the
        // host arm — creates the `*compilation*` buffer host-side +
        // runs the `CompilationServiceHandle`),
        // the `CompilationServiceHandle` process-lifecycle service,
        // and the `CompilationOutputPushed` off-keystroke wake so the
        // streaming `*compilation*` buffer repaints without a keypress.
        lattice_compilation::install(&mut boot);
        // DB.2: dashboard subsystem — registers `dashboard-mode` (major), the
        // `:dashboard` ex-command (returns `Effect::OpenDashboard`, applied by
        // `Editor::do_open_dashboard`), and the built-in `DashboardRegistry`
        // service. See `lattice_dashboard::install` + dashboard.md §9.
        lattice_dashboard::install(&mut boot);
        // PL8.H.2: the plugin-manager view — registers `plugins-mode` (major,
        // read-only) + the `:plugins` ex-command (returns
        // `Effect::OpenSyntheticBuffer`, applied by `Editor::open_synthetic_buffer`).
        // A pure provider crate: the mode resolves `PluginLoaderHandle` at
        // activation, so this needs no ordering vs the loader install below.
        lattice_plugin_manager::install(&mut boot);
        // PO.4.1: the plugin boundary-trace views — registers `plugin-trace-mode`
        // (major, read-only) + the `:plugin-trace` ex-command (returns
        // `Effect::OpenSyntheticBuffer`). A pure provider crate: the mode resolves
        // `PluginTracerHandle` at activation (registered by the loader install),
        // so this needs no ordering vs the loader install.
        lattice_plugin_trace::install(&mut boot);
        // LSP (BC.8a — last + largest, sub-sliced BC.8a–e): registers the LSP
        // modes (`lsp-completion-mode` reads the supervisor handle via
        // `boot.service::<LspSupervisorHandle>()`, registered in Phase A) + the
        // four `workspace/*/refresh` off-keystroke wakes (`boot.wake_on_event`).
        // Residue staying host-side (NOT violations): `build_lsp_subsystem`
        // (produces Editor fields — the diff `DiffSubsystem`-bind class), the
        // host-created services (logger / diagnostics-query), and the four
        // inbound buses + drains (reshaped onto `boot.inbound::<T>` in BC.8b–e).
        // See `lattice_lsp::install` for the full rationale.
        lattice_lsp::install(&mut boot);

        // BC.3a: freeze the mode registry into its shared `Arc` BEFORE
        // `register_mode_toggle_commands`. The toggle helper needs
        // `&mut CommandRegistry` + `&ModeRegistry` simultaneously; both live
        // in `boot`, so a concurrent `boot.commands_mut()` + mode-read borrow
        // would conflict. Freezing first hands back an `Arc<ModeRegistry>`
        // (derefs to `&ModeRegistry`); the registry is fully populated here,
        // so the auto-generated `:<mode-name>` toggles are identical.
        let mode_registry = boot.freeze_mode_registry();
        register_mode_toggle_commands(boot.commands_mut(), &mode_registry.load());
        // PL8.B: share the runtime-mutable mode registry so the plugin loader can
        // RCU-register a mode plugin at runtime (`service::<ModeRegistryHandle>()`).
        boot.register_service::<lattice_mode::ModeRegistryHandle>(mode_registry.clone());

        // Slice 8.i action ids: each `CommandKind::Action` entry
        // returns `Effect::AppAction(AppEffect::Foo)`; per-mode
        // keymap modules consume the resulting `ActionIds` to
        // build typed `CommandInvocation`s for chord bindings.
        let action_ids = crate::actions::populate(boot.commands_mut(), &builtins);

        // `repl-mode` (foundation minor, registered above) owns its
        // `action:repl-focus-input` command. Register it here so
        // `translate_mode_keymaps` resolves the mode's keymap `cmd` name and
        // `register_mode_action_handlers` binds the handler — both run later in
        // boot. The `register_ai_conversation_actions` pattern, kept with the
        // mode's own crate.
        lattice_mode::register_repl_mode_actions(boot.commands_mut());

        // BC.7 (2026-06-24): the multibuffer excerpt-jump motions
        // (`]e`/`[e`/`]E`/`[E`), the `:multibuffer-*` / `:narrow` / `:widen` /
        // `:search` ex-commands, AND the `zn` narrow operator SPEC are all
        // registered by `lattice_multibuffer::install(boot)` above. The host no
        // longer threads the operator's `OperatorId` from registration to the
        // `zn` binding — the binding resolves `operator:narrow` by name (the
        // K.2.5 motion name-resolution pattern); see the
        // `register_operator_bindings` call below.

        // N.1.4c: register the structural (tree-sitter) text objects
        // (`af`/`if`/`ac`/`ic`/`aa`/`ia`/`al`/`il`) -- owned by
        // lattice-syntax -- and capture their ids so the universal
        // operator-pending keymap (`register_normal_bindings` + the `zn`
        // operator below) can bind their chords. Must run while the command
        // registry is still mutable (before `freeze_command_registry` below).
        let syntax_textobject_ids =
            lattice_syntax::register_syntax_text_objects(boot.commands_mut());

        // TSM.4: register the sixteen structural (tree-sitter) MOTIONS
        // (`]f`/`[f`/`]F`/`[F`, `]c`/`[c`/`]C`/`[C`, `]a`/`[a`/`]A`/`[A`,
        // `]l`/`[l`/`]L`/`[L`) -- the motion counterpart to the structural
        // text objects registered just above. Same discipline: owned by
        // lattice-syntax, threaded to the keymap binders so the host only
        // wires chord -> id. Must run while the command registry is still
        // mutable (before `freeze_command_registry` below).
        let syntax_motion_ids = lattice_syntax::register_syntax_motions(boot.commands_mut());

        // §5.11.3 completion pipeline: register the built-in
        // generators / matchers / rankers / annotators and wire
        // sensible defaults (prefix matcher, score ranker, kind
        // + doc annotators).
        let mut completion_registry = CompletionRegistry::new();
        let _completion_builtins = lattice_completion::populate(&mut completion_registry);

        // Help-topic registry + its completion generator
        // (`gen:help-topics`). Registering here lets `:help <Tab>`
        // enumerate built-in + plugin-supplied topics through
        // the same pipeline `:e <Tab>` and `:describe-command <Tab>`
        // use.
        let help_topics = crate::help_topics::builtin_topics();
        completion_registry.register_generator(
            "gen:help-topics",
            "Every registered free-form help topic (`:help <topic>`).",
            crate::help_topics::HelpTopicsGenerator {
                topics: help_topics.clone(),
            },
        );

        // Subscribe the editor's cascade-handler channel to
        // `OptionChanged` events on the bus. The receiver lives
        // on `Editor.option_change_rx`; the runtime's per-tick
        // drain pulls from it. This decouples cascades from the
        // publish path: any consumer that calls `config.set`
        // -- the cmdline, plugins, the future customize buffer
        // view -- triggers the cascade through the same channel.
        let (option_tx, option_change_rx) = tokio::sync::mpsc::unbounded_channel();
        event_bus.subscribe(
            lattice_runtime::EventFilter::kind(lattice_protocol::EventKind::OptionChanged),
            lattice_runtime::SubscriptionTarget::Channel(option_tx),
        );
        // LSP log live-tail.
        let (lsp_log_tx, lsp_log_event_rx) =
            tokio::sync::mpsc::unbounded_channel::<lattice_lsp::LspLogPushed>();
        event_bus.subscribe_typed(lsp_log_tx);
        // ML.3c: LSP `$/progress` + `experimental/serverStatus` are no
        // longer accumulated host-side. `lattice_lsp::modeline`'s
        // forwarder subscribes them, folds them into the shared
        // `LspProgressStore` (created below), and pushes the `lsp` element
        // per attached buffer; the host reads the same store only for
        // `:lsp-progress-cancel`.
        // `LspBufferDetached`: `LspMode::on_deactivate` publishes
        // this; the per-tick drain calls `lsp_close_buffer` for
        // each so the wire-level `didClose` + `buffer_uris`
        // cleanup runs *after* the mode lifecycle.
        let (lsp_detach_tx, lsp_detach_rx) =
            tokio::sync::mpsc::unbounded_channel::<lattice_lsp::LspBufferDetached>();
        event_bus.subscribe_typed(lsp_detach_tx);
        // M-async.3: mode lifecycle events for `ModeActivationFailed`
        // (and aborted cascade parents); the per-tick drain calls
        // `deactivate_mode_by_id` on each.
        let (mode_lifecycle_tx, mode_lifecycle_rx) =
            tokio::sync::mpsc::unbounded_channel::<lattice_mode::ModeEvent>();
        event_bus.subscribe_typed(mode_lifecycle_tx);
        // ML.3: modeline element content pushed by modes/plugins over the
        // bus. `drain_modeline_element_updates` applies each into the
        // shared `modeline` content store (single-writer, actor thread);
        // a separate subscription in the L1c wake block fires
        // `async_landed` so the push repaints off-keystroke (§12 wake).
        let (modeline_update_tx, modeline_update_rx) =
            tokio::sync::mpsc::unbounded_channel::<lattice_mode::ModelineElementUpdate>();
        event_bus.subscribe_typed(modeline_update_tx);
        // MA.2: minor-activation resolver input. One channel
        // subscribed to `Event::MajorEntered`; the per-tick
        // `drain_minor_activation` reads it, looks up each buffer's
        // kind, and auto-activates the minors whose ActivationPolicy
        // admits the entered major (Global gated to document buffers).
        let (major_entered_tx, major_entered_rx) =
            tokio::sync::mpsc::unbounded_channel::<lattice_protocol::Event>();
        event_bus.subscribe(
            lattice_runtime::EventFilter::kind(lattice_protocol::EventKind::MajorEntered),
            lattice_runtime::SubscriptionTarget::Channel(major_entered_tx),
        );
        // CI.4: the mode-enablement bridge. A plugin's `enable-mode` publishes
        // `Event::ModeEnablementRequested`; the per-tick `drain_mode_enablement`
        // flips the mode registry + re-activates open buffers (the guest can't
        // reach the activator, so it routes through here — config-and-init.md §6).
        let (mode_enablement_tx, mode_enablement_rx) =
            tokio::sync::mpsc::unbounded_channel::<lattice_protocol::Event>();
        event_bus.subscribe(
            lattice_runtime::EventFilter::kind(
                lattice_protocol::EventKind::ModeEnablementRequested,
            ),
            lattice_runtime::SubscriptionTarget::Channel(mode_enablement_tx),
        );
        // SN.2: the live snippet session, shared between the host
        // (creates it on expand) and `SnippetActiveMode`'s
        // `<Tab>`/`<S-Tab>` handlers (navigate it). The same Arc is
        // both stored on the Editor and registered in ServiceRegistry.
        let snippet_session: lattice_snippet::SnippetSessionHandle =
            Arc::new(lattice_snippet::SnippetSession::new());
        // Inlay-hint refresh.
        let (lsp_inlay_refresh_tx, lsp_inlay_refresh_rx) =
            tokio::sync::mpsc::unbounded_channel::<lattice_lsp::LspInlayHintRefresh>();
        event_bus.subscribe_typed(lsp_inlay_refresh_tx);
        // Semantic-tokens refresh.
        let (lsp_semantic_tokens_refresh_tx, lsp_semantic_tokens_refresh_rx) =
            tokio::sync::mpsc::unbounded_channel::<lattice_lsp::LspSemanticTokensRefresh>();
        event_bus.subscribe_typed(lsp_semantic_tokens_refresh_tx);
        // Pull-diagnostic refresh.
        let (lsp_diagnostic_refresh_tx, lsp_diagnostic_refresh_rx) =
            tokio::sync::mpsc::unbounded_channel::<lattice_lsp::LspDiagnosticRefresh>();
        event_bus.subscribe_typed(lsp_diagnostic_refresh_tx);
        // Code-lens refresh.
        let (lsp_code_lens_refresh_tx, lsp_code_lens_refresh_rx) =
            tokio::sync::mpsc::unbounded_channel::<lattice_lsp::LspCodeLensRefresh>();
        event_bus.subscribe_typed(lsp_code_lens_refresh_tx);
        // `*messages*` buffer live-tail subscriber.
        let (message_event_tx, message_event_rx) =
            tokio::sync::mpsc::unbounded_channel::<lattice_runtime::MessagePushed>();
        event_bus.subscribe_typed(message_event_tx);

        // msg-mode.1: install the global `tracing::Subscriber`
        // bridge. `install_messages_subscriber` is idempotent
        // (process-wide `set_global_default`); only the first
        // call succeeds. Tests with multiple Editor instances
        // share the first-installed layer, which is fine because
        // tests use the per-test layer constructor for unit
        // coverage rather than relying on the global install.
        //
        // 2026-05-22 messages-overhaul: read initial filter from
        // the runtime's `boot_log_level` (set by the CLI from
        // -v/-q/--log-level flags before App::new runs). Falls
        // back to "info" when unset (library / test callers).
        // The subscriber composes a fmt layer + MessagesLayer so
        // tracing events land in BOTH stderr and `*messages*`.
        // Live-editable via `:set messages.filter=<level>`.
        let messages_ring = Arc::new(std::sync::Mutex::new(MessagesRing::default()));
        let initial_filter =
            lattice_runtime::boot_log_level().unwrap_or_else(|| "info".to_string());
        // Issue #36 (2026-05-22): TUI peers must NOT enable
        // stderr — stderr IS the terminal ratatui paints into.
        // CLI sets `boot_stderr_enabled` based on the selected
        // renderer (false for TUI; true for GPUI). Library
        // callers / tests that don't set it get the safe
        // default (false — never accidentally corrupt a TUI
        // screen).
        let stderr_enabled = lattice_runtime::boot_stderr_enabled().unwrap_or(false);
        let _ = lattice_runtime::install_messages_subscriber(
            messages_ring.clone(),
            event_bus.clone(),
            &initial_filter,
            stderr_enabled,
        );
        // Wire the logger's publisher to the same bus. The
        // closure captures an Arc<EventBus> clone so the
        // logger's lifetime is independent of any single field.
        let bus_for_log = event_bus.clone();
        lsp_logger.set_event_publisher(Arc::new(move |event| {
            bus_for_log.publish_typed(event);
        }));

        // 4.4.o: seed the LSP logger from typed-options defaults.
        // Invalid values were filtered by the option validators
        // before they hit the registry; we treat any miss here
        // as "use the built-in default the logger already has."
        if let Some(level_str) = config.get_typed::<lattice_config::core_options::LspLogLevel>()
            && let Some(level) = lattice_lsp::LogLevel::parse(&level_str)
        {
            lsp_logger.set_default_level(level);
        }
        if let Some(cap) = config.get_typed::<lattice_config::core_options::LspLogCapacity>() {
            lsp_logger.set_default_capacity((*cap).max(0) as usize);
        }

        // `gen:options` -- completion source for `:set <Tab>` and
        // `:set name=<Tab>`. Wired to the same `ConfigRegistry`
        // the `:set` parser consults so completions never drift
        // from the canonical option list.
        completion_registry.register_generator(
            "gen:options",
            "Every registered option name + (when applicable) its enumerated values.",
            lattice_config::OptionsGenerator::new(config.clone()),
        );
        // Editor-state completion sources for `:describe-*` /
        // `:customize` / `:lsp-*` commands. Each generator
        // captures the slice of state it needs; names are
        // stable so `ArgSpec::completion` references stay in
        // sync.
        completion_registry.register_generator(
            "gen:events",
            "Every typed event registered via `register_event!`; used by `:describe-event <Tab>`.",
            crate::host_generators::EventsGenerator,
        );
        completion_registry.register_generator(
            "gen:log-levels",
            "The five log levels (`error`/`warn`/`info`/`debug`/`trace`); used by `:lsp-log-level <Tab>`.",
            crate::host_generators::LogLevelsGenerator,
        );
        completion_registry.register_generator(
            "gen:lsp-servers",
            "Currently running LSP server ids; used by `:lsp-log <Tab>` / `:lsp-restart <Tab>` / etc.",
            crate::host_generators::LspServersGenerator { lsp: lsp.clone() },
        );
        completion_registry.register_generator(
            "gen:customize",
            "Group names + mode names; used by `:customize <Tab>`.",
            crate::host_generators::CustomizeNamesGenerator {
                registry: Arc::downgrade(&mode_registry),
            },
        );

        // BC.3a: freeze the command registry into its shared `Arc` — all
        // command registration (builtins, ex-commands, toggles, actions,
        // multibuffer motions/ex-commands, narrow, syntax text objects) is
        // done by here, and the `Arc` is consumed just below (picker sources
        // that capture it, document handles). Subsequent `boot.commands_mut()`
        // panics — a boot-sequencing bug.
        let registry = boot.freeze_command_registry();
        // T.3/T.4 (theme-system): the theme-element registry, seeded
        // with the default palette + all builtin elements (resolved +
        // ready). Created here (ahead of the struct literal) so the
        // T.12a colorscheme picker source can capture a clone, AND the
        // `services:` block / `builtin_element_ids` capture / the
        // `theme_registry` field all share the one Arc. See
        // theme-system.md §3.5 / §7.
        let theme_registry: lattice_theme::ThemeRegistryHandle =
            Arc::new(lattice_theme::InMemoryThemeRegistry::with_defaults());
        // MP.2b: create the keymap handle here (ahead of the
        // `keymap:` struct field that registers all bindings) so
        // the commands picker can capture a reverse-lookup
        // adapter over the *same* registry. `KeymapHandle` is
        // dependency-free and `Clone` over an inner
        // `Arc<KeymapRegistry>`; the adapter holds a clone of the
        // registry's `ArcSwap` reverse cache, so bindings
        // registered later (in the `keymap:` block, via the moved
        // handle) are visible to the picker at open time. See
        // `marginalia.md` §6 + the wiring rationale in the
        // picker-marginalia slice plan (MP.2b).
        let keymap_handle = crate::keymap_registry::KeymapHandle::new();
        let keybinding_reverse: Arc<dyn lattice_completion::KeymapReverseLookup> =
            crate::keymap_registry::KeymapReverseLookupHandle::new(
                &keymap_handle,
                registry.clone(),
            );
        // PH.3: the shared lang registry — created here (ahead of the
        // document `Syntax` below, which reuses it) so the grep picker's
        // preview highlighter selects grammars from the same set the
        // buffers use. The highlighter parses each grep hit's preview
        // line on the grep blocking task (off the render thread; the
        // picker crate has no syntax dep). See picker-preview-highlight.md §7.
        let lang_registry = LangRegistry::standard().expect("standard lang registry");
        let grep_highlighter: Option<
            Arc<dyn lattice_picker::picker_sources::GrepPreviewHighlighter>,
        > = Some(crate::grep_highlight::SyntaxGrepHighlighter::new(
            lang_registry.clone(),
        ));
        // PL8.B: held behind `ArcSwap` so the plugin loader can RCU-register a
        // loaded picker plugin's source at runtime while the picker-open path
        // reads it wait-free. Registered as a `PickerRegistryHandle` service
        // below so `lattice-plugin-loader` reaches it without a host dep.
        let picker_registry: lattice_picker::PickerRegistryHandle =
            Arc::new(arc_swap::ArcSwap::from_pointee(built_in_picker_registry(
                registry.clone(),
                config.clone(),
                keybinding_reverse,
                grep_highlighter,
                snippet_registry_handle.clone(),
                theme_registry.clone(),
            )));
        boot.register_service::<lattice_picker::PickerRegistryHandle>(picker_registry.clone());

        // PL8.E: the runtime-mutable registry of async gutter-decoration
        // producers. Held behind `ArcSwap` so the plugin loader
        // (`drain_decorations`) RCU-registers a loaded decoration plugin's
        // producer while the host's per-tick refresh reads it wait-free.
        // Registered as a service so `lattice-plugin-loader` reaches it without
        // a host dep, and cloned onto the `Editor` below so the refresh drives
        // it. Starts empty — no producer until a decoration plugin loads.
        let decoration_registry: lattice_mode::GutterDecorationSourceRegistryHandle = Arc::new(
            arc_swap::ArcSwap::from_pointee(lattice_mode::GutterDecorationSourceRegistry::new()),
        );
        boot.register_service::<lattice_mode::GutterDecorationSourceRegistryHandle>(
            decoration_registry.clone(),
        );

        // MRU cache load. Honor `picker.mru.persist` at boot;
        // failure modes: no persist path (sandboxed), no file
        // (fresh install), or corrupt file (log + reset).
        let persist = config
            .get_typed::<lattice_config::core_options::PickerMruPersist>()
            .map(|b| *b)
            .unwrap_or(true);
        let picker_mru_path = if persist {
            lattice_picker::default_persist_path()
        } else {
            None
        };
        let mru_cap = config
            .get_typed::<lattice_config::core_options::PickerMruCapPerNamespace>()
            .map(|n| (*n).max(1) as usize)
            .unwrap_or(lattice_picker::DEFAULT_CAP_PER_NAMESPACE);
        let picker_mru = match &picker_mru_path {
            Some(path) => match lattice_picker::PickerMruIndex::load_from(path) {
                Ok(Some(idx)) => idx,
                Ok(None) => lattice_picker::PickerMruIndex::with_cap(mru_cap),
                Err(e) => {
                    // Route through tracing, not raw stderr: stderr may be
                    // the TUI's terminal (see the picker-MRU-save fix in
                    // dispatch.rs). → *messages*.
                    tracing::warn!("discarding corrupt MRU cache at {}: {e}", path.display());
                    lattice_picker::PickerMruIndex::with_cap(mru_cap)
                }
            },
            None => lattice_picker::PickerMruIndex::with_cap(mru_cap),
        };
        completion_registry.register_generator(
            "gen:picker-sources",
            "Every source id registered with the `PickerRegistry`; \
             drives `:picker <Tab>` completion.",
            crate::host_generators::PickerSourcesGenerator {
                registry: Arc::downgrade(&picker_registry),
            },
        );
        // T.9.d follow-up: `gen:elements` — theme-element / face names for
        // `:describe-element <Tab>` / `:describe-face <Tab>`. Holds a clone of
        // the same `theme_registry` handle the renderers + `:describe-element`
        // handler read, so completion never drifts from the live element set.
        completion_registry.register_generator(
            "gen:elements",
            "Every registered theme element / face name; used by \
             `:describe-element <Tab>` / `:describe-face <Tab>`.",
            crate::host_generators::ElementsGenerator {
                registry: theme_registry.clone(),
            },
        );
        // MB.5: `gen:history-kinds` — valid args for `:history <Tab>`.
        completion_registry.register_generator(
            "gen:history-kinds",
            "Valid history kind arguments (`commands`, `searches`); used by `:history <Tab>`.",
            crate::host_generators::HistoryKindsGenerator,
        );

        // `lang_registry` (one per Editor, shared between the document
        // buffer's `Syntax`, every `HelpBuffer`, and the grep preview
        // highlighter) was created above with the picker registry.
        let lang = Lang::detect_from_path(document.path());
        // Build the underlying `Syntax` synchronously + seed it
        // with one parse of the initial text so the renderer's
        // first frame has highlights without waiting for the
        // worker. After that the handle takes over: subsequent
        // `request_reparse` calls run the parse on a worker
        // thread; the renderer reads the latest snapshot via
        // `ArcSwap`.
        let initial_text = document.text();
        let initial_text_version = document.text_version();
        // BC.3a: `async_landed` is a Phase-A primitive (created up top, owned
        // by `boot`). The reparse worker below takes it as its `on_publish`
        // wake and the diagnostics layer's `set_wake` arms it here.
        // Wake the render loop on every server `publishDiagnostics`
        // push so diagnostic changes — a new error OR the clear when one
        // is fixed — repaint off-keystroke, instead of waiting for the
        // next cursor-driven publish. The layer fires `async_landed`
        // from `apply`; shared across every actor pump via the cloned
        // `DiagnosticsLayer` (Arc-backed). See lsp-architecture.md §12.
        lsp_diagnostics.set_wake(async_landed.clone());
        let syntax: Option<SyntaxHandle> =
            match Syntax::for_language_with_registry(lang, lang_registry.clone()) {
                Ok(Some(mut s)) => {
                    s.parse_at(&initial_text, initial_text_version);
                    let al = async_landed.clone();
                    let eb = event_bus.clone();
                    Some(SyntaxHandle::seeded_with_runtime(
                        s,
                        &runtime_handle,
                        Some(std::sync::Arc::new(move || {
                            al.notify_one();
                            eb.publish_typed(crate::events::SyntaxReparsed);
                        })),
                    ))
                }
                _ => None,
            };
        let last_parsed_text_version = initial_text_version;

        // M.2.b.0.A: allocate BufferId before spawning so the
        // handle carries its own registry id (used by
        // `MotionContext::buffer_id` for kind-specific motion
        // handlers).
        let document_buffer_id = BufferId::next();
        // Hand the document to the actor (DESIGN.md §5.7).
        // After this call the only way to read or mutate it is
        // through the returned `RopeDocumentHandle`.
        let handle = spawn_document(document_buffer_id, document, registry.clone());
        let snapshot_cache = handle.snapshot_cache();
        // M.0: wrap the handle in `ActiveDocument` so the slot
        // can hold either a regular doc or (M.1+) a multibuffer
        // handle without kind-branching at the use site.
        let document = lattice_runtime::ActiveDocument::new(handle);
        let initial_pane = PaneState {
            id: PaneId::next(),
            buffer: BufferKind::Document,
            buffer_id: document_buffer_id,
            cursor: Position::ZERO,
            scroll: 0,
            leftcol: 0,
            // Populated by the renderer's per-frame layout pass
            // (Issue #25, 2026-05-22). Zero at boot is safe — the
            // first frame's `set_viewport_*` calls update before
            // any motion / ensure-visible reads.
            viewport_height: 0,
            viewport_width: 0,
            committed_buffer_id: None,
        };
        let pane_tree = PaneTree::single(initial_pane);

        // Seed the buffer registry with the initial document. `buffers` is a
        // Phase-A primitive (created empty up top, handed to `boot` via the
        // `BufferStoreHandle`); this seeding is observable through that handle
        // (shared inner Arc). The hot-path `Editor.document` / `Editor.syntax`
        // / `Editor.last_parsed_text_version` mirror what's stored here for the
        // active buffer; switching buffers swaps them.
        buffers.insert(BufferEntry {
            id: document_buffer_id,
            flags: BufferFlags::default(),
            data: BufferData::Document(DocumentEntry {
                id: document_buffer_id,
                handle: document.as_arc(),
            }),
            name: None,
        });

        // M.3.2.c.4: seed the initial document's buffer-locals
        // so reader-side flips can route through the locals map
        // for inactive buffers uniformly. The active buffer's
        // hot-path fields (Editor.syntax / Editor.folds / ...)
        // are still canonical until the readers flip; locals are
        // updated at each de-activation boundary via
        // `seed_document_entry_locals`.
        let mut buffer_locals: HashMap<BufferId, lattice_mode::BufferLocals> = HashMap::new();
        let mut initial_locals = lattice_mode::BufferLocals::default();
        initial_locals.insert(crate::modes::DocumentSyntax(None));
        initial_locals.insert(crate::modes::DocumentLastParsedTextVersion(0));
        initial_locals.insert(crate::modes::DocumentLastSyncedSyntaxVersion(0));
        initial_locals.insert(crate::modes::DocumentFolds(Vec::new()));
        buffer_locals.insert(document_buffer_id, initial_locals);

        // Phase 5.8.AF.5 / Slice X2.4 (gut + rename B4.2):
        // instantiate the overlay worker's shared cell BEFORE the
        // Editor literal so we can hand the worker its own clone at
        // spawn time. The Editor literal below assigns these into the
        // struct fields explicitly (overriding the
        // `..Editor::default()` tail) so all three holders (Editor,
        // RenderState, worker) share the SAME Arc identity — the
        // worker's writes into the quads cell are observable through
        // every
        // `render_state.load_full().syntax.static_overlay_quads.load()`.
        let overlay_wake = crate::editor::OverlayWake::default();
        // Perf plan B.2 slice B.2.a: cell carrying the worker's
        // per-row pre-bucketed static-overlay quads (doc_highlight /
        // all_matches / substitute). Created here so the worker
        // (spawned below), the Editor field, and the
        // `SyntaxRenderState.static_overlay_quads` clone on every
        // `publish_render_state` all share the SAME `Arc` identity.
        // Without this shared identity the worker's `.store()` would
        // not be observable through `RenderState.load_full()` after
        // later publishes.
        let syntax_static_overlay_quads_cell: std::sync::Arc<
            arc_swap::ArcSwap<crate::render_state::StaticOverlayQuads>,
        > = std::sync::Arc::default();
        // BC.3a: `render_state_arc` is a Phase-A primitive (created up top,
        // owned by `boot`); the workers below + every `publish_render_state`
        // share its exact Arc identity.
        // X1b: paint-request signal. Created here so the worker
        // (spawned below) and the Editor (constructed below) hold
        // the same `Arc<Notify>`. The renderer peer subscribes to
        // `editor.paint_request` and translates wakes to its own
        // redraw mechanism (TUI I.3: a bridge task forwards each notify
        // as a `Wake::Repaint` onto the input-reader channel the main
        // loop blocks on; GPUI: foreground-executor future that calls
        // `cx.notify()`).
        let paint_request: std::sync::Arc<tokio::sync::Notify> = std::sync::Arc::default();
        runtime_handle.spawn(crate::overlay_worker::run(
            render_state_arc.clone(),
            overlay_wake.clone(),
            syntax_static_overlay_quads_cell.clone(),
            paint_request.clone(),
        ));

        // S2.2 (2026-05-26): cell-builder worker. Same same-Arc-
        // identity pattern as the overlay worker — `cells_wake`
        // and `cells_matrix_cell` are constructed here, cloned into
        // the worker, then assigned into the Editor literal below
        // (overriding `..Editor::default()` so all three holders
        // share the SAME Arc identities). The worker's `.store()`
        // on `cells_matrix_cell` is therefore observable through
        // every `render_state.load_full().cells.matrix.load()`.
        let cells_wake = crate::editor::CellsWake::default();
        let cells_matrix_cell: std::sync::Arc<arc_swap::ArcSwap<lattice_cells::CellMatrix>> =
            std::sync::Arc::default();
        // D.4.d.0 (2026-05-29): per-document cells-matrix
        // registry. Seed with the initial document's matrix
        // sharing the same Arc identity as
        // `cells_matrix_cell` so the existing worker write
        // path and renderer read path stay coherent.
        // Subsequent buffer switches insert their own
        // entries lazily via `Editor::cells_matrix_for`.
        let cells_matrices: std::sync::Arc<
            std::sync::Mutex<
                std::collections::HashMap<
                    lattice_core::BufferId,
                    std::sync::Arc<arc_swap::ArcSwap<lattice_cells::CellMatrix>>,
                >,
            >,
        > = {
            let mut map = std::collections::HashMap::new();
            map.insert(document_buffer_id, cells_matrix_cell.clone());
            std::sync::Arc::new(std::sync::Mutex::new(map))
        };
        // B2.1 (2026-06-04): per-line display-cache output cell +
        // per-document registry. Same Arc-identity discipline as the
        // cells seed above: the active document's registry entry
        // shares its Arc with `display_matrix_cell` so the worker's
        // future `.store()` (B2.2) and the renderer's read land on
        // the same cell. Subsequent buffers lazy-insert via
        // `Editor::display_matrix_for`.
        let display_matrix_cell: std::sync::Arc<
            arc_swap::ArcSwap<crate::display_matrix::DisplayMatrix>,
        > = std::sync::Arc::default();
        let display_matrices: std::sync::Arc<
            std::sync::Mutex<
                std::collections::HashMap<
                    lattice_core::BufferId,
                    std::sync::Arc<arc_swap::ArcSwap<crate::display_matrix::DisplayMatrix>>,
                >,
            >,
        > = {
            let mut map = std::collections::HashMap::new();
            map.insert(document_buffer_id, display_matrix_cell.clone());
            std::sync::Arc::new(std::sync::Mutex::new(map))
        };
        // D.4.d.1.b (2026-05-29): the worker now writes per
        // pane via `cells.panes[i].matrix` (each entry's cell
        // comes from `Editor::cells_matrix_for`), so the
        // single top-level `cells_matrix_cell.clone()` arg the
        // pre-d.1.b worker took is gone. The active pane's
        // entry shares Arc identity with `cells_matrix_cell`
        // via the seeded `cells_matrices` registry above, so
        // the existing renderer read path keeps landing on
        // the worker's writes until D.4.d.1.c teaches the
        // renderer about the per-pane map.
        runtime_handle.spawn(crate::cells_worker::run(
            render_state_arc.clone(),
            cells_wake.clone(),
            paint_request.clone(),
        ));

        // D.0a.1 (2026-05-29): virtual-rows worker. Sibling of
        // the cells worker — same Arc-sharing discipline so
        // `Editor::virtual_rows_matrix_cell` and the worker's
        // sibling clone resolve to the same publish target.
        let virtual_rows_wake = crate::editor::VirtualRowsWake::default();
        let virtual_rows_matrix_cell: std::sync::Arc<
            arc_swap::ArcSwap<lattice_cells::VirtualRowMatrix>,
        > = std::sync::Arc::default();
        // D.4.d.2.0 (2026-05-29): per-document virtual-rows
        // matrix registry. Seed with the initial document's
        // matrix sharing the same Arc identity as
        // `virtual_rows_matrix_cell` so the existing single-
        // writer hot path (virtual_rows_worker →
        // virtual_rows_matrix_cell → RenderState.virtual_rows.matrix)
        // stays bit-identical. Subsequent buffer switches insert
        // their own entries lazily via
        // `Editor::virtual_rows_matrix_for`. Mirror of the
        // `cells_matrices` seeding above.
        let virtual_rows_matrices: std::sync::Arc<
            std::sync::Mutex<
                std::collections::HashMap<
                    lattice_core::BufferId,
                    std::sync::Arc<arc_swap::ArcSwap<lattice_cells::VirtualRowMatrix>>,
                >,
            >,
        > = {
            let mut map = std::collections::HashMap::new();
            map.insert(document_buffer_id, virtual_rows_matrix_cell.clone());
            std::sync::Arc::new(std::sync::Mutex::new(map))
        };
        // Reuse the VirtualRowProviderRegistry created during Phase A (the
        // `vrp` binding above). Cloning the `Arc` shares the same registry,
        // so the service registration, the worker below, and the Editor field
        // all see the same providers.
        let virtual_row_providers = vrp.clone();
        runtime_handle.spawn(crate::virtual_rows_worker::run(
            render_state_arc.clone(),
            virtual_rows_wake.clone(),
            virtual_row_providers.clone(),
            paint_request.clone(),
        ));

        // Event-bus → cells_wake bridges.
        //
        // SyntaxReparsed: fired by the on_publish callback in every
        // SyntaxHandle after a snapshot is published. Wakes the cells
        // worker so a fresh display matrix is built once tree-sitter
        // finishes — without this, a reparse that completes with no
        // keystroke in flight doesn't repaint.
        {
            use tokio::sync::mpsc;
            let (tx, mut rx) = mpsc::unbounded_channel::<crate::events::SyntaxReparsed>();
            event_bus.subscribe_typed::<crate::events::SyntaxReparsed>(tx);
            let cw = cells_wake.clone();
            runtime_handle.spawn(async move {
                while rx.recv().await.is_some() {
                    cw.0.notify_one();
                }
            });
        }

        // BC.7 (2026-06-24): the `MultibufferExcerptsReady` →
        // `async_landed` wake forwarder moved into
        // `lattice_multibuffer::install(boot)` as `boot.wake_on_event::<…>()`
        // (the wake is now baked into the primitive). Behaviour unchanged:
        // each appended batch fires `async_landed` so the actor republishes
        // render state (picking up the new excerpt_syntax entries) and the
        // `AsyncRenderStatePublished` → cells bridge below wakes `cells_wake`
        // AFTER the ArcSwap store — same ordering, no race.

        // L1c: wake the render pipeline when render-relevant LSP
        // events arrive off-keystroke. Without this, `$/progress` and
        // the `*/refresh` notifications only reach the screen on the
        // next keypress (their drains run inside `run_tick_pending`), so
        // indexing progress accumulates then batch-drains to empty and
        // is never seen, and a refresh that lands while idle doesn't
        // repaint. Mirrors the SyntaxReparsed / MultibufferExcerptsReady
        // forwarders: dedicated subscriptions whose only job is to fire
        // `async_landed`; the existing per-type drain channels still do
        // the accumulation. See lsp-architecture.md §12,
        // slice-plans/lsp.md L1c.
        {
            use tokio::sync::mpsc;
            async fn wake_on<T: Send + 'static>(
                mut rx: mpsc::UnboundedReceiver<T>,
                al: std::sync::Arc<tokio::sync::Notify>,
            ) {
                while rx.recv().await.is_some() {
                    al.notify_one();
                }
            }
            // ML.3c: the `$/progress` + `serverStatus` wake forwarders
            // are gone — those events now reach the screen through the
            // `lattice_lsp::modeline` forwarder, which folds them and
            // publishes `ModelineElementUpdate`; the modeline wake below
            // fires `async_landed` for that push.
            // BC.8a: the four `workspace/*/refresh` wake forwarders
            // (`LspInlayHintRefresh` / `LspSemanticTokensRefresh` /
            // `LspDiagnosticRefresh` / `LspCodeLensRefresh`) moved into
            // `lattice_lsp::install(boot)` as `boot.wake_on_event::<E>()`
            // (byte-identical: subscribe-typed + spawn a notify task). The
            // per-type drain channels (host-side `pending_*_refresh_rx`) still
            // do the cache-eviction in `run_tick_pending` — those stay here.
            // ML.3: a pushed modeline-element content update repaints
            // off-keystroke. Same shape as the LSP forwarders above: a
            // dedicated subscription whose only job is to fire
            // `async_landed`; `drain_modeline_element_updates` (its own
            // channel) does the accumulation in `run_tick_pending`.
            let (ml_tx, ml_rx) = mpsc::unbounded_channel::<lattice_mode::ModelineElementUpdate>();
            event_bus.subscribe_typed(ml_tx);
            runtime_handle.spawn(wake_on(ml_rx, async_landed.clone()));
        }

        // AsyncRenderStatePublished: fired by the actor after every
        // publish_render_state triggered by the async_landed arm.
        // Wakes cells_wake so the cells worker reads the freshly-
        // written PaneCellsInputs. Ordering guarantee: the event is
        // published after the ArcSwap store, so cells always sees
        // fresh state. This replaces the racy direct notify_one that
        // previously fired from the async event handlers.
        {
            use tokio::sync::mpsc;
            let (tx, mut rx) =
                mpsc::unbounded_channel::<crate::events::AsyncRenderStatePublished>();
            event_bus.subscribe_typed::<crate::events::AsyncRenderStatePublished>(tx);
            let cw = cells_wake.clone();
            runtime_handle.spawn(async move {
                while rx.recv().await.is_some() {
                    cw.0.notify_one();
                }
            });
        }

        // D.3.a.1 (2026-05-29): bind the diff subsystem to the
        // event bus. The drainer task subscribes to
        // DocumentChanged + DocumentClosed and routes through
        // the per-session debouncer. The guard's `Drop`
        // unsubscribes + aborts the drainer when the editor
        // tears down. `_enter` lets us spawn the drainer task
        // onto `runtime_handle` even though `bind` uses
        // `tokio::spawn`.
        let diff_subsystem: std::sync::Arc<crate::diff::subsystem::DiffSubsystem> =
            std::sync::Arc::default();
        // D.5.a (2026-05-30): the subsystem owns its diff-mode
        // bridge via `Default`. Editor accesses it through
        // `diff_subsystem.mode_bridge()` during the dispatch
        // tail (`apply_pending_diff_mode_changes`); no separate
        // wiring step required.
        let diff_subscription_guard = {
            let _enter = runtime_handle.enter();
            let resolver: std::sync::Arc<dyn crate::diff::subsystem::DocumentBufferResolver> =
                std::sync::Arc::new(crate::diff::subsystem::BufferRegistryDocumentResolver::new(
                    buffers.clone(),
                ));
            diff_subsystem.bind(event_bus.clone(), resolver)
        };
        let diff_forwarders: std::sync::Arc<
            std::sync::Mutex<
                std::collections::HashMap<lattice_core::BufferId, tokio::task::JoinHandle<()>>,
            >,
        > = std::sync::Arc::default();

        // M.7: pre-create shared fold registry so `FoldOverlayServiceImpl`
        // and the `fold_registry:` field both point at the same Arc.
        let fold_registry = std::sync::Arc::new(std::sync::Mutex::new(
            crate::fold_provider::FoldRegistry::with_builtins(),
        ));

        // SN.3b: fold the loaded `snippet.activation` /
        // `snippet.languages` config into the shared policy cell so
        // the `snippet-mode` gate resolves with the user's settings
        // from the first buffer onward. Defaults (`global` / empty)
        // reproduce the pre-SN.3b Global behavior. Re-folded live on
        // `:set` via `apply_option_cascade`.
        {
            let activation = config
                .get_typed::<lattice_snippet::SnippetActivation>()
                .map(|v| *v)
                .unwrap_or_default();
            let languages = config
                .get_typed::<lattice_snippet::SnippetLanguages>()
                .map(|v| (*v).clone())
                .unwrap_or_default();
            snippet_activation_policy.store(std::sync::Arc::new(
                lattice_snippet::fold_activation_policy(activation, &languages),
            ));
        }

        // SN.3c.0: the shared action-handler registry. Created here
        // (not inside the `services:` block below) so the boot walk
        // can register modes' declarative *global* action handlers
        // (`Mode::action_handlers()`) and the resulting app-lifetime
        // tokens land on `Editor.global_action_handler_regs`. The
        // same Arc is registered as a service so per-buffer handlers
        // still register from `on_activate`. See
        // `mode_action_handlers::register_mode_action_handlers`.
        let action_handlers: lattice_mode::ActionHandlerRegistryHandle =
            Arc::new(lattice_mode::ActionHandlerRegistry::new());
        let global_action_handler_regs = crate::mode_action_handlers::register_mode_action_handlers(
            &action_handlers,
            &mode_registry.load(),
            &registry.load(),
        );

        // IDE-protocol I1.1 / BC.3a: the per-tick drain-closure registry is a
        // Phase-A primitive (`tick_callbacks`, created up top, owned by
        // `boot`). A single Arc spans the editor's lifetime; registered as a
        // service so modes add their drain from `on_activate` (e.g. the Claude
        // Code IDE peer's `IdeInbound` drain, I3). The host runs every
        // registered closure once per tick inside `run_tick_pending`
        // (`drain_tick_callbacks`) and applies the returned `Effect`s.

        // T.3/T.4 (theme-system): the builtin element ids, captured from
        // the theme-element registry created earlier (above the picker
        // registry so the T.12a colorscheme picker could capture a
        // clone). The registry is registered into `services` below + held
        // in the `theme_registry` field. See theme-system.md §3.5 / §7.
        let builtin_element_ids =
            lattice_theme::BuiltinElementIds::capture(theme_registry.as_ref());

        // ML.0b-2: one ModelineService instance, shared three ways —
        // registered into `services` (modes reach it via
        // `ctx.service::<ModelineServiceHandle>()`), stashed on
        // `Editor.modeline` (host built-ins write content + the publish
        // path snapshots it), and thus read wait-free by the renderers.
        let modeline_service: lattice_mode::ModelineServiceHandle = std::sync::Arc::default();
        // ML.1a-render: register the host's built-in descriptors
        // (`core.mode` / `core.path` / `core.position` / `core.lang`).
        // Content is resolved per-pane host-side at render time
        // (`crate::modeline::resolve_builtin_content`) so both renderers
        // paint identical content.
        crate::modeline::register_builtin_elements(&modeline_service);
        // ML.3b: the diff subsystem owns its `diff` element descriptor.
        // Content is pushed by the actor's `sync_diff_modeline_element`.
        crate::diff::mode::register_diff_modeline_element(&modeline_service);
        // ML.3c: lattice-lsp owns the `lsp` element. Its forwarder folds
        // `$/progress` + `serverStatus` into the shared `LspProgressStore`
        // and pushes the badge per attached buffer; the host keeps the
        // store handle for `:lsp-progress-cancel`.
        lattice_lsp::modeline::register_lsp_modeline_element(&modeline_service);
        let lsp_progress_store: lattice_lsp::modeline::LspProgressStoreHandle =
            std::sync::Arc::default();
        lattice_lsp::modeline::spawn_modeline_forwarder(
            event_bus.clone(),
            lsp_progress_store.clone(),
            &runtime_handle,
        );

        // ── BC.3a: service registration through `boot` ───────────────────
        // Hoisted out of the former `services: { … }` field in the `Editor`
        // literal so each registration runs through `boot.register_service()`
        // / `boot.services_mut()` (decision 2-b). `freeze_service_registry`
        // hands back the shared `Arc<ServiceRegistry>` the literal seats.
        // Registration order preserved verbatim from the old block.
        // BC.8a: `boot.register_service(lsp.clone())` moved up to Phase A
        // (right after `build_lsp_subsystem`) so `lattice_lsp::install` can read
        // the supervisor handle for `lsp-completion-mode` registration.
        // ML.0b-2: same Arc as `Editor.modeline` below, so modes
        // register/update the instance the renderer snapshots.
        boot.register_service(modeline_service.clone());
        // M.6.cd.1 (2026-07-16): CurrentDirHandle — shared current
        // working directory for mode-owned handlers (e.g. search
        // `gr` refresh) to re-resolve scan roots after `:cd`.
        // Registered as an `Arc<Mutex<Option<PathBuf>>>` under the
        // typed alias so the `ServiceRegistry` Arc/TypeId rule holds.
        boot.register_service(
            std::sync::Arc::<std::sync::Mutex<Option<std::path::PathBuf>>>::new(
                std::sync::Mutex::new(std::env::current_dir().ok()),
            ),
        );
        // T-mode-1 (2026-05-27): TerminalStoreHandle so `TerminalNormalMode`
        // can install / clear the SyntheticDoc on a TerminalBuffer from its
        // lifecycle hooks. Same `BufferRegistry` backs both stores — cheap
        // clone (Arc inside). BC.4: this is a HOST-PUBLISHED primitive (the host
        // `BufferRegistry` exposed under `dyn TerminalStore`), so it stays here
        // — not in `lattice_terminal::install` — sibling to `buffer_store` /
        // `diagnostics`. Terminal owning it would need `TerminalStore` impl'd
        // over `BufferStoreHandle` (a terminal-crate slice).
        let term_store: Arc<dyn lattice_terminal::TerminalStore> = Arc::new(buffers.clone());
        boot.register_service(lattice_terminal::TerminalStoreHandle::new(term_store));
        // BC.3a: the generic buffer-store is a Phase-A handle (`boot` already
        // holds a clone via `BootContext::new`); register the clone for mode
        // lookups via `services().get::<BufferStoreHandle>()`.
        boot.register_service(buffer_store_handle.clone());
        // CB.0 (clipboard.md): default clipboard backing. `FakeClipboard` is
        // the safe default (no OS resource, no display dependency); the TUI
        // peer (CB.2 — `arboard` + OSC52 fallback) and the GPUI peer (CB.4 —
        // gpui-native) override this with a real backend at renderer boot.
        // Registered under `ClipboardHandle` per the ServiceRegistry Arc/TypeId
        // rule so the host register layer (CB.1) AND `terminal-mode` (CB.3)
        // look it up by that exact type.
        let clipboard: lattice_core::ClipboardHandle = Arc::new(lattice_core::FakeClipboard::new());
        boot.register_service(clipboard);
        boot.register_service(lsp_logger.clone());
        // PI.3/PI.4: the (initially empty) plugin-id → metadata map (name +
        // the plugin's own doc). The Phase-8 plugin loader populates it via
        // `Editor::register_plugin`; provenance (`:list-commands`) reads the
        // name, `:describe-plugin` / `:list-plugins` read the full metadata.
        boot.register_service(crate::dispatch::PluginMetaRegistry::default());
        // PL8.B: expose the SAME meta registry as a `PluginMetaSinkHandle` so
        // the plugin loader can write provenance (name/doc) as each plugin loads
        // without naming this host type. `service` returns the Arc just
        // registered; coerce it to the trait object (same instance the host's
        // `register_plugin` / `plugin_meta` read through).
        if let Some(meta) = boot.service::<crate::dispatch::PluginMetaRegistry>() {
            let sink: lattice_mode::PluginMetaSinkHandle = meta;
            boot.register_service::<lattice_mode::PluginMetaSinkHandle>(sink);
        }

        // L4b (lsp-architecture.md §15): the diagnostics-query service
        // (`lsp-diagnostics-mode`'s `gl` handler + claude-code's read tools read
        // it) is registered in Phase A above, so a subsystem `install(boot)` can
        // reach it via `boot.service::<DiagnosticsQueryHandle>()`. Not re-registered here.
        // (BC.3b: the claude-code `install_services` read/write wiring moved into
        // `lattice_claude_code::install` in the Phase-B list above.)
        // BC.7 (2026-06-24): the `MultibufferRegistryHandle` + the
        // project-search services moved into `lattice_multibuffer::install(boot)`
        // (registered via `boot.register_service` / `boot.services_mut()`
        // there). The host still reads the registry handle back via
        // `services.get::<MultibufferRegistryHandle>()` at dispatch time
        // (`resolve_narrow_target`).
        // M.4 (2026-06-01): expose the EventBus as a generic Phase-A primitive —
        // multibuffer views subscribe to source events + publish typed events
        // (`MultibufferSourceClosed`, `MultibufferHeaderlineChanged`) via
        // `services().get::<EventBus>()`, and other subsystems consume it too.
        // Host-owned (not multibuffer-owned), so it stays here.
        boot.register_service(event_bus.clone());
        // M.10.1.b (2026-06-03): action-handler registry — mode-contributed
        // chord/ex-command handler closures. Modes register from `on_activate`
        // via `ctx.service::<ActionHandlerRegistryHandle>()`; one Arc serves
        // every activation. SN.3c.0: reuse the Arc created above (after the
        // boot action-handler walk). See `mode-architecture.md` §5.3 +
        // `feedback_mode_owns_its_surface`.
        boot.register_service::<lattice_mode::ActionHandlerRegistryHandle>(action_handlers.clone());
        // IDE-protocol I1.1: per-tick drain-closure registry. Read each tick by
        // `Editor::drain_tick_callbacks`; written by modes' `on_activate` via
        // `ctx.service::<TickCallbackRegistryHandle>()`.
        boot.register_service::<lattice_mode::TickCallbackRegistryHandle>(tick_callbacks.clone());
        // M.10.3 (2026-06-03): expose the CommandRegistry as a service so mode
        // handlers (registered via M.10.1.b ActionHandlerRegistry) can look up
        // CommandIds by action name at `on_activate` time — e.g.
        // `cmd_registry.id_by_name("action:search-refresh")` — without
        // depending on host-internal types. Same `Arc<X>` alias pattern.
        boot.register_service::<lattice_grammar::CommandRegistryHandle>(registry.clone());
        // PL8.B (drain_mode): expose the keymap handle so `lattice-plugin-loader`
        // can pass it to `spawn_mode_plugin` — a mode plugin's per-mode
        // `MinorMode` keymap bindings land in it. `KeymapHandle` is Arc-backed +
        // `Clone` with interior-mutable (mutex+ArcSwap) writes, so the loader's
        // captured clone shares the one live registry (bindings are immediately
        // visible). Registered as `KeymapHandle` (already a shareable handle — no
        // `Arc<X>` wrapper needed); the loader looks it up under the same type.
        boot.register_service::<crate::keymap_registry::KeymapHandle>(keymap_handle.clone());

        // Phase 8 (PL8.A/B): the plugin loader. Stands the wasmtime runtime up,
        // registers the `PluginLoaderHandle` service, and spawns on-disk
        // discovery off the boot thread. First subsystem whose install pulls the
        // plugin *runtime* into the editor (the host was wasmtime-free through
        // Phase 7 — only the `lattice-plugin-api` catalog). **Seated last among
        // its service dependencies**, which is why it lives here and not in the
        // Phase-B install list: the drains capture the picker (~L893) + meta sink
        // + mode registry + config + the `CommandRegistryHandle` (drain_grammar,
        // just above) + the `KeymapHandle` (drain_mode, just above), so it MUST
        // follow every one of those `register_service` calls. A boot-ordering
        // regression (moving this earlier) would silently degrade grammar/mode
        // plugin loading to a `NotWired` skip — the boot pin
        // `plugin_loader_captures_every_drain_service` guards against exactly
        // that. A host that fails to build degrades to no-plugin-support, logged,
        // never a failed boot — see `lattice_plugin_loader::install`.
        lattice_plugin_loader::install(&mut boot);
        // (BC.3b: the `ClaudeCodeServerHandle` service is registered by
        // `lattice_claude_code::install` in the Phase-B list above.)
        // M.7: expose the fold-overlay service so `MultibufferMode::on_activate`
        // can register `ExcerptFoldProvider` without depending on
        // `lattice-host`. Same Arc as `fold_registry` above.
        let fold_svc: lattice_core::FoldOverlayServiceHandle = Arc::new(
            crate::fold_provider::FoldOverlayServiceImpl::new(fold_registry.clone()),
        );
        boot.register_service::<lattice_core::FoldOverlayServiceHandle>(fold_svc);
        // DX.3-C7 (2026-06-24): publish the diff subsystem so
        // `DiffMode::on_activate` can look up a buffer's session and
        // register its `HunkFoldSource` via the fold service above
        // (mode-owned hunk folds, mirroring multibuffer's
        // `MultibufferRegistryHandle`). Same `Arc` as the
        // `Editor.diff_subsystem` field.
        boot.register_service::<crate::diff::subsystem::DiffSubsystemHandle>(
            diff_subsystem.clone(),
        );
        // SN.2: register the live snippet session so `SnippetActiveMode`'s
        // `<Tab>`/`<S-Tab>` handlers can reach it from `on_activate`. Same Arc
        // as the `Editor.snippet_session` field (set below).
        boot.register_service::<lattice_snippet::SnippetSessionHandle>(snippet_session.clone());
        // T.3/T.4 (theme-system): register the theme-element registry (created
        // above) so modes + renderers look it up via
        // `services().get::<ThemeRegistryHandle>()`. Register + look up the
        // SAME `Arc<dyn ThemeRegistry>` per the ServiceRegistry Arc/TypeId rule
        // (`feedback_servicesregistry_arc_typeid`). Renderers read the resolved
        // table via the `RenderState` snapshot (T.4); modes intern their own
        // `ElementId`s from `on_activate` (T.7). Moves `theme_registry` (its
        // last use — captured into `builtin_element_ids` above).
        boot.register_service::<lattice_theme::ThemeRegistryHandle>(theme_registry);
        // MH.A3 / DB.5: `Arc<ConfigRegistry>` is registered as a Phase-A
        // service near `config`'s construction (above, next to `event_bus`)
        // rather than here — see the DB.5 hoist note there for why a
        // Phase-B `install(&mut boot)` (e.g. `lattice_dashboard::install`)
        // needs it to already be registered.
        // BC.3a: freeze the service registry into its shared `Arc` (last, after
        // the full services block). The `Editor` literal seats it below.
        let services = boot.freeze_service_registry();
        // BC.3b: consume `boot`, taking the boot-lifetime tick-callback
        // registration tokens (e.g. the claude-code inbound drain wired in the
        // Phase-B install list). They move onto the `Editor` so the drains live
        // for the program rather than being dropped when `boot` drops here.
        let boot_tick_registrations = boot.into_registrations();

        let mut editor = Editor {
            messages: messages_ring.clone(),
            pending_message_event_rx: Some(message_event_rx),
            option_change_rx: Some(option_change_rx),
            lang_registry: lang_registry.clone(),
            syntax,
            last_parsed_text_version,
            picker_registry: picker_registry.clone(),
            // PL8.E: hand the decoration-producer registry to the editor so the
            // per-tick refresh (`maybe_refresh_wasm_decorations`) drives loaded
            // producers off the render path. The loader registers into the same
            // handle via the service registered above.
            wasm_decorations: crate::wasm_decorations::WasmDecorationState::with_registry(
                decoration_registry.clone(),
            ),
            picker_mru,
            picker_mru_path,
            // MH.A3 (2026-06-19): clone so the `services:` block below
            // can register the same `Arc<ConfigRegistry>` (read by
            // multibuffer's `create_multibuffer_view` for the
            // `ui.nerd_fonts` icon-palette default). Field initializers
            // run top-down; `config,` would otherwise move the binding
            // before `services:` evaluates.
            config: config.clone(),
            // K.2.4 (2026-06-01): clone the Arc so the
            // `keymap: { ... }` block below can still borrow
            // `mode_registry` to run the mode-keymap
            // translation pass. Field initializers run top-down
            // in source order; the original binding would
            // otherwise be moved into the struct before
            // `keymap:` evaluates.
            mode_registry: mode_registry.clone(),
            // ML.0b-2: the shared modeline service (same Arc registered
            // into `services` below).
            modeline: modeline_service.clone(),
            // BC.3a: the frozen service registry (registration hoisted above,
            // through `boot.register_service` / `boot.services_mut`).
            services,
            // BC.3b: boot-lifetime tick-callback drain tokens (claude-code's
            // inbound write-bus drain today). Held for the editor's lifetime.
            _boot_tick_registrations: boot_tick_registrations,
            // T.4 (theme-system): builtin ids captured (above) from the
            // theme registry, which is registered into `services` for
            // the renderer snapshot + mode lookups.
            builtin_element_ids,
            // Perf plan B.4: wrap the seeded HashMap so the
            // buffer_locals sub-state cache can detect when no
            // mutation has fired between publishes.
            buffer_locals: crate::versioned::Versioned::new(buffer_locals),
            help_topics,
            // K.2.4.A.0.3 (2026-06-02): clone the Arc so the
            // `keymap: { ... }` block below can still borrow
            // `registry` to pass as the `&CommandRegistry`
            // argument that K.2.4.A.0.3 added to
            // `translate_mode_keymaps`. Field initializers run
            // top-down in source order; the original Arc would
            // otherwise be moved into the struct before
            // `keymap:` evaluates. Same shape as the
            // `mode_registry: mode_registry.clone()` cell
            // above.
            registry: registry.clone(),
            event_bus: event_bus.clone(),
            builtins,
            action_ids,
            keymap: {
                // MP.2b: reuse the handle created above so the
                // commands picker's reverse-lookup adapter and the
                // binding registration below share one registry.
                let h = keymap_handle;
                crate::keymap_replace::register_replace_bindings(&h, &action_ids);
                crate::keymap_visual::register_visual_bindings(
                    &h,
                    &builtins,
                    &action_ids,
                    &syntax_textobject_ids,
                    &syntax_motion_ids,
                );
                // SN.3d.2: Select mode's motion/text-object table —
                // duplicated from Visual, kept honest by the parity test
                // in `keymap_select` (select-mode.md §4).
                crate::keymap_select::register_select_bindings(
                    &h,
                    &builtins,
                    &action_ids,
                    &syntax_textobject_ids,
                    &syntax_motion_ids,
                );
                crate::keymap_insert::register_insert_bindings(&h, &action_ids);
                crate::keymap_normal::register_normal_bindings(
                    &h,
                    &builtins,
                    &action_ids,
                    &syntax_textobject_ids,
                    &syntax_motion_ids,
                );
                // N.1.3 (2026-06-10): wire the narrow `zn` operator
                // chord into the universal operator-pending layer.
                // `zn{motion|text-object}` narrows that span; `znn`
                // narrows the current line. The operator SPEC + apply
                // are owned by `lattice-multibuffer::providers::narrow`;
                // only this chord-wiring lives host-side (it needs the
                // resolved `Builtins`).
                //
                // BC.7 (2026-06-24, decision A): the SPEC is registered by
                // `lattice_multibuffer::install(boot)`; the host resolves its
                // `OperatorId` by name here (the K.2.5 motion name-resolution
                // pattern) rather than threading the registration return value.
                // `operator:narrow` is registered above install, so the lookup
                // is infallible at this point.
                let narrow_operator_id = lattice_grammar::registry::OperatorId(
                    registry
                        .load()
                        .id_by_name("operator:narrow")
                        .expect("operator:narrow registered by lattice_multibuffer::install"),
                );
                crate::keymap_normal::register_operator_bindings(
                    &h,
                    &[
                        lattice_protocol::chord::ChordPattern::Literal(
                            lattice_protocol::chord::KeyChord::char('z'),
                        ),
                        lattice_protocol::chord::ChordPattern::Literal(
                            lattice_protocol::chord::KeyChord::char('n'),
                        ),
                    ],
                    narrow_operator_id,
                    lattice_protocol::chord::ChordPattern::Literal(
                        lattice_protocol::chord::KeyChord::char('n'),
                    ),
                    &builtins,
                    &syntax_textobject_ids,
                    &syntax_motion_ids,
                );
                // K.3.2 (2026-06-02): emacs-style <C-h> map at
                // KeymapLayer::Builtin (Normal-mode only) —
                // <C-h><C-h> / <C-h>? open :help-for-help;
                // <C-h>{k,c,o,e,m,b,a,K} route to the
                // respective :describe-* / :apropos / :keymap.
                // Resolves command names against the registry
                // (populated above by ex_commands::populate +
                // actions::populate).
                crate::keymap_help::register_help_prefix_bindings(&h, &registry.load());
                // MO.x (2026-06-24): the diff-mode `do`/`dp` keymap is
                // contributed via `DiffMode::keymap()` and pushed by the
                // K.2.4 `translate_mode_keymaps` pass below (under
                // `MinorMode(diff-mode)`, K.1.c-gated, names resolved against
                // the registry) — the bespoke explicit host push is retired.
                // The mode now owns its binding choice end-to-end (no
                // diff-specific host push remains).
                // emacs-keys (S1): push the `<C-x>` leader layer once.
                // K.1.c's filter gates the chords to buffers where the
                // mode is active. S1b reads the configurable leader prefix
                // + enable flag (`emacs-keys-prefix` / `emacs-keys`) from
                // config so lattice.toml can rebind or disable the tribute;
                // `:set` re-pushes the layer live (see dispatch.rs). `config`
                // is still borrowable here (the field above clones it).
                // Disabled, or a malformed prefix => empty layer (no panic).
                let emacs_keys_enabled = config
                    .get_typed::<lattice_config::core_options::EmacsKeys>()
                    .map(|v| *v)
                    .unwrap_or(true);
                let emacs_keys_prefix = config
                    .get_typed::<lattice_config::core_options::EmacsKeysPrefix>()
                    .map(|v| (*v).clone())
                    .unwrap_or_else(|| "<C-x>".to_string());
                h.push_layer(
                    crate::keymap_registry::PushLayerKind::MinorMode(
                        lattice_mode::EmacsKeysMode::mode_id(),
                    ),
                    "emacs-keys-mode",
                    lattice_mode::emacs_keys_layer_bindings(
                        emacs_keys_enabled,
                        &emacs_keys_prefix,
                        &registry.load(),
                    ),
                );
                // K.2.5 (2026-06-02): explicit push_layer calls
                // for `multibuffer-mode` and
                // `project-search-multibuffer-mode` retired.
                // Their bindings now flow through
                // `MultibufferMode::keymap()` and
                // `ProjectSearchMultibufferMode::keymap()` via
                // the K.2.4 translation pass below — host glue
                // no longer needs to know about them. (Diff
                // mode's bindings still go through the explicit
                // path above; migrating them is a separate
                // MO.x slice tracked under
                // `mode-ownership-cleanup.md`.)
                // K.2.4 (2026-06-01): translate every registered
                // mode's `Mode::keymap()` contribution into a
                // `MinorMode(mode_id)` layer on `h`. Today most
                // modes still return `Keymap::default()` (the empty
                // contribution is skipped); K.2.5 promotes the
                // multibuffer + project-search bindings into their
                // owning mode crates so this pass becomes
                // load-bearing for them, and the explicit
                // `push_layer` calls above for those modes retire
                // in the same slice. The pass is idempotent on
                // `mode_id`: re-pushing replaces the layer rather
                // than minting a sibling (K.1.b).
                crate::keymap_mode_contributions::translate_mode_keymaps(
                    &h,
                    &mode_registry.load(),
                    &registry.load(),
                );
                // MARG.2 (2026-06-03): now that every layer's
                // bindings are registered, the reverse cache
                // reflects the full Normal-mode keymap. Build
                // the keybinding annotator against the
                // registry's reverse-cache adapter and
                // register it into the completion pipeline so
                // command-completion candidates surface their
                // chord. Subsequent `:map` / `:unmap` rebuild
                // the cache automatically (see the
                // `rebuild_reverse_cache` call in every
                // KeymapRegistry mutation site); the
                // annotator references the cache through an
                // `Arc<ArcSwap<_>>` so it always reads the
                // current snapshot. See
                // `docs/dev/architecture/marginalia.md` §6.
                let kb_anno = lattice_completion::KeybindingAnnotator::new(
                    crate::keymap_registry::KeymapReverseLookupHandle::new(&h, registry.clone()),
                );
                let kb_anno_id = completion_registry.register_annotator(
                    "anno:keybinding",
                    "Append the chord(s) bound to a command in Normal mode (e.g. `<C-w>v` next to `:split-pane-vertical`).",
                    kb_anno,
                );
                // 2026-06-03 placement fix: insert at position
                // 0 so the keybinding renders LEFTMOST in the
                // annotation column — immediately to the right
                // of the command name where the user's eye
                // already is. The previous `.push(...)` placed
                // it last, after kind + doc snippet; user
                // reported it was "too far away to notice."
                // Column alignment for the kind / doc labels
                // is sacrificed (keybinding is inherently
                // variable-width) but proximity to the command
                // is the higher-value scan affordance.
                completion_registry.default_annotators.insert(0, kb_anno_id);
                h
            },
            completion_registry,
            completion_state: None,
            // Perf plan B.4: wrap in `Versioned` so per-publish
            // identity tracking starts at version 0; subsequent
            // pane-tree mutations bump it via `DerefMut`.
            pane_tree: crate::versioned::Versioned::new(pane_tree),
            // Issue #29 (2026-05-22): boot with one tab. The
            // slot's `panes` is a default placeholder; the real
            // pane tree above is live on `editor.pane_tree`.
            // `TabSlot::new` mints a fresh TabId.
            // Perf plan B.4.b: wrap in `Versioned` so version
            // starts at 0; subsequent tab list mutations bump via
            // DerefMut autoref.
            tabs: crate::versioned::Versioned::new(vec![lattice_core::ui::tab::TabSlot::new()]),
            active_tab: 0,
            document,
            snapshot_cache,
            document_buffer_id,
            buffers,
            active_buffer: BufferKind::Document,
            viewport_height: 1,
            lsp,
            lsp_diagnostics,
            lsp_logger,
            // 5.8.AA.o / 5.8.AF.5: lazy-spawn on first
            // `workspace/didChangeWatchedFiles` registration via
            // `Editor::refresh_lsp_file_watcher`. The handle here
            // is the cmd_tx side; the watcher itself + the
            // notify/event loop live on a tokio task on the LSP
            // runtime so nothing runs on the renderer's per-tick.
            lsp_watcher: None,
            lsp_watcher_subscriptions: std::collections::HashMap::new(),
            lsp_watcher_watched_roots: std::collections::HashSet::new(),
            // Phase 5.8.AF.5 / Slice 3a: empty `RenderState` so
            // the first dispatch publication has somewhere to
            // store into. Renderers reading before the first
            // dispatch (e.g. the initial paint at boot) see the
            // default empty sub-states, which is correct -- no
            // diagnostics, no popups, no pickers exist yet.
            //
            // X2.4: the same Arc is now also held by the
            // highlights worker (spawned above) so its
            // reads of `syntax` inputs see the SAME atomic snapshots
            // the renderer reads.
            render_state: render_state_arc,
            // X2.4 (gut + rename B4.2): same-Arc-identity values
            // constructed above and shared with the overlay worker.
            // Overrides the `..Editor::default()` tail (which would
            // otherwise construct fresh, unshared cells).
            overlay_wake,
            syntax_static_overlay_quads_cell,
            paint_request,
            // Slice B.1: same Notify the initial document's reparse
            // worker fires on publish (handed in above); the actor
            // loop awaits it to re-publish on idle reparse completion.
            async_landed,
            // S2.2 (2026-05-26): same-Arc-identity values for the
            // cell-builder worker. Overrides `..Editor::default()`
            // so the matrix the worker `.store()`s into is the
            // same one `render_state.cells.matrix` points at.
            cells_wake,
            cells_matrix_cell,
            cells_matrices,
            // B2.1 (2026-06-04): same-Arc-identity values for the
            // per-line display cache; active doc seeded above.
            display_matrix_cell,
            display_matrices,
            // D.0a.1 (2026-05-29): the worker's three Arcs +
            // wake match the cells pattern — same identities
            // here as the `runtime_handle.spawn(...)` above.
            virtual_rows_wake,
            virtual_rows_matrix_cell,
            // D.4.d.2.0 (2026-05-29): same-Arc-identity
            // seeding for the active doc's virtual-rows
            // matrix; subsequent buffers lazy-insert via
            // `Editor::virtual_rows_matrix_for`.
            virtual_rows_matrices,
            virtual_row_providers,
            // D.3.a.1 (2026-05-29): diff subsystem + its bus
            // subscription guard + the per-session wake
            // forwarder map. `:diff` mutates the map at slice
            // mount; `:diffoff` aborts a forwarder and clears
            // its entry.
            diff_subsystem,
            diff_subscription_guard: Some(diff_subscription_guard),
            diff_forwarders,
            lsp_log_event_rx: Some(lsp_log_event_rx),
            lsp_progress_store: lsp_progress_store.clone(),
            modeline_update_rx: Some(modeline_update_rx),
            pending_apply_edit_rx: Some(lsp_apply_edit_rx),
            // I4: seat the host-drained programmatic-diff receiver + its
            // accept-path map (the sender was registered as a service above).
            pending_programmatic_diff_rx: Some(programmatic_diff_rx),
            programmatic_diff_accept_paths: std::collections::HashMap::new(),
            programmatic_diff_panes: std::collections::HashMap::new(),
            // D-fix.5: empty until the first diff-fold refresh observes a
            // session's published revision.
            diff_fold_seen_revisions: std::collections::HashMap::new(),
            // BC.8b: `pending_configuration_rx` removed (the generic inbound
            // drain replaces the host receiver). The SHARED config tree is
            // seated below (overriding `..Editor::default()`'s fresh Arc) so the
            // mode-owned handler + the editor observe one tree.
            lsp_config_tree,
            // BC.8c: `pending_show_document_rx` removed — the generic inbound
            // drain (mode-owned handler → host-applied open effects) replaces
            // the host receiver field.
            pending_show_message_request_rx: Some(lsp_show_message_request_rx),
            pending_lsp_detach_rx: Some(lsp_detach_rx),
            pending_mode_lifecycle_rx: Some(mode_lifecycle_rx),
            pending_major_entered_rx: Some(major_entered_rx),
            pending_mode_enablement_rx: Some(mode_enablement_rx),
            pending_inlay_hint_refresh_rx: Some(lsp_inlay_refresh_rx),
            inlay_refresh_pending: std::collections::HashSet::new(),
            semantic_tokens_refresh_pending: std::collections::HashSet::new(),
            pending_semantic_tokens_refresh_rx: Some(lsp_semantic_tokens_refresh_rx),
            pending_code_lens_refresh_rx: Some(lsp_code_lens_refresh_rx),
            pending_diagnostic_refresh_rx: Some(lsp_diagnostic_refresh_rx),
            popup_back_stack: Vec::new(),
            insert_completion: None,
            snippet_registry: snippet_registry_handle,
            snippet_activation_policy,
            global_action_handler_regs,
            insert_completion_snippet_meta: Vec::new(),
            completion_accept_freq: HashMap::new(),
            pending_config_structural_sections: std::collections::BTreeMap::new(),
            per_language_completion: lattice_completion::per_language_defaults(),
            completion_in_path_context: false,
            // Generic session-backed-minor registration (composition
            // root): the host reconciles `active-snippet-mode` from
            // the shared `SnippetSession` predicate each overlay-sync
            // (`Editor::sync_keymap_overlays`) instead of a
            // snippet-specific block. `feedback_mode_owns_its_surface`:
            // the "when is my mode active?" policy lives in
            // `lattice-snippet` (`snippet_active_predicate`); the host
            // runs a generic loop. Clone the handle so
            // `snippet_session` still moves into its own field below.
            session_backed_minors: vec![crate::editor::SessionBackedMinor {
                active: lattice_snippet::snippet_active_predicate(snippet_session.clone()),
                mode_id: lattice_snippet::modes::SnippetActiveMode::mode_id(),
            }],
            snippet_session,
            // Default user snippet dir: `~/.config/lattice/snippets`
            // (the same XDG config root as `lattice.toml`, via
            // `lattice_config::config_home` — honours `$XDG_CONFIG_HOME`
            // and reads `~/.config` on macOS, NOT the platform-native
            // dir, so config + snippets always live together).
            // `:reload-snippets` merges any `<language>.json` packs here
            // on top of the embedded built-ins. Absent dir → skipped
            // gracefully by the reload path (not an error — the user just
            // hasn't added any packs).
            snippet_dirs: lattice_config::config_home()
                .map(|d| d.join("lattice").join("snippets"))
                .into_iter()
                .collect(),
            // M.7: use the pre-created Arc so the `services:` block
            // and `fold_registry` field share identity.
            fold_registry,
            ..Editor::default()
        };
        // 2026-05-26: register the built-in invocation runners
        // under the mode-ids each owning [`lattice_mode::Mode`]
        // exposes via [`lattice_mode::Mode::invocation_runner`].
        // `run_invocation` resolves the runner by walking the
        // active modes on the active pane (minors first, then
        // major) and looking the first match up here. Plugin-
        // installed modes (post Phase 7) reuse
        // [`Editor::register_invocation_runner`] for the same
        // effect.
        editor.register_invocation_runner(
            lattice_mode::HelpMode::mode_id(),
            Editor::run_help_invocation,
        );
        editor.register_invocation_runner(
            lattice_oil::OilMode::mode_id(),
            Editor::run_oil_invocation,
        );
        editor.register_invocation_runner(
            lattice_file_tree::FileTreeMode::mode_id(),
            Editor::run_file_tree_invocation,
        );
        editor.register_invocation_runner(
            lattice_terminal::TerminalMode::mode_id(),
            Editor::run_terminal_invocation,
        );
        // AU‑3 gap fix: the AI conversation buffer is read-only above an
        // editable prompt tail. Its runner gates vim operators (`x` / `dd`)
        // to the tail so they can't mutate the frozen transcript — the
        // editable-tail read-only gate otherwise only covered the
        // `apply_edit_blocking` char path, not the operator path.
        editor.register_invocation_runner(
            lattice_ai::acp::conversation_mode::AiConversationMode::mode_id(),
            Editor::run_editable_tail_invocation,
        );
        editor
    }
}
