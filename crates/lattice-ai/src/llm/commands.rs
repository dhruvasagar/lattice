//! `:llm` -- open a new chat buffer.
//!
//! Binding and body live here, in the owning crate. Creating the chat is cheap
//! and non-blocking (no I/O: the config was read at boot), so the ex-command may
//! do it on the editor thread; the buffer is then opened by the generic
//! `Effect::OpenSyntheticBuffer`, and `llm-chat-mode`'s `on_activate` binds it to
//! the chat by name.

use std::sync::Arc;

use lattice_agent::parse_no_args;
use lattice_grammar::command::LatencyClass;
use lattice_grammar::effect::Effect;
use lattice_grammar::registry::{CommandRegistry, ExCommandSpec, SurfaceForm};

use super::chat::LlmChatsHandle;
use super::modes::chat::LlmChatMode;

pub fn register(registry: &mut CommandRegistry, chats: LlmChatsHandle) {
    registry.register_ex_command(
        "llm",
        "Open a new chat with a language model. Every chat gets its own buffer, \
         *llm:<n>*, so questions never share a history.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(move |_ctx| {
                let session = chats.create();
                Ok(Effect::OpenSyntheticBuffer {
                    name: session.name.clone(),
                    mode_id: LlmChatMode::mode_id().as_str().to_string(),
                    content: None,
                    cursor: None,
                    activate_minor: None,
                })
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
}
