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

use super::chat::{LlmChats, LlmChatsHandle, MenuScope};
use super::menu::MENU_SOURCE;
use super::modes::chat::LlmChatMode;
use super::modes::system::LlmSystemMode;
use super::settings::{RawSet, SettingKey, validate};

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
fn unknown_profile(chats: &LlmChats, name: &str) -> String {
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

/// The effects that drop every override `buffer` holds: its `:setlocal llm.*`
/// values, the chat's creation-time copy and any `:llm-menu` choice waiting for
/// the next request. The echo names what went.
pub fn clear_overrides(chats: &LlmChats, buffer: BufferId) -> Effect {
    let (mut cleared, pending) = take_overrides(chats, buffer, true);
    cleared.sort();
    let mut effects: Vec<Effect> = cleared
        .iter()
        .map(|k| Effect::SetLocalOption {
            spec: format!("{}&", k.option_name()),
        })
        .collect();
    let mut names: Vec<&str> = cleared.iter().map(|k| k.option_name()).collect();
    names.extend(pending.iter().map(|k| k.option_name()));
    let text = if names.is_empty() {
        "llm: no overrides to clear".to_string()
    } else {
        format!("llm: cleared {}", names.join(", "))
    };
    effects.push(echo(EchoLevel::Info, text));
    Effect::Many(effects)
}

/// Forget `buffer`'s overrides and say which. `with_profile` keeps or drops
/// `llm.profile` among them: a profile switch keeps it (it is about to be set).
fn take_overrides(
    chats: &LlmChats,
    buffer: BufferId,
    with_profile: bool,
) -> (Vec<SettingKey>, Vec<SettingKey>) {
    let mut cleared: Vec<SettingKey> = chats
        .session_overrides(buffer)
        .into_keys()
        .filter(|k| with_profile || *k != SettingKey::Profile)
        .collect();
    // The chat's creation-time copy is an override like any other.
    for k in chats.clear_seed(buffer) {
        if (with_profile || k != SettingKey::Profile) && !cleared.contains(&k) {
            cleared.push(k);
        }
    }
    let pending = chats.clear_next_request(buffer);
    (cleared, pending)
}

/// The effects of `:llm-profile name` from `buffer`: clear each session override,
/// set the profile, then echo what was cleared.
pub fn switch_profile(chats: &LlmChats, buffer: BufferId, name: &str) -> Effect {
    if !chats.profile_names().iter().any(|p| p == name) {
        return echo(EchoLevel::Error, unknown_profile(chats, name));
    }
    let (mut cleared, pending) = take_overrides(chats, buffer, false);
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
    let mut names: Vec<&str> = cleared.iter().map(|k| k.option_name()).collect();
    names.extend(pending.iter().map(|k| k.option_name()));
    let text = if names.is_empty() {
        format!("llm: profile {name}")
    } else {
        format!("llm: profile {name}; cleared {}", names.join(", "))
    };
    effects.push(echo(EchoLevel::Info, text));
    Effect::Many(effects)
}

/// The keys a command argument may set for one request.
fn arg_key(name: &str) -> Option<SettingKey> {
    match name {
        "profile" => Some(SettingKey::Profile),
        other => SettingKey::from_file_key(other).filter(|k| *k != SettingKey::System),
    }
}

/// Split leading `key=value` words off a command's text: the overrides for one
/// request, and the rest, which is the prompt. `:llm-ask model=local/qwen
/// temperature=0 explain this` gives `{model, temperature}` and `explain this`.
///
/// The words stop at the first one that is not a known key, so a prompt that
/// starts `x=1 is wrong` is a prompt. A known key with a bad value is an error
/// naming it, never silently part of the prompt. `system` is not accepted here:
/// a system prompt is a paragraph and has `:llm-system`.
pub fn parse_request_args(text: &str) -> Result<(RawSet, String), String> {
    let mut set = RawSet::new();
    let mut rest = text.trim_start();
    while let Some(word_end) = rest.find(char::is_whitespace).or(Some(rest.len())) {
        let word = &rest[..word_end];
        let Some((name, value)) = word.split_once('=') else {
            break;
        };
        let Some(key) = arg_key(name) else {
            break;
        };
        validate(key, value).map_err(|e| format!("llm: {e}"))?;
        if value.is_empty() {
            return Err(format!("llm: {name}= needs a value"));
        }
        set.insert(key, value.to_string());
        rest = rest[word_end..].trim_start();
        if rest.is_empty() {
            break;
        }
    }
    Ok((set, rest.to_string()))
}

/// The synthetic name of the buffer `:llm-system` opens for `target`. The
/// counter makes every open a fresh buffer, so a discarded edit (`:q!`) is not
/// what the next open shows. A form opened for the next request only says so
/// (`...:next*`): the scope is read back from the name when `:w` applies it.
pub fn system_buffer_name(target: BufferId, open: u32, scope: MenuScope) -> String {
    match scope {
        MenuScope::Session => format!("*llm-system:{}:{open}*", target.0),
        MenuScope::NextRequest => format!("*llm-system:{}:{open}:next*", target.0),
    }
}

fn system_parts(name: &str) -> Option<(BufferId, MenuScope)> {
    let inner = name.strip_prefix("*llm-system:")?.strip_suffix('*')?;
    let mut parts = inner.split(':');
    let buffer = BufferId(parts.next()?.parse().ok()?);
    let _open: u32 = parts.next()?.parse().ok()?;
    let scope = match parts.next() {
        None => MenuScope::Session,
        Some("next") => MenuScope::NextRequest,
        Some(_) => return None,
    };
    parts.next().is_none().then_some((buffer, scope))
}

/// The buffer a `*llm-system:...*` buffer edits the system prompt of.
pub fn system_target(name: &str) -> Option<BufferId> {
    system_parts(name).map(|(b, _)| b)
}

/// The scope a `*llm-system:...*` buffer was opened in.
pub fn system_scope(name: &str) -> Option<MenuScope> {
    system_parts(name).map(|(_, s)| s)
}

/// Open the system prompt of `buffer`'s session in an ordinary buffer. `text`
/// seeds it (a range's lines); without it the prompt in force does, or nothing.
/// `scope` is where `:w` will apply it. A prompt that names a file shows the
/// path: the buffer edits text, and reading the file is not something to do on
/// the editor thread.
pub fn open_system(
    chats: &LlmChats,
    buffer: BufferId,
    text: Option<String>,
    scope: MenuScope,
) -> Effect {
    let content = text.unwrap_or_else(|| {
        chats
            .resolve_for(buffer)
            .ok()
            .and_then(|r| r.values.system)
            .unwrap_or_default()
    });
    Effect::OpenSyntheticBuffer {
        name: system_buffer_name(buffer, chats.next_system_open(), scope),
        mode_id: LlmSystemMode::mode_id().as_str().to_string(),
        content: Some(content),
        cursor: None,
        activate_minor: None,
    }
}

/// The lines a `:'<,'>llm-system` range names, joined. The host has already
/// made a Visual selection into line addresses, so only lines arrive here.
fn range_text(ctx: &lattice_grammar::registry::ExCommandContext) -> Result<Option<String>, String> {
    use lattice_grammar::range::{RangeEnv, resolve_lines};
    let Some(range) = &ctx.range else {
        return Ok(None);
    };
    let marks: std::collections::HashMap<char, lattice_protocol::position::Position> =
        std::collections::HashMap::new();
    let search = |_: &str, _: u32, _: bool| -> Result<Option<u32>, String> { Ok(None) };
    let last = ctx.buffer.content_line_count().saturating_sub(1);
    let env = RangeEnv {
        cursor: ctx.cursor.line,
        last,
        marks: &marks,
        search: &search,
    };
    match resolve_lines(range, &env, true).map_err(|e| format!("llm: {}", e.0))? {
        Some((a, b)) => Ok(Some(
            (a..=b)
                .filter_map(|l| ctx.buffer.line(l))
                .collect::<Vec<_>>()
                .join("\n"),
        )),
        None => Err("llm: give :llm-system a line range or select text first".to_string()),
    }
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
    let profile_chats = chats.clone();
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
                    let known = profile_chats.profile_names();
                    let text = if known.is_empty() {
                        "llm: no profiles are configured (add [llm.profiles.<name>] to lattice.toml)"
                            .to_string()
                    } else {
                        format!("llm: profiles: {}", known.join(", "))
                    };
                    return Ok(echo(EchoLevel::Info, text));
                }
                Ok(switch_profile(&profile_chats, ctx.buffer_id, &name))
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    registry.register_ex_command(
        "llm-menu",
        "Show every LLM setting for this buffer's session with where each value came from, and \
         change them. `=` switches between the session and the next request only.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_rest_as_text),
            apply: Arc::new(|_| {
                Ok(Effect::OpenTransient {
                    source: MENU_SOURCE.to_string(),
                    args: Args::None,
                })
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let system = chats;
    registry.register_ex_command(
        "llm-system",
        "Edit this session's system prompt in a buffer. `:w` applies it and closes the buffer; \
         `:q!` discards. With a line range, the range seeds the buffer.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: true,
            parse_args: Arc::new(parse_rest_as_text),
            apply: Arc::new(move |ctx| match range_text(ctx) {
                Ok(text) => Ok(open_system(
                    &system,
                    ctx.buffer_id,
                    text,
                    MenuScope::Session,
                )),
                Err(e) => Ok(echo(EchoLevel::Error, e)),
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
}
