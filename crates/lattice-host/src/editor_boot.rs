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
    ApplyEditBus, ConfigurationBus, DiagnosticsLayer, InboundApplyEdit,
    InboundConfigurationRequest, InboundShowDocument, InboundShowMessageRequest, LspLogger,
    LspSupervisor, LspSupervisorHandle, ShowDocumentBus, ShowMessageRequestBus,
};
use lattice_mode::{ModeRegistry, ServiceRegistry};
use lattice_picker::PickerRegistry;
use lattice_protocol::position::Position;
use lattice_runtime::{EventBus, MessagesRing, spawn_document};
use lattice_snippet::SnippetRegistry;
use lattice_syntax::{Lang, LangRegistry, Syntax, SyntaxHandle};

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
) -> (
    LspSupervisorHandle,
    DiagnosticsLayer,
    LspLogger,
    tokio::sync::mpsc::UnboundedReceiver<InboundApplyEdit>,
    tokio::sync::mpsc::UnboundedReceiver<InboundConfigurationRequest>,
    tokio::sync::mpsc::UnboundedReceiver<InboundShowDocument>,
    tokio::sync::mpsc::UnboundedReceiver<InboundShowMessageRequest>,
) {
    let logger = LspLogger::with_defaults();
    let mut sup = LspSupervisor::new(logger.clone());
    sup.set_configs(lattice_lsp::builtin_servers());
    let diagnostics = sup.diagnostics().clone();
    let (apply_edit_bus, apply_edit_rx) = ApplyEditBus::new();
    sup.set_apply_edit_bus(apply_edit_bus);
    let (configuration_bus, configuration_rx) = ConfigurationBus::new();
    sup.set_configuration_bus(configuration_bus);
    let (show_document_bus, show_document_rx) = ShowDocumentBus::new();
    sup.set_show_document_bus(show_document_bus);
    let (show_message_request_bus, show_message_request_rx) = ShowMessageRequestBus::new();
    sup.set_show_message_request_bus(show_message_request_bus);
    sup.set_event_bus(event_bus.clone());
    let handle = sup.spawn(runtime_handle);
    // M-async.5: LSP attach driver is gone; modes drive
    // `open_buffer` directly via the supervisor handle pulled
    // from `ctx.service::<...>()`. `event_bus` stays bound to
    // the supervisor for the per-actor edit fan-in; the keep
    // is intentional -- no other consumer in this function.
    let _ = &event_bus;
    (
        handle,
        diagnostics,
        logger,
        apply_edit_rx,
        configuration_rx,
        show_document_rx,
        show_message_request_rx,
    )
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
    command_registry: Arc<CommandRegistry>,
    config: Arc<ConfigRegistry>,
    snippet_registry: Arc<ArcSwap<SnippetRegistry>>,
) -> PickerRegistry {
    let mut reg = PickerRegistry::new();
    for generator in
        lattice_picker::picker_sources::first_party_generators(command_registry, config)
    {
        reg.register_generator(generator);
    }
    lattice_snippet::picker_sources::register(&mut reg, snippet_registry);
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
    use lattice_grammar::args::ArgSpec;
    use lattice_grammar::registry::{ExCommandSpec, SurfaceForm};
    use lattice_grammar::{Args, CommandError, Effect};
    let mut names: Vec<String> = mode_registry
        .iter_meta()
        .map(|(id, _kind)| id.to_string())
        .collect();
    // Sort for deterministic registration order (HashMap iteration
    // is hash-randomized; deterministic boot keeps `:describe-*`
    // and tests stable).
    names.sort();
    for name in names {
        let mode_name = name.clone();
        let cmd_name = name.clone();
        cmd_registry.register_ex_command(
            &cmd_name,
            "Toggle the mode on the active buffer (auto-generated; \
             see `:help modes` for the full mode-system overview).",
            ExCommandSpec {
                latency_class: lattice_grammar::command::LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                parse_args: Box::new(|s: &str, _bang: bool| {
                    if s.trim().is_empty() {
                        Ok(Args::None)
                    } else {
                        Err(CommandError::BadArgs(
                            "mode toggle takes no arguments".into(),
                        ))
                    }
                }),
                apply: Box::new(move |_ctx| {
                    Ok(Effect::ToggleMode {
                        mode_name: mode_name.clone(),
                    })
                }),
                args_schema: Vec::<ArgSpec>::new(),
                surface_form: SurfaceForm::Keyword,
            },
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
        let (
            lsp,
            lsp_diagnostics,
            lsp_logger,
            lsp_apply_edit_rx,
            lsp_configuration_rx,
            lsp_show_document_rx,
            lsp_show_message_request_rx,
        ) = build_lsp_subsystem(event_bus.clone(), &runtime_handle);

        let mut registry = CommandRegistry::new();
        let builtins = grammar_builtins_populate(&mut registry);
        // Register the built-in ex-commands as peers of motions /
        // operators / text objects (DESIGN.md §5.2.1). The returned
        // ids aren't held in App state today -- the parser front-
        // end looks them up by name -- but registering them
        // populates the registry so `:`-line parsing can route to
        // them.
        let _ex_builtins = lattice_grammar::ex_commands::populate(&mut registry);

        // CSM.5: shared snippet-registry handle. Built before the
        // mode registry so `register_snippet_modes` can capture a
        // clone of the outer Arc -- the same outer Arc the Editor
        // field below holds. `:reload-snippets` updates the inner
        // via `.store()`; the mode + source see the fresh data on
        // the next produce().
        let snippet_registry_handle: Arc<ArcSwap<SnippetRegistry>> =
            Arc::new(ArcSwap::from_pointee(SnippetRegistry::new()));

        // M.5.1 (mode-architecture §9.6.1): build the mode
        // registry first so we can iterate it and register a
        // `:<mode-name>` toggle ex-command per mode. The mode
        // registry is then wrapped in `Arc`.
        let mode_registry = {
            let mut mr = ModeRegistry::new();
            lattice_mode::register_foundation_modes(&mut mr);
            lattice_syntax::register_language_modes(&mut mr);
            lattice_lsp::modes::register_lsp_log_modes(&mut mr);
            // CSM.8a: lsp-completion-mode needs the supervisor
            // handle (its contributed source captures it).
            lattice_lsp::completion::register_lsp_completion_mode(&mut mr, lsp.clone());
            lattice_oil::register_oil_modes(&mut mr);
            lattice_file_tree::register_file_tree_modes(&mut mr);
            lattice_snippet::register_snippet_modes(&mut mr, snippet_registry_handle.clone());
            crate::modes::register_buffer_kind_modes(&mut mr);
            mr
        };
        register_mode_toggle_commands(&mut registry, &mode_registry);
        let mode_registry = Arc::new(mode_registry);

        // Slice 8.i action ids: each `CommandKind::Action` entry
        // returns `Effect::AppAction(AppEffect::Foo)`; per-mode
        // keymap modules consume the resulting `ActionIds` to
        // build typed `CommandInvocation`s for chord bindings.
        let action_ids = crate::actions::populate(&mut registry, &builtins);

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
        // LSP work-done progress.
        let (lsp_progress_tx, lsp_progress_event_rx) =
            tokio::sync::mpsc::unbounded_channel::<lattice_lsp::LspProgressUpdate>();
        event_bus.subscribe_typed(lsp_progress_tx);
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
        let messages_ring = Arc::new(std::sync::Mutex::new(MessagesRing::default()));
        let _ = lattice_runtime::install_messages_subscriber(
            messages_ring.clone(),
            event_bus.clone(),
            "info",
        );
        // Wire the logger's publisher to the same bus. The
        // closure captures an Arc<EventBus> clone so the
        // logger's lifetime is independent of any single field.
        let bus_for_log = event_bus.clone();
        lsp_logger.set_event_publisher(Arc::new(move |event| {
            bus_for_log.publish_typed(event);
        }));

        // Typed-options registry (DESIGN.md §5.12). Single source
        // of truth for every option's *current value*: each
        // `Option<T>` owns a wait-free `ArcSwap<T>` cell that
        // `:set` parses into, hot-path readers load from, and
        // the (future) customize buffer view edits through.
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
            "gen:modes",
            "Every registered mode (major + minor); used by `:describe-mode <Tab>`.",
            crate::host_generators::ModesGenerator {
                registry: Arc::downgrade(&mode_registry),
            },
        );
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

        // Wrap the command registry in `Arc` early so picker
        // sources that capture it (CommandsSource) can clone
        // here.
        let registry = Arc::new(registry);
        let picker_registry: Arc<PickerRegistry> = Arc::new(built_in_picker_registry(
            registry.clone(),
            config.clone(),
            snippet_registry_handle.clone(),
        ));

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
                    eprintln!(
                        "lattice: discarding corrupt MRU cache at {}: {e}",
                        path.display(),
                    );
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

        // One `LangRegistry` per Editor, shared between the
        // document buffer's `Syntax` and every `HelpBuffer` for
        // `:describe-*` / `:apropos` / `:keymap` (markdown
        // highlighted with fenced-block language injection).
        let lang_registry = LangRegistry::standard().expect("standard lang registry");
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
        let syntax: Option<SyntaxHandle> =
            match Syntax::for_language_with_registry(lang, lang_registry.clone()) {
                Ok(Some(mut s)) => {
                    s.parse_at(&initial_text, initial_text_version);
                    Some(SyntaxHandle::seeded_with_runtime(s, &runtime_handle))
                }
                _ => None,
            };
        let last_parsed_text_version = initial_text_version;

        // Hand the document to the actor (DESIGN.md §5.7).
        // After this call the only way to read or mutate it is
        // through the returned `DocumentHandle`.
        let document = spawn_document(document, registry.clone());
        let snapshot_cache = document.snapshot_cache();
        let document_buffer_id = BufferId::next();
        let initial_pane = PaneState {
            id: PaneId::next(),
            buffer: BufferKind::Document,
            buffer_id: document_buffer_id,
            cursor: Position::ZERO,
            scroll: 0,
        };
        let pane_tree = PaneTree::single(initial_pane);

        // Seed the buffer registry with the initial document.
        // The hot-path `Editor.document` / `Editor.syntax` /
        // `Editor.last_parsed_text_version` mirror what's stored
        // here for the active buffer; switching buffers swaps
        // them.
        let buffers = BufferRegistry::new();
        buffers.insert(BufferEntry {
            id: document_buffer_id,
            flags: BufferFlags::default(),
            data: BufferData::Document(DocumentEntry {
                id: document_buffer_id,
                handle: document.clone(),
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

        // B'.3: keep a clone of `buffers` outside the struct
        // initialiser so the service-registry block below can
        // also hold one (BufferRegistry is `Clone` via Arc).
        let buffers_for_services = buffers.clone();

        Editor {
            messages: messages_ring.clone(),
            pending_message_event_rx: Some(message_event_rx),
            option_change_rx: Some(option_change_rx),
            lang_registry: lang_registry.clone(),
            syntax,
            last_parsed_text_version,
            picker_registry: picker_registry.clone(),
            picker_mru,
            picker_mru_path,
            config,
            mode_registry,
            services: {
                let mut s = ServiceRegistry::new();
                s.register(lsp.clone());
                let store: Arc<dyn lattice_mode::BufferStore> = Arc::new(buffers_for_services);
                s.register(lattice_mode::BufferStoreHandle::new(store));
                s.register(lsp_logger.clone());
                Arc::new(s)
            },
            buffer_locals,
            help_topics,
            registry,
            event_bus: event_bus.clone(),
            builtins,
            action_ids,
            keymap: {
                let h = crate::keymap_registry::KeymapHandle::new();
                crate::keymap_replace::register_replace_bindings(&h, &action_ids);
                crate::keymap_visual::register_visual_bindings(&h, &builtins, &action_ids);
                crate::keymap_insert::register_insert_bindings(&h, &action_ids);
                crate::keymap_normal::register_normal_bindings(&h, &builtins, &action_ids);
                h
            },
            completion_registry,
            completion_state: None,
            pane_tree,
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
            render_state: std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
                crate::render_state::RenderState::default(),
            )),
            lsp_log_event_rx: Some(lsp_log_event_rx),
            lsp_progress_event_rx: Some(lsp_progress_event_rx),
            pending_apply_edit_rx: Some(lsp_apply_edit_rx),
            pending_configuration_rx: Some(lsp_configuration_rx),
            pending_show_document_rx: Some(lsp_show_document_rx),
            pending_show_message_request_rx: Some(lsp_show_message_request_rx),
            pending_lsp_detach_rx: Some(lsp_detach_rx),
            pending_mode_lifecycle_rx: Some(mode_lifecycle_rx),
            pending_inlay_hint_refresh_rx: Some(lsp_inlay_refresh_rx),
            pending_semantic_tokens_refresh_rx: Some(lsp_semantic_tokens_refresh_rx),
            pending_code_lens_refresh_rx: Some(lsp_code_lens_refresh_rx),
            pending_diagnostic_refresh_rx: Some(lsp_diagnostic_refresh_rx),
            popup_back_stack: Vec::new(),
            insert_completion: None,
            snippet_registry: snippet_registry_handle,
            insert_completion_snippet_meta: Vec::new(),
            completion_accept_freq: HashMap::new(),
            pending_config_structural_sections: std::collections::BTreeMap::new(),
            per_language_completion: lattice_completion::per_language_defaults(),
            completion_in_path_context: false,
            active_snippet: None,
            snippet_dirs: Vec::new(),
            ..Editor::default()
        }
    }
}
