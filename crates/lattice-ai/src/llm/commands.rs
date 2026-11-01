//! `:llm [profile]` -- open a new chat buffer -- and `:llm-profile`, the one way
//! to switch a session's profile.
//!
//! Binding and body live here, in the owning crate. Creating the chat is cheap
//! and non-blocking (no I/O: the config was read at boot), so the ex-command may
//! do it on the editor thread; the buffer is then opened by the generic
//! `Effect::OpenSyntheticBuffer`, and `llm-chat-mode`'s `on_activate` binds it to
//! the chat by name.
//!
//! # Switching profile
//!
//! A profile replaces the base a session's overrides sit on, so overrides made
//! on the old base no longer mean what they did. `:llm-profile x` therefore
//! clears them and says which, rather than leaving a `llm.model` the user set
//! for the previous profile silently shadowing the new one. `:setlocal
//! llm.profile=x` cannot do that (a write to one option cannot clear its
//! siblings), so it is not the switch; the command is. `:setlocal llm.profile=x`
//! still works and only changes the base.

use std::sync::Arc;

use lattice_agent::parse_rest_as_text;
use lattice_core::BufferId;
use lattice_grammar::args::Args;
use lattice_grammar::command::LatencyClass;
use lattice_grammar::effect::{EchoLevel, Effect};
use lattice_grammar::registry::{CommandRegistry, ExCommandSpec, SurfaceForm};

use super::chat::LlmChatsHandle;
use super::modes::chat::LlmChatMode;
use super::settings::SettingKey;

fn arg_text(args: &Args) -> String {
    match args {
        Args::String(t) => t.trim().to_string(),
        _ => String::new(),
    }
}

fn echo(level: EchoLevel, text: String) -> Effect {
    Effect::Echo { level, text }
}

/// `unknown profile `x` (known: a, b)`.
fn unknown_profile(chats: &LlmChatsHandle, name: &str) -> String {
    let known = chats.profile_names();
    if known.is_empty() {
        format!(
            "llm: unknown profile `{name}`: no profiles are configured (add [llm.profiles.<name>] to lattice.toml)"
        )
    } else {
        format!(
            "llm: unknown profile `{name}` (known: {})",
            known.join(", ")
        )
    }
}

/// The effects of `:llm-profile name` from `buffer`: clear each session override,
/// set the profile, then echo what was cleared.
pub fn switch_profile(chats: &LlmChatsHandle, buffer: BufferId, name: &str) -> Effect {
    if !chats.profile_names().iter().any(|p| p == name) {
        return echo(EchoLevel::Error, unknown_profile(chats, name));
    }
    let mut cleared: Vec<SettingKey> = chats
        .session_overrides(buffer)
        .into_keys()
        .filter(|k| *k != SettingKey::Profile)
        .collect();
    // The chat's creation-time copy is an override like any other.
    for k in chats.clear_seed(buffer) {
        if k != SettingKey::Profile && !cleared.contains(&k) {
            cleared.push(k);
        }
    }
    cleared.sort();
    let mut effects: Vec<Effect> = cleared
        .iter()
        .map(|k| Effect::SetLocalOption {
            spec: format!("{}&", k.option_name()),
        })
        .collect();
    effects.push(Effect::SetLocalOption {
        spec: format!("llm.profile={name}"),
    });
    let text = if cleared.is_empty() {
        format!("llm: profile {name}")
    } else {
        let names: Vec<&str> = cleared.iter().map(|k| k.option_name()).collect();
        format!("llm: profile {name}; cleared {}", names.join(", "))
    };
    effects.push(echo(EchoLevel::Info, text));
    Effect::Many(effects)
}

pub fn register(registry: &mut CommandRegistry, chats: LlmChatsHandle) {
    let open = chats.clone();
    registry.register_ex_command(
        "llm",
        "Open a new chat with a language model, optionally starting from a profile \
         (`:llm writer`). Every chat gets its own buffer, *llm:<n>*, so questions never \
         share a history.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_rest_as_text),
            apply: Arc::new(move |ctx| {
                let profile = arg_text(&ctx.args);
                let profile = (!profile.is_empty()).then_some(profile);
                if let Some(p) = &profile
                    && !open.profile_names().iter().any(|n| n == p)
                {
                    return Ok(echo(EchoLevel::Error, unknown_profile(&open, p)));
                }
                let session = open.create_with(profile.as_deref(), Some(ctx.buffer_id));
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
    registry.register_ex_command(
        "llm-profile",
        "Switch this session's profile. The profile replaces the base the session's \
         overrides sit on, so they are cleared, and the echo names them.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_rest_as_text),
            apply: Arc::new(move |ctx| {
                let name = arg_text(&ctx.args);
                if name.is_empty() {
                    let known = chats.profile_names();
                    let text = if known.is_empty() {
                        "llm: no profiles are configured (add [llm.profiles.<name>] to lattice.toml)"
                            .to_string()
                    } else {
                        format!("llm: profiles: {}", known.join(", "))
                    };
                    return Ok(echo(EchoLevel::Info, text));
                }
                Ok(switch_profile(&chats, ctx.buffer_id, &name))
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
}
