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
pub fn install(boot: &mut impl SubsystemBoot) {
    let registry: RequestRegistryHandle = Arc::new(RequestRegistry::new(
        boot.runtime_handle().clone(),
        Some(boot.event_bus().clone()),
        None,
    ));
    boot.register_service::<RequestRegistryHandle>(registry.clone());

    // The user's `[llm]` table, read once here (boot is not the editor thread's
    // hot path). Problems become log lines; a bad provider is skipped.
    let config = LlmConfig::load_user();
    for warning in &config.warnings {
        tracing::warn!(target: "lattice::llm", "{warning}");
    }

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

    boot.modes_mut()
        .register(LlmChatMode::new(chats.clone()))
        .expect("llm-chat-mode register");
    boot.modes_mut()
        .register(LlmSystemMode::new(chats.clone()))
        .expect("llm-system-mode register");
    register_actions(boot.commands_mut());
    super::menu::register_actions(boot.commands_mut());
    register_system_action(boot.commands_mut());
    // The menu is a transient like magit's; its source is looked up under the
    // handle type, and a harness with no picker layer simply has none.
    if let Some(ids) = super::menu::MenuIds::resolve(boot.commands_mut()) {
        let sources: lattice_picker::TransientSourceRegistryHandle = boot
            .service::<lattice_picker::TransientSourceRegistryHandle>()
            .map(|h| (*h).clone())
            .unwrap_or_else(|| Arc::new(lattice_picker::TransientSourceRegistry::new()));
        super::menu::register_source(&sources, chats.clone(), ids, boot.buffer_store().clone());
    }
    super::commands::register(boot.commands_mut(), chats);

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
