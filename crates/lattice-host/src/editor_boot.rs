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
    theme_registry: lattice_theme::ThemeRegistryHandle,
) -> PickerRegistry {
    let mut reg = PickerRegistry::new();
    for generator in
        lattice_picker::picker_sources::first_party_generators(command_registry, config)
    {
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
        // SN.3b: captured out of the registry-build block so boot
        // can fold `snippet.activation` / `snippet.languages` into
        // it (below) and `Editor` can hold the clone the cascade
        // re-folds.
        let snippet_activation_policy;
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
            snippet_activation_policy =
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
            // N.1.1 (2026-06-10): narrow provider-minor mode (marker
            // for narrow views). First-class — no feature gate.
            lattice_multibuffer::providers::narrow::register_narrow_mode(&mut mr);
            // D.5.a (2026-05-30): `diff-mode` minor — marker bit
            // consulted by K.1.c per-keystroke lookup so D.5.b/c
            // `do`/`dp` chords gate on per-buffer diff
            // participation.
            crate::diff::mode::register_diff_modes(&mut mr);
            crate::tutor::register_tutor_modes(&mut mr);
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
        // N.1.1 (2026-06-10): `:narrow` + `:widen`. First-class — no
        // feature gate.
        lattice_multibuffer::providers::narrow::register_narrow_ex_commands(&mut registry);
        // N.1.3 (2026-06-10): register the `zn` narrow operator SPEC
        // (owned by the narrow provider) and capture its OperatorId;
        // the `zn` chord is wired into the universal operator-pending
        // layer below, right after `register_normal_bindings`.
        let narrow_operator_id =
            lattice_multibuffer::providers::narrow::register_narrow_operator(&mut registry);

        // N.1.4c: register the structural (tree-sitter) text objects
        // (`af`/`if`/`ac`/`ic`/`aa`/`ia`/`al`/`il`) -- owned by
        // lattice-syntax -- and capture their ids so the universal
        // operator-pending keymap (`register_normal_bindings` + the `zn`
        // operator below) can bind their chords. Must run while
        // `registry` is still `&mut` (before the Arc freeze below).
        let syntax_textobject_ids = lattice_syntax::register_syntax_text_objects(&mut registry);

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
        // L2: serverStatus readiness drain channel (accumulated in
        // `drain_lsp_server_status`; woken via the L1c forwarder which
        // fires `async_landed`).
        let (lsp_server_status_tx, lsp_server_status_event_rx) =
            tokio::sync::mpsc::unbounded_channel::<lattice_lsp::LspServerStatusChanged>();
        event_bus.subscribe_typed(lsp_server_status_tx);
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
        // T.3/T.4 (theme-system): the theme-element registry, seeded
        // with the default palette + all builtin elements (resolved +
        // ready). Created here (ahead of the struct literal) so the
        // T.12a colorscheme picker source can capture a clone, AND the
        // `services:` block / `builtin_element_ids` capture / the
        // `theme_registry` field all share the one Arc. See
        // theme-system.md §3.5 / §7.
        let theme_registry: lattice_theme::ThemeRegistryHandle =
            Arc::new(lattice_theme::InMemoryThemeRegistry::with_defaults());
        let picker_registry: Arc<PickerRegistry> = Arc::new(built_in_picker_registry(
            registry.clone(),
            config.clone(),
            snippet_registry_handle.clone(),
            theme_registry.clone(),
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
        // Slice B.1: created here (before the syntax handle) so it can
        // be handed to the reparse worker as its `on_publish` callback;
        // it also seats the `Editor::async_landed` field below.
        let async_landed: std::sync::Arc<tokio::sync::Notify> = std::sync::Arc::default();
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
        let render_state_arc: std::sync::Arc<arc_swap::ArcSwap<crate::render_state::RenderState>> =
            std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
                crate::render_state::RenderState::default(),
            ));
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

        // MultibufferExcerptsReady: fired by the project-search
        // forwarder after each batch of excerpts is appended.
        // Only fires async_landed so the actor calls
        // publish_render_state (picking up the new excerpt_syntax
        // entries). The actor then publishes AsyncRenderStatePublished
        // via the event bus; the bridge below wakes cells_wake AFTER
        // the ArcSwap store — no race possible.
        #[cfg(feature = "search")]
        {
            use tokio::sync::mpsc;
            let (tx, mut rx) = mpsc::unbounded_channel::<lattice_multibuffer::providers::search::MultibufferExcerptsReady>();
            event_bus.subscribe_typed::<lattice_multibuffer::providers::search::MultibufferExcerptsReady>(tx);
            let al = async_landed.clone();
            runtime_handle.spawn(async move {
                while rx.recv().await.is_some() {
                    al.notify_one();
                }
            });
        }

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
            let (prog_tx, prog_rx) =
                mpsc::unbounded_channel::<lattice_lsp::LspProgressUpdate>();
            event_bus.subscribe_typed(prog_tx);
            runtime_handle.spawn(wake_on(prog_rx, async_landed.clone()));
            let (inlay_tx, inlay_rx) =
                mpsc::unbounded_channel::<lattice_lsp::LspInlayHintRefresh>();
            event_bus.subscribe_typed(inlay_tx);
            runtime_handle.spawn(wake_on(inlay_rx, async_landed.clone()));
            let (sem_tx, sem_rx) =
                mpsc::unbounded_channel::<lattice_lsp::LspSemanticTokensRefresh>();
            event_bus.subscribe_typed(sem_tx);
            runtime_handle.spawn(wake_on(sem_rx, async_landed.clone()));
            let (diag_tx, diag_rx) =
                mpsc::unbounded_channel::<lattice_lsp::LspDiagnosticRefresh>();
            event_bus.subscribe_typed(diag_tx);
            runtime_handle.spawn(wake_on(diag_rx, async_landed.clone()));
            let (lens_tx, lens_rx) =
                mpsc::unbounded_channel::<lattice_lsp::LspCodeLensRefresh>();
            event_bus.subscribe_typed(lens_tx);
            runtime_handle.spawn(wake_on(lens_rx, async_landed.clone()));
            let (status_tx, status_rx) =
                mpsc::unbounded_channel::<lattice_lsp::LspServerStatusChanged>();
            event_bus.subscribe_typed(status_tx);
            runtime_handle.spawn(wake_on(status_rx, async_landed.clone()));
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
        let global_action_handler_regs =
            crate::mode_action_handlers::register_mode_action_handlers(
                &action_handlers,
                &mode_registry,
                &registry,
            );

        // T.3/T.4 (theme-system): the builtin element ids, captured from
        // the theme-element registry created earlier (above the picker
        // registry so the T.12a colorscheme picker could capture a
        // clone). The registry is registered into `services` below + held
        // in the `theme_registry` field. See theme-system.md §3.5 / §7.
        let builtin_element_ids =
            lattice_theme::BuiltinElementIds::capture(theme_registry.as_ref());

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
                // M.10.1.b (2026-06-03): action-handler registry —
                // mode-contributed chord/ex-command handler
                // closures. Modes register from `on_activate` via
                // `ctx.service::<ActionHandlerRegistryHandle>()`;
                // the registry's lifetime spans the editor's
                // lifetime, so a single Arc registered here
                // serves every mode activation. See
                // `mode-architecture.md` §5.3 +
                // `feedback_mode_owns_its_surface`.
                // SN.3c.0: reuse the Arc created above (after the
                // boot action-handler walk); register it as a service
                // so per-buffer handlers still register from
                // `on_activate`.
                s.register::<lattice_mode::ActionHandlerRegistryHandle>(
                    action_handlers.clone(),
                );
                // M.10.3 (2026-06-03): expose the CommandRegistry
                // as a service so mode handlers (registered via
                // M.10.1.b ActionHandlerRegistry) can look up
                // CommandIds by action name at `on_activate`
                // time — e.g.
                // `cmd_registry.id_by_name("action:search-jump-to-source")`
                // — without depending on host-internal types.
                // Same `Arc<X>` alias pattern as
                // ActionHandlerRegistryHandle.
                s.register::<lattice_grammar::CommandRegistryHandle>(registry.clone());
                // M.7: expose the fold-overlay service so
                // `MultibufferMode::on_activate` can register
                // `ExcerptFoldProvider` without depending on
                // `lattice-host`. Same Arc as `fold_registry` above.
                let fold_svc: lattice_core::FoldOverlayServiceHandle = Arc::new(
                    crate::fold_provider::FoldOverlayServiceImpl::new(fold_registry.clone()),
                );
                s.register::<lattice_core::FoldOverlayServiceHandle>(fold_svc);
                // SN.2: register the live snippet session so
                // `SnippetActiveMode`'s `<Tab>`/`<S-Tab>` handlers can
                // reach it from `on_activate`. Same Arc as the
                // `Editor.snippet_session` field (set below).
                s.register::<lattice_snippet::SnippetSessionHandle>(snippet_session.clone());
                // T.3/T.4 (theme-system): register the theme-element
                // registry (created above the struct literal) so modes
                // + renderers look it up via
                // `services().get::<ThemeRegistryHandle>()`. Register +
                // look up the SAME `Arc<dyn ThemeRegistry>` type per
                // the ServiceRegistry Arc/TypeId rule
                // (`feedback_servicesregistry_arc_typeid`). The
                // renderers read the resolved table via the
                // `RenderState` snapshot (T.4); modes intern their
                // own `ElementId`s from `on_activate` (T.7).
                s.register::<lattice_theme::ThemeRegistryHandle>(theme_registry);
                // MH.A3 (2026-06-19): expose the ConfigRegistry so
                // extension-crate code (`create_multibuffer_view`) can
                // read global option defaults — e.g. `ui.nerd_fonts`
                // for the rich excerpt-header icon palette — without
                // depending on `lattice-host`'s typed option decls.
                // Same `Arc<X>` register/lookup pair per the
                // ServiceRegistry Arc/TypeId rule. Read by name
                // (`get_bool_by_name`) so multibuffer needn't import
                // the `UiNerdFonts` decl (which lives in host).
                s.register::<Arc<ConfigRegistry>>(config.clone());
                Arc::new(s)
            },
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
                let h = crate::keymap_registry::KeymapHandle::new();
                crate::keymap_replace::register_replace_bindings(&h, &action_ids);
                crate::keymap_visual::register_visual_bindings(
                    &h,
                    &builtins,
                    &action_ids,
                    &syntax_textobject_ids,
                );
                // SN.3d.2: Select mode's motion/text-object table —
                // duplicated from Visual, kept honest by the parity test
                // in `keymap_select` (select-mode.md §4).
                crate::keymap_select::register_select_bindings(
                    &h,
                    &builtins,
                    &action_ids,
                    &syntax_textobject_ids,
                );
                crate::keymap_insert::register_insert_bindings(&h, &action_ids);
                crate::keymap_normal::register_normal_bindings(
                    &h,
                    &builtins,
                    &action_ids,
                    &syntax_textobject_ids,
                );
                // N.1.3 (2026-06-10): wire the narrow `zn` operator
                // chord into the universal operator-pending layer.
                // `zn{motion|text-object}` narrows that span; `znn`
                // narrows the current line. The operator SPEC + apply
                // are owned by `lattice-multibuffer::providers::narrow`;
                // only this chord-wiring lives host-side (it needs the
                // resolved `Builtins`).
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
                );
                // K.3.2 (2026-06-02): emacs-style <C-h> map at
                // KeymapLayer::Builtin (Normal-mode only) —
                // <C-h><C-h> / <C-h>? open :help-for-help;
                // <C-h>{k,c,o,e,m,b,a,K} route to the
                // respective :describe-* / :apropos / :keymap.
                // Resolves command names against the registry
                // (populated above by ex_commands::populate +
                // actions::populate).
                crate::keymap_help::register_help_prefix_bindings(&h, &registry);
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
            lsp_progress_event_rx: Some(lsp_progress_event_rx),
            lsp_server_status_event_rx: Some(lsp_server_status_event_rx),
            pending_apply_edit_rx: Some(lsp_apply_edit_rx),
            pending_configuration_rx: Some(lsp_configuration_rx),
            pending_show_document_rx: Some(lsp_show_document_rx),
            pending_show_message_request_rx: Some(lsp_show_message_request_rx),
            pending_lsp_detach_rx: Some(lsp_detach_rx),
            pending_mode_lifecycle_rx: Some(mode_lifecycle_rx),
            pending_major_entered_rx: Some(major_entered_rx),
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
            // (the same config root as `lattice.toml`, via
            // `dirs::config_dir`). `:reload-snippets` merges any
            // `<language>.json` packs here on top of the embedded
            // built-ins. Absent dir → skipped gracefully by the
            // reload path (not an error — the user just hasn't
            // added any packs).
            snippet_dirs: dirs::config_dir()
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
        editor
    }
}
