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

        // M.2.b.2 (2026-06-01): build the multibuffer registry
        // handle before the mode registry so
        // `register_multibuffer_modes` can capture a clone for
        // its `DocumentClosed` cleanup subscriber AND the
        // services block below can register the same Arc'd
        // handle for provider lookups.
        let multibuffer_registry_handle: lattice_multibuffer::MultibufferRegistryHandle =
            lattice_multibuffer::InMemoryMultibufferRegistry::handle();

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
            // Issue #40 / Terminal-mode T1: register the
            // `terminal-mode` major so option contributions
            // (ReadOnly + NoFile) apply to Terminal buffers.
            lattice_terminal::register_terminal_modes(&mut mr);
            crate::modes::register_buffer_kind_modes(&mut mr);
            // M.2.b.2 (2026-06-01): register `multibuffer-mode`
            // + wire its `DocumentClosed` cleanup subscriber. The
            // mode is H.2 kind-bound to `BufferKind::Multibuffer`.
            lattice_multibuffer::register_multibuffer_modes(
                &mut mr,
                &event_bus,
                multibuffer_registry_handle.clone(),
            );
            // M.6 (2026-06-01): register the project-search
            // provider-minor mode. The service handle is
            // registered separately in the ServiceRegistry block
            // below.
            #[cfg(feature = "search")]
            lattice_multibuffer::providers::search::register_project_search_mode(&mut mr);
            // D.5.a (2026-05-30): `diff-mode` minor — marker bit
            // consulted by K.1.c per-keystroke lookup so D.5.b/c
            // `do`/`dp` chords gate on per-buffer diff
            // participation.
            crate::diff::mode::register_diff_modes(&mut mr);
            mr
        };
        register_mode_toggle_commands(&mut registry, &mode_registry);
        let mode_registry = Arc::new(mode_registry);

        // Slice 8.i action ids: each `CommandKind::Action` entry
        // returns `Effect::AppAction(AppEffect::Foo)`; per-mode
        // keymap modules consume the resulting `ActionIds` to
        // build typed `CommandInvocation`s for chord bindings.
        let action_ids = crate::actions::populate(&mut registry, &builtins);

        // M.2.b.3 (2026-06-01): register multibuffer excerpt-jump
        // motions (`]e` / `[e` / `]E` / `[E`) against the command
        // registry. Handlers capture the multibuffer registry
        // handle so they reach the typed view by buffer id at
        // dispatch time.
        //
        // K.2.5 (2026-06-02): the returned `MultibufferMotionIds`
        // is no longer consumed here — the multibuffer-mode
        // keymap now references the motions by canonical name
        // (`multibuffer.next-excerpt-start` etc.) via
        // `MultibufferMode::keymap()`, resolved at host
        // translation time. The registration side-effect (motion
        // names in `CommandRegistry`) is what keeps the keymap's
        // name lookup successful.
        let _ = lattice_multibuffer::register_multibuffer_motions(
            &mut registry,
            multibuffer_registry_handle.clone(),
        );

        // K.2.5 (2026-06-02): ex-commands moved to
        // lattice-multibuffer in the migration that retires the
        // host-side `multibuffer_keymap.rs` glue. Behaviour
        // preserved verbatim; the new home sits next to the
        // modes that use them.
        lattice_multibuffer::register_multibuffer_ex_commands(&mut registry);
        #[cfg(feature = "search")]
        lattice_multibuffer::providers::search::register_search_ex_command(&mut registry);

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
            // Populated by the renderer's per-frame layout pass
            // (Issue #25, 2026-05-22). Zero at boot is safe — the
            // first frame's `set_viewport_*` calls update before
            // any motion / ensure-visible reads.
            viewport_height: 0,
            viewport_width: 0,
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

        // B'.3: keep a clone of `buffers` outside the struct
        // initialiser so the service-registry block below can
        // also hold one (BufferRegistry is `Clone` via Arc).
        let buffers_for_services = buffers.clone();

        // Phase 5.8.AF.5 / Slice X2.4: instantiate the highlights
        // worker's shared cells BEFORE the Editor literal so we
        // can hand the worker its own clones at spawn time. The
        // Editor literal below assigns these into the struct
        // fields explicitly (overriding the `..Editor::default()`
        // tail) so all three holders (Editor, RenderState, worker)
        // share the SAME Arc identities — the worker's writes
        // into `spans_cell` are observable through every
        // `render_state.load_full().syntax.visible_spans.load()`.
        let highlight_wake = crate::editor::HighlightWake::default();
        let syntax_visible_spans_cell: std::sync::Arc<
            arc_swap::ArcSwap<crate::render_state::VisibleSpans>,
        > = std::sync::Arc::default();
        // Perf plan A.2 slice A.2a: parallel pre-paint cell created
        // here so the worker (spawned below), the Editor field, and
        // the `SyntaxRenderState.visible_rows` clone on every
        // `publish_render_state` all share the SAME `Arc` identity.
        // Without this shared identity the worker's `.store()`
        // would not be observable through `RenderState.load_full()`
        // after later publishes.
        let syntax_visible_rows_cell: std::sync::Arc<
            arc_swap::ArcSwap<crate::render_state::VisibleRows>,
        > = std::sync::Arc::default();
        // Perf plan B.2 slice B.2.a: parallel cell carrying the
        // worker's per-row pre-bucketed static-overlay quads
        // (doc_highlight / all_matches / substitute). Same
        // same-Arc-identity construction as
        // `syntax_visible_rows_cell` so the worker's `.store()`
        // is observable through `RenderState.load_full()` across
        // later publishes.
        let syntax_static_overlay_quads_cell: std::sync::Arc<
            arc_swap::ArcSwap<crate::render_state::StaticOverlayQuads>,
        > = std::sync::Arc::default();
        let render_state_arc: std::sync::Arc<arc_swap::ArcSwap<crate::render_state::RenderState>> =
            std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
                crate::render_state::RenderState::default(),
            ));
        // X1b: paint-request signal. Created here so the worker
        // (spawned below) and the Editor (constructed below) hold
        // the same `Arc<Notify>`. The renderer peer subscribes to
        // `editor.paint_request` and translates wakes to its own
        // redraw mechanism (TUI: free via 100ms event poll; GPUI:
        // foreground-executor future that calls `cx.notify()`).
        let paint_request: std::sync::Arc<tokio::sync::Notify> = std::sync::Arc::default();
        runtime_handle.spawn(crate::highlights_worker::run(
            render_state_arc.clone(),
            highlight_wake.clone(),
            syntax_visible_spans_cell.clone(),
            syntax_visible_rows_cell.clone(),
            syntax_static_overlay_quads_cell.clone(),
            paint_request.clone(),
        ));

        // S2.2 (2026-05-26): cell-builder worker. Same same-Arc-
        // identity pattern as the highlights worker — `cells_wake`
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
        let virtual_row_providers: std::sync::Arc<
            crate::virtual_rows_worker::VirtualRowProviderRegistry,
        > = std::sync::Arc::default();
        // D.4.d.2.1.c (2026-05-30): `run` no longer takes the
        // `virtual_rows_matrix_cell` directly — the worker
        // writes per-pane via `pane.virtual_rows_matrix`
        // sourced from `Editor::virtual_rows_matrices` at
        // publish (D.4.d.2.1.b). The active-pane Arc-identity
        // invariant (D.4.d.2.0 boot seed) means worker writes
        // for the active pane still land on
        // `virtual_rows_matrix_cell`, preserving the existing
        // renderer read path through
        // `RenderState.virtual_rows.matrix` until D.4.d.2.1.d
        // swaps in a per-pane lookup.
        runtime_handle.spawn(crate::virtual_rows_worker::run(
            render_state_arc.clone(),
            virtual_rows_wake.clone(),
            virtual_row_providers.clone(),
            paint_request.clone(),
        ));

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
            let resolver: std::sync::Arc<
                dyn crate::diff::subsystem::DocumentBufferResolver,
            > = std::sync::Arc::new(
                crate::diff::subsystem::BufferRegistryDocumentResolver::new(
                    buffers.clone(),
                ),
            );
            diff_subsystem.bind(event_bus.clone(), resolver)
        };
        let diff_forwarders: std::sync::Arc<
            std::sync::Mutex<
                std::collections::HashMap<
                    lattice_core::BufferId,
                    tokio::task::JoinHandle<()>,
                >,
            >,
        > = std::sync::Arc::default();

        let mut editor = Editor {
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
            // K.2.4 (2026-06-01): clone the Arc so the
            // `keymap: { ... }` block below can still borrow
            // `mode_registry` to run the mode-keymap
            // translation pass. Field initializers run top-down
            // in source order; the original binding would
            // otherwise be moved into the struct before
            // `keymap:` evaluates.
            mode_registry: mode_registry.clone(),
            services: {
                let mut s = ServiceRegistry::new();
                s.register(lsp.clone());
                // T-mode-1 (2026-05-27): TerminalStoreHandle so
                // `TerminalNormalMode` can install / clear the
                // SyntheticDoc on a TerminalBuffer from its
                // lifecycle hooks. Same `BufferRegistry` backs
                // both stores — cheap clone (Arc inside).
                let term_store: Arc<dyn lattice_terminal::TerminalStore> =
                    Arc::new(buffers_for_services.clone());
                s.register(lattice_terminal::TerminalStoreHandle::new(term_store));
                let store: Arc<dyn lattice_mode::BufferStore> = Arc::new(buffers_for_services);
                s.register(lattice_mode::BufferStoreHandle::new(store));
                s.register(lsp_logger.clone());
                // M.2.b.2 (2026-06-01): expose the typed
                // multibuffer-handle lookup so providers
                // (`create_multibuffer_view`, future M.6
                // `:search` minor) reach it via
                // `services().get::<MultibufferRegistryHandle>()`.
                s.register(multibuffer_registry_handle.clone());
                // M.4 (2026-06-01): expose the EventBus so
                // multibuffer views (and future provider
                // triggers) can subscribe to source events +
                // publish typed events
                // (`MultibufferSourceClosed`,
                // `MultibufferHeaderlineChanged`) via
                // `services().get::<EventBus>()`.
                s.register(event_bus.clone());
                // M.6 (2026-06-01): register the project-search
                // service handle so `project_search` triggers
                // can look it up.
                #[cfg(feature = "search")]
                lattice_multibuffer::providers::search::register_project_search_service(&mut s);
                Arc::new(s)
            },
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
                let h = crate::keymap_registry::KeymapHandle::new();
                crate::keymap_replace::register_replace_bindings(&h, &action_ids);
                crate::keymap_visual::register_visual_bindings(&h, &builtins, &action_ids);
                crate::keymap_insert::register_insert_bindings(&h, &action_ids);
                crate::keymap_normal::register_normal_bindings(&h, &builtins, &action_ids);
                // D.5.b (2026-05-30): push the diff-mode minor
                // keymap layer once. K.1.c per-keystroke filter
                // gates the chord so it only fires on buffers
                // where `diff-mode` is in `ActiveModes.minors()`.
                // No matching push/pop on activation needed —
                // the layer stays for the editor's lifetime and
                // the filter takes the per-buffer responsibility.
                h.push_layer(
                    crate::keymap_registry::PushLayerKind::MinorMode(
                        crate::diff::mode::DiffMode::mode_id(),
                    ),
                    "diff-mode",
                    crate::diff::mode::diff_mode_layer_bindings(&action_ids),
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
                    &mode_registry,
                    &registry,
                );
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
            // X2.4: same-Arc-identity values constructed above and
            // shared with the highlights worker. Overrides the
            // `..Editor::default()` tail (which would otherwise
            // construct fresh, unshared cells).
            highlight_wake,
            syntax_visible_spans_cell,
            syntax_visible_rows_cell,
            syntax_static_overlay_quads_cell,
            paint_request,
            // S2.2 (2026-05-26): same-Arc-identity values for the
            // cell-builder worker. Overrides `..Editor::default()`
            // so the matrix the worker `.store()`s into is the
            // same one `render_state.cells.matrix` points at.
            cells_wake,
            cells_matrix_cell,
            cells_matrices,
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
        editor
    }
}
