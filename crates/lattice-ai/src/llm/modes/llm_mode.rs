//! `llm-mode`: the LLM layer's surface in every buffer (design §2).
//!
//! A minor mode, active on every buffer kind (`Universal`), because asking is
//! as valid from `*compilation*`, a terminal or a multibuffer as from a file
//! (design §1). It owns the `<leader>a` chords and their handler bodies; the
//! chat buffer's own chords are `llm-chat-mode`'s.
//!
//! The operator chords (`<leader>aa{motion}`, and the linewise `<leader>aaa`;
//! `<leader>aA` through the menu; `<leader>ar{motion}`, the rewrite)
//! are bound into this mode's keymap layer by `install` through
//! `SubsystemBoot::bind_operator`, so they exist only where the mode is active;
//! a `keymap()` entry cannot express operator-pending composition.

use lattice_mode::{
    ActionHandlerContribution, ActivationPolicy, CapabilitySet, LifecycleFuture, Mode, ModeContext,
    ModeId, ModeKind,
};

use crate::llm::chat::LlmChatsHandle;
use crate::llm::operators::{
    ACTION_ASK_CONFIRMED, ACTION_ASK_CONTINUE, ACTION_ASK_SEND, LlmAsksHandle, confirmed_handler,
    continue_handler, send_handler,
};
use crate::llm::rewrite::{ACTION_REWRITE_SEND, LlmRewritesHandle};

pub struct LlmMode {
    chats: LlmChatsHandle,
    asks: LlmAsksHandle,
    rewrites: LlmRewritesHandle,
}

impl LlmMode {
    pub fn new(chats: LlmChatsHandle, asks: LlmAsksHandle, rewrites: LlmRewritesHandle) -> Self {
        Self {
            chats,
            asks,
            rewrites,
        }
    }

    pub fn mode_id() -> ModeId {
        ModeId::new("llm-mode")
    }
}

impl Mode for LlmMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Universal
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![
            ActionHandlerContribution {
                action_name: ACTION_ASK_SEND,
                handler: send_handler(self.chats.clone(), self.asks.clone()),
            },
            ActionHandlerContribution {
                action_name: ACTION_ASK_CONTINUE,
                handler: continue_handler(self.chats.clone(), self.asks.clone()),
            },
            ActionHandlerContribution {
                action_name: ACTION_ASK_CONFIRMED,
                handler: confirmed_handler(self.chats.clone(), self.asks.clone()),
            },
            ActionHandlerContribution {
                action_name: ACTION_REWRITE_SEND,
                handler: crate::llm::rewrite::send_handler(
                    self.chats.clone(),
                    self.rewrites.clone(),
                ),
            },
        ]
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
