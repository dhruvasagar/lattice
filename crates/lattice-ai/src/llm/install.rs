//! Boot wiring for the llm layer: the request registry, the chat registry, the
//! `llm-chat` mode and `:llm`.

use std::sync::Arc;

use lattice_mode::SubsystemBoot;

use super::chat::{LlmChats, LlmChatsHandle, spawn_options_listener, spawn_turn_listener};
use super::config::LlmConfig;
use super::modes::chat::{LlmChatMode, register_actions};
use super::modes::system::LlmSystemMode;
use super::request::{RequestRegistry, RequestRegistryHandle};

/// Register the llm layer's services, mode and command. Look services up as
/// their `*Handle` types (the same type used here -- `Arc<X>` registered under a
/// different `T` would silently miss).
pub fn install(
    boot: &mut impl SubsystemBoot,
    permissions: &crate::permission::PermissionSourcesHandle,
) {
    let registry: RequestRegistryHandle = Arc::new(RequestRegistry::new(
        boot.runtime_handle().clone(),
        Some(boot.event_bus().clone()),
        None,
    ));
    boot.register_service::<RequestRegistryHandle>(registry.clone());
    // LL.14c: the modeline says whether a request is running. The element's
    // descriptor is registered by `llm-mode` on its first activation: the
    // host builds the modeline service after this install runs.
    super::modeline::spawn_forwarder(
        registry.clone(),
        boot.event_bus().clone(),
        boot.runtime_handle(),
    );

    // The user's `[llm]` table, read once here (boot is not the editor thread's
    // hot path). Problems become log lines; a bad provider is skipped.
    let config = LlmConfig::load_user();
    for warning in &config.warnings {
        tracing::warn!(target: "lattice::llm", "{warning}");
    }
    let mcp_configs = config.mcp.clone();

    let chats: LlmChatsHandle = Arc::new(LlmChats::new(
        boot.runtime_handle().clone(),
        registry,
        boot.event_bus().clone(),
        config,
    ));
    // The global registry holds `:set llm.*` and the config file's scalar
    // settings; the bus carries the buffer-local layer (`:setlocal`) from the
    // editor thread to the requests that run off it.
    if let Some(registry) = boot.service::<Arc<lattice_config::ConfigRegistry>>() {
        chats.set_registry((*registry).clone());
    }
    spawn_turn_listener(chats.clone(), boot.event_bus());
    spawn_options_listener(chats.clone(), boot.event_bus());
    boot.register_service::<LlmChatsHandle>(chats.clone());

    // LL.18: a tool that must ask puts its question in its session's store;
    // the shared permission popup finds it there. Weak: no cycle through boot.
    {
        let chats = Arc::downgrade(&chats);
        permissions.add(Arc::new(move || {
            chats.upgrade().map(|c| c.stores()).unwrap_or_default()
        }));
    }
    // `chats` reads context from the host's buffers when a request is sent.
    chats.set_store(boot.buffer_store().clone());
    // `llm.exclude` sees through composed views by their sources. Installed
    // before this crate, so the registry is here already.
    if let Some(views) = boot
        .services_mut()
        .get::<lattice_multibuffer::MultibufferRegistryHandle>()
    {
        chats.set_composed((*views).clone());
    }
    // LL.11b: the rewrite. Its answer reaches review, and its result the
    // editor, from a task; the inbound bus carries that back with a wake, so a
    // finished rewrite shows without a keystroke.
    let diffs = boot
        .service::<lattice_diff::ProgrammaticDiffBus>()
        .map(|h| (*h).clone());
    let rewrites: super::rewrite::LlmRewritesHandle = Arc::new(super::rewrite::LlmRewrites::new(
        boot.runtime_handle().clone(),
        diffs.clone(),
    ));
    rewrites.set_effects(boot.inbound::<super::rewrite::RewriteEffects, _>(|e| e.0));
    rewrites.set_store(boot.buffer_store().clone());
    // LL.20: `:llm-save`, `:llm-resume` and `:llm-export` read and write on
    // the runtime; their result (an echo, the resumed chat's buffer) comes
    // back with a wake.
    let files: super::session_file::LlmSessionFilesHandle = Arc::new(
        super::session_file::LlmSessionFiles::new(boot.runtime_handle().clone(), chats.clone()),
    );
    files.set_effects(boot.inbound::<super::session_file::SessionFileEffects, _>(|e| e.0));
    super::session_file::register(boot.commands_mut(), files);
    // LL.16: lattice's read tools, over an editor view of its own (open
    // buffers by path, so a read sees unsaved text). Offered only to requests
    // whose `llm.tools` selects them. LL.17: with a diff bus, the edit tools
    // too, reviewed one diff at a time with the rewrites.
    {
        let cache =
            lattice_agent::state_cache::spawn_read_cache(boot.event_bus(), boot.runtime_handle());
        let access = lattice_agent::EditorAccess::new(
            cache,
            Some(boot.buffer_store().clone()),
            Vec::new(),
            None,
        );
        // Weak: the broker lives inside `chats`, so a strong handle here
        // would be a cycle.
        let (root_of, exclude_of) = (Arc::downgrade(&chats), Arc::downgrade(&chats));
        let mut tools = super::tools::lattice::LatticeTools::new(
            access,
            Arc::new(move || {
                root_of
                    .upgrade()
                    .map(|c| c.tools_root())
                    .unwrap_or_default()
            }),
            // Gone means the editor is closing; refuse rather than read.
            Arc::new(move |p| {
                exclude_of
                    .upgrade()
                    .map_or(Some("(closing)".to_string()), |c| c.excluded(p))
            }),
        );
        let keys_of = Arc::downgrade(&chats);
        tools = tools.without_variables(Arc::new(move || {
            keys_of
                .upgrade()
                .map(|c| c.key_variables())
                .unwrap_or_default()
        }));
        if let Some(diffs) = diffs {
            let effects = Arc::downgrade(&rewrites);
            tools = tools.with_review(super::tools::lattice::Review {
                diffs,
                gate: rewrites.review_gate(),
                // Diff-only: no file-close fallback when nothing matched.
                close: Arc::new(move |origin| {
                    if let Some(r) = effects.upgrade() {
                        r.send(vec![
                            lattice_grammar::effect::Effect::CloseAllSessionDiffs {
                                origin_session: origin,
                            },
                        ]);
                    }
                }),
            });
        }
        chats
            .tools()
            .add_source(super::tools::broker::LATTICE, Arc::new(tools));
    }
    // LL.24: each `[llm.mcp.<name>]` server is a tool source of its own. None
    // starts here: a server starts when a request's `llm.tools` first selects
    // it. A change of state publishes `McpServersChanged`, which repaints
    // `*llm-mcp*`.
    let mcp: super::tools::mcp::McpServersHandle = {
        let bus = boot.event_bus().clone();
        Arc::new(super::tools::mcp::McpServers::new(
            &mcp_configs,
            super::tools::mcp::connect_transport({
                let chats = Arc::downgrade(&chats);
                Arc::new(move || {
                    chats
                        .upgrade()
                        .map(|c| c.key_variables())
                        .unwrap_or_default()
                })
            }),
            boot.runtime_handle(),
            Arc::new(move || bus.publish_typed(super::tools::mcp::McpServersChanged)),
        ))
    };
    mcp.register(chats.tools());
    boot.register_service::<super::tools::mcp::McpServersHandle>(mcp.clone());
    let chat_mode = LlmChatMode::new(chats.clone());
    let anchors = chat_mode.anchors();
    boot.modes_mut()
        .register(chat_mode)
        .expect("llm-chat-mode register");
    boot.modes_mut()
        .register(LlmSystemMode::new(chats.clone()))
        .expect("llm-system-mode register");
    register_actions(boot.commands_mut());
    super::menu::register_actions(boot.commands_mut());
    register_system_action(boot.commands_mut());
    // LL.10b: the ask. The operators capture their span; `llm-mode` owns the
    // prompt, menu-row and confirm handlers and the `<leader>a` layer. Before
    // the menu, which resolves the `a` row's action from the registry.
    let asks: super::operators::LlmAsksHandle = Arc::new(super::operators::LlmAsks::default());
    // LL.13b: `destination=below`. Its writer runs off the editor thread and
    // wakes the editor after each write through the inbound bus, so streamed
    // text and the caret it moves show without a keystroke.
    let inline: super::sink::LlmInlineHandle =
        Arc::new(super::sink::LlmInline::new(boot.runtime_handle().clone()));
    inline.set_effects(boot.inbound::<super::sink::InlineEffects, _>(|e| e.0));
    boot.register_service::<super::sink::LlmInlineHandle>(inline.clone());
    // LL.14d: a chat that answers while its window is closed says so, and
    // quitting with a request running asks for `!` first.
    {
        let store = boot.buffer_store().clone();
        let notices = inline.clone();
        chats.set_done_hook(Arc::new(move |buffer, name, failed| {
            if store.is_displayed(buffer) {
                return;
            }
            let effect = match failed {
                None => super::operators::echo(
                    lattice_grammar::effect::EchoLevel::Info,
                    format!("llm: {name} has answered"),
                ),
                Some(e) => super::operators::echo(
                    lattice_grammar::effect::EchoLevel::Error,
                    format!("llm: {name} failed: {e}"),
                ),
            };
            notices.send(vec![effect]);
        }));
    }
    if let Some(guards) = boot.service::<lattice_mode::QuitGuardsHandle>() {
        let in_flight = chats.clone();
        guards.register(
            "llm",
            Arc::new(move || {
                // Sessions, not the registry: a request being prepared is not
                // registered yet, and quitting would drop it all the same.
                let running = in_flight.in_flight();
                (running > 0).then(|| {
                    let noun = if running == 1 {
                        "request is"
                    } else {
                        "requests are"
                    };
                    format!("llm: {running} {noun} running; :llm-requests lists them")
                })
            }),
        );
    }
    // LL.14b: `*llm-requests*` repaints itself on request state changes and
    // wakes the editor through the same bus.
    let bus = boot.event_bus().clone();
    boot.modes_mut()
        .register(super::modes::requests::LlmRequestsMode::new(
            chats.clone(),
            bus,
            inline.clone(),
        ))
        .expect("llm-requests-mode register");
    super::modes::requests::register_actions(boot.commands_mut());
    // LL.24: `*llm-mcp*`, the servers and their tools.
    let bus = boot.event_bus().clone();
    boot.modes_mut()
        .register(super::modes::mcp::LlmMcpMode::new(mcp, bus, inline.clone()))
        .expect("llm-mcp-mode register");
    super::modes::mcp::register(boot.commands_mut());
    // LL.15: what requests carry, and exactly what one sends. Both views
    // repaint themselves and wake the editor through the same bus.
    boot.modes_mut()
        .register(super::modes::context::LlmContextMode::new(
            chats.clone(),
            inline.clone(),
        ))
        .expect("llm-context-mode register");
    boot.modes_mut()
        .register(super::modes::context::LlmInspectMode::new(
            chats.clone(),
            inline.clone(),
        ))
        .expect("llm-inspect-mode register");
    super::modes::context::register_actions(boot.commands_mut());
    let store = boot.buffer_store().clone();
    let attach_op = super::context::register(boot.commands_mut(), chats.clone(), store, anchors);
    asks.set_inline(inline);
    boot.register_service::<super::operators::LlmAsksHandle>(asks.clone());
    let store = boot.buffer_store().clone();
    let ops = super::operators::register(
        boot.commands_mut(),
        chats.clone(),
        asks.clone(),
        store.clone(),
    );
    super::operators::register_ex(boot.commands_mut(), chats.clone(), asks.clone(), store);
    boot.register_service::<super::rewrite::LlmRewritesHandle>(rewrites.clone());
    let store = boot.buffer_store().clone();
    let rewrite_ops =
        super::rewrite::register(boot.commands_mut(), chats.clone(), rewrites.clone(), store);
    super::commands::register(boot.commands_mut(), chats.clone());
    // LL.15b: every chord sits under `llm.prefix`; `llm-mode` binds them on
    // its first activation and the listener rebinds them when it changes.
    let keys: super::modes::llm_mode::LlmKeysHandle = {
        use super::modes::llm_mode::{LlmKeys, PREFIX_COMMANDS, PrefixBindings, PrefixOperator};
        let registry = boot.commands_mut();
        let operator = |key, op: lattice_grammar::registry::OperatorId| PrefixOperator {
            key,
            op,
            post_motion_char: registry.operator_post_motion_char(op.0),
        };
        let operators = vec![
            operator("a", ops.ask),
            operator("A", ops.ask_menu),
            operator("r", rewrite_ops.rewrite),
            operator("R", rewrite_ops.rewrite_menu),
            operator("c", attach_op),
        ];
        let commands = PREFIX_COMMANDS
            .iter()
            .filter_map(|(key, name)| registry.id_by_name(name).map(|id| (*key, id)))
            .collect();
        let config = boot
            .service::<Arc<lattice_config::ConfigRegistry>>()
            .map(|c| (*c).clone());
        Arc::new(LlmKeys::new(
            PrefixBindings {
                operators,
                commands,
            },
            config,
        ))
    };
    boot.register_service::<super::modes::llm_mode::LlmKeysHandle>(keys.clone());
    super::modes::llm_mode::spawn_prefix_listener(
        keys.clone(),
        boot.event_bus(),
        boot.runtime_handle(),
    );
    boot.modes_mut()
        .register(super::modes::llm_mode::LlmMode::new(
            chats.clone(),
            asks,
            rewrites,
            keys,
        ))
        .expect("llm-mode register");
    // The menu is a transient like magit's; its source is looked up under the
    // handle type, and a harness with no picker layer simply has none.
    if let Some(ids) = super::menu::MenuIds::resolve(boot.commands_mut()) {
        let sources: lattice_picker::TransientSourceRegistryHandle = boot
            .service::<lattice_picker::TransientSourceRegistryHandle>()
            .map(|h| (*h).clone())
            .unwrap_or_else(|| Arc::new(lattice_picker::TransientSourceRegistry::new()));
        super::menu::register_source(&sources, chats.clone(), ids, boot.buffer_store().clone());
    }

    // The drain publishes `ConversationProjected` after each re-projection edit
    // lands; waking the editor on it is what paints a streamed reply without a
    // keystroke. The ACP install registers the same wake; only add it when that
    // feature is absent so it is not subscribed twice.
    #[cfg(not(feature = "acp"))]
    boot.wake_on_event::<crate::conversation::ConversationProjected>();
}

/// Declare `:w`'s action for a system-prompt buffer; the body is the mode's.
fn register_system_action(registry: &mut lattice_grammar::CommandRegistry) {
    use lattice_grammar::effect::Effect;
    use lattice_grammar::registry::ActionSpec;
    registry.register_action(
        super::modes::system::ACTION_APPLY,
        "llm-system: apply the buffer to the session (what :w does there).",
        ActionSpec {
            apply: Arc::new(|_| Ok(Effect::None)),
            args_schema: vec![],
        },
    );
}
