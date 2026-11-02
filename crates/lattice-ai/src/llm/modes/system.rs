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
    ActionContext, ActionHandlerContribution, BufferStoreHandle, CapabilitySet, LifecycleFuture,
    Mode, ModeContext, ModeId, ModeKind,
};

use crate::llm::commands::system_target;
use crate::llm::settings::{SettingKey, validate};

/// The action `:w` runs in a system-prompt buffer.
pub const ACTION_APPLY: &str = "action:llm-system-apply";

pub struct LlmSystemMode;

impl LlmSystemMode {
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
            handler: Arc::new(apply),
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
    Effect::Many(vec![
        Effect::BufferDelete { force: true },
        Effect::FocusBuffer(target.0),
        Effect::SetLocalOption { spec },
        Effect::Echo {
            level: EchoLevel::Info,
            text: said,
        },
    ])
}

fn apply(ctx: &ActionContext<'_>) -> Option<Effect> {
    let store = ctx.services.get::<BufferStoreHandle>()?;
    let me = BufferId(ctx.buffer_id.0 as u32);
    // A buffer that is not one `:llm-system` made applies to nothing.
    let target = system_target(&store.name_for(me)?)?;
    if !store.contains_buffer(target) {
        // The session this was written for is gone. Keep the form open: the
        // text is the user's work, and there is nowhere to apply it.
        return Some(Effect::Echo {
            level: EchoLevel::Error,
            text: "llm: the buffer this system prompt was opened from is closed".to_string(),
        });
    }
    let text = store.handle_for(me)?.snapshot().buffer.as_string();
    Some(apply_effects(&text, target))
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
