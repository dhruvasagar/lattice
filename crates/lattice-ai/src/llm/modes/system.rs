//! `llm-system-mode`: the system prompt of a session, edited in an ordinary
//! buffer (`:llm-system`).
//!
//! The buffer is a form, not a file. `:w` **applies** the text to the session
//! it was opened from and closes the buffer; `:q!` discards. `:w` is claimed
//! through [`Mode::write_action`], so nothing is written to disk and the host
//! learns nothing about system prompts. The buffer's name carries the session's
//! buffer (`*llm-system:<buffer>:<n>*`), which is how the handler, running on
//! the system buffer, finds what to apply to.

use std::sync::Arc;

use lattice_core::BufferId;
use lattice_grammar::effect::{EchoLevel, Effect};
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerContribution, BufferStoreHandle, CapabilitySet,
    LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
};

use crate::llm::chat::{LlmChats, LlmChatsHandle, MenuScope};
use crate::llm::commands::{system_scope, system_target};
use crate::llm::settings::{SettingKey, validate};

/// The action `:w` runs in a system-prompt buffer.
pub const ACTION_APPLY: &str = "action:llm-system-apply";

pub struct LlmSystemMode {
    chats: LlmChatsHandle,
}

impl LlmSystemMode {
    pub fn new(chats: LlmChatsHandle) -> Self {
        Self { chats }
    }

    pub fn mode_id() -> ModeId {
        ModeId::new("llm-system-mode")
    }
}

impl Mode for LlmSystemMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn write_action(&self) -> Option<&'static str> {
        Some(ACTION_APPLY)
    }

    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![ActionHandlerContribution {
            action_name: ACTION_APPLY,
            handler: apply_handler(self.chats.clone()),
        }]
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async { Ok(()) })
    }
}

/// The effects of `:w` in a system buffer holding `text`, for the session of
/// `target`. Pure, so it is testable without a host.
///
/// An option write names no target of its own: it lands on the active buffer.
/// So the form is closed (`BufferDelete` is applied in order, where the
/// `KillBuffer` of a compose buffer is applied after every host effect and
/// would leave the write on the wrong buffer) and `target` is focused
/// explicitly before the write, rather than trusting which buffer the close
/// happened to fall back to.
pub fn apply_effects(text: &str, target: BufferId) -> Effect {
    let text = text.trim();
    if let Err(e) = validate(SettingKey::System, text) {
        return Effect::Echo {
            level: EchoLevel::Error,
            text: format!("llm: {e}"),
        };
    }
    let (spec, said) = if text.is_empty() {
        (
            format!("{}&", SettingKey::System.option_name()),
            "llm: system prompt cleared for the session".to_string(),
        )
    } else {
        (
            format!("{}={text}", SettingKey::System.option_name()),
            "llm: system prompt applied to the session".to_string(),
        )
    };
    close_and_say(target, &said, Some(spec))
}

/// Close the form, focus `target`, optionally set an option there, then say what
/// happened. In that order, and with `target` named: see [`apply_effects`].
fn close_and_say(target: BufferId, said: &str, spec: Option<String>) -> Effect {
    let mut parts = vec![
        Effect::BufferDelete { force: true },
        Effect::FocusBuffer(target.0),
    ];
    parts.extend(spec.map(|spec| Effect::SetLocalOption { spec }));
    parts.push(Effect::Echo {
        level: EchoLevel::Info,
        text: said.to_string(),
    });
    Effect::Many(parts)
}

fn error(text: &str) -> Effect {
    Effect::Echo {
        level: EchoLevel::Error,
        text: text.to_string(),
    }
}

/// The effects of `:w` in the form named `name`, holding `text`, for `target`:
/// to the session, or held for the next request, as the form was opened.
pub fn apply_text(chats: &LlmChats, name: &str, text: &str, target: BufferId) -> Effect {
    match system_scope(name).unwrap_or_default() {
        MenuScope::Session => apply_effects(text, target),
        MenuScope::NextRequest => {
            let text = text.trim();
            if let Err(e) = validate(SettingKey::System, text) {
                return error(&format!("llm: {e}"));
            }
            chats.set_next_request(target, SettingKey::System, text);
            close_and_say(
                target,
                if text.is_empty() {
                    "llm: system prompt cleared for the next request"
                } else {
                    "llm: system prompt applied to the next request only"
                },
                None,
            )
        }
    }
}

fn apply_handler(chats: LlmChatsHandle) -> ActionHandler {
    Arc::new(move |ctx: &ActionContext<'_>| {
        // `:w` here is a submit. A form it cannot place gets an answer, not a
        // silent no-op: the text is the user's work and stays open.
        let Some(store) = ctx.services.get::<BufferStoreHandle>() else {
            return Some(error(
                "llm: cannot apply the system prompt (no buffer store)",
            ));
        };
        let me = BufferId(ctx.buffer_id.0 as u32);
        let Some(name) = store.name_for(me) else {
            return Some(error("llm: this buffer is not a system prompt form"));
        };
        let Some(target) = system_target(&name) else {
            return Some(error(&format!(
                "llm: `{name}` does not say which session its system prompt is for; \
                 open it with :llm-system"
            )));
        };
        if !store.contains_buffer(target) {
            // The session this was written for is gone. Keep the form open: the
            // text is the user's work, and there is nowhere to apply it.
            return Some(error(
                "llm: the buffer this system prompt was opened from is closed",
            ));
        }
        let Some(handle) = store.handle_for(me) else {
            return Some(error("llm: cannot read this system prompt form"));
        };
        let text = handle.snapshot().buffer.as_string();
        Some(apply_text(&chats, &name, &text, target))
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::panic)]
    use super::*;

    #[test]
    fn writing_closes_the_form_focuses_the_target_then_sets_the_prompt_there() {
        let Effect::Many(parts) = apply_effects("You are terse.\n", BufferId(7)) else {
            panic!("expected a batch");
        };
        assert!(matches!(parts[0], Effect::BufferDelete { force: true }));
        assert!(matches!(parts[1], Effect::FocusBuffer(7)));
        assert!(matches!(
            &parts[2],
            Effect::SetLocalOption { spec } if spec == "llm.system=You are terse."
        ));
    }

    #[test]
    fn an_empty_buffer_clears_the_prompt() {
        let Effect::Many(parts) = apply_effects("  \n", BufferId(7)) else {
            panic!("expected a batch");
        };
        assert!(matches!(
            &parts[2],
            Effect::SetLocalOption { spec } if spec == "llm.system&"
        ));
    }
}
