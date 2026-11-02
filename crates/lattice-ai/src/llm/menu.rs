//! `:llm-menu` -- overriding the settings on the go (design §4).
//!
//! One `TransientSpec`, the menu mechanism magit and org already use. Each
//! setting is a row that shows its effective value and **where that value came
//! from**; pressing its key asks for a new one. `=` switches the menu between
//! two scopes and stays switched:
//!
//! - **session** (the default): the choice becomes a `:setlocal llm.*` on the
//!   buffer, so `:set llm.model?` shows it and it lasts until cleared;
//! - **next request**: the choice is held here and taken by the next
//!   submission, so it never reaches a second request.
//!
//! The title names the session the next request would use: the chat itself, or
//! `new session from foo.rs` for a buffer that has no chat yet.
//!
//! Every chord and every handler body lives in this crate. The only host
//! pieces are the generic ones: `OpenTransient`, `OpenPrompt`, `SetLocalOption`.
//! A prompt's submit handler runs against the *prompt's* buffer, so the target
//! buffer and key travel in the prompt's name, as they do for magit's prompts.

use std::sync::Arc;

use lattice_config::OptionOrigin;
use lattice_core::BufferId;
use lattice_grammar::CommandRegistry;
use lattice_grammar::args::Args;
use lattice_grammar::effect::{EchoLevel, Effect};
use lattice_grammar::registry::ActionSpec;
use lattice_mode::{ActionContext, ActionHandler, ActionHandlerContribution, BufferStoreHandle};
use lattice_picker::{
    TransientGroup, TransientItem, TransientItemKind, TransientSourceRegistry, TransientSpec,
};
use lattice_protocol::ids::CommandId;

use super::chat::{LlmChats, LlmChatsHandle, MenuScope};
use super::commands::{clear_overrides, open_system, switch_profile};
use super::settings::{MessageScope, SettingKey, Settings, validate};

/// The transient source `:llm-menu` and the chat chord open.
pub const MENU_SOURCE: &str = "llm-menu";

pub const ACTION_EDIT: &str = "action:llm-menu-edit";
pub const ACTION_APPLY: &str = "action:llm-menu-apply";
pub const ACTION_SCOPE: &str = "action:llm-menu-scope";
pub const ACTION_CLEAR: &str = "action:llm-menu-clear";

/// `(key, setting, label)`, in menu order.
const ROWS: [(&str, SettingKey, &str); 9] = [
    ("m", SettingKey::Model, "model"),
    ("s", SettingKey::System, "system prompt"),
    ("t", SettingKey::Temperature, "temperature"),
    ("n", SettingKey::MaxTokens, "max tokens"),
    ("r", SettingKey::Reasoning, "reasoning"),
    ("h", SettingKey::Messages, "history sent"),
    ("T", SettingKey::Tools, "tools"),
    ("d", SettingKey::Destination, "destination"),
    ("p", SettingKey::Profile, "profile"),
];

/// The command ids the menu's rows fire, resolved once after the actions are
/// registered.
#[derive(Debug, Clone, Copy)]
pub struct MenuIds {
    edit: CommandId,
    scope: CommandId,
    clear: CommandId,
}

impl MenuIds {
    pub fn resolve(registry: &CommandRegistry) -> Option<Self> {
        Some(Self {
            edit: registry.id_by_name(ACTION_EDIT)?,
            scope: registry.id_by_name(ACTION_SCOPE)?,
            clear: registry.id_by_name(ACTION_CLEAR)?,
        })
    }
}

fn scope_word(scope: MenuScope) -> &'static str {
    match scope {
        MenuScope::Session => "session",
        MenuScope::NextRequest => "next request",
    }
}

/// A setting's value as the menu shows it.
fn display_value(s: &Settings, key: SettingKey) -> String {
    let or = |v: Option<String>, none: &str| v.unwrap_or_else(|| none.to_string());
    match key {
        SettingKey::Model => or(s.model.clone(), "(provider default)"),
        SettingKey::System => match &s.system {
            None => "(none)".to_string(),
            Some(t) => {
                let line = t.lines().next().unwrap_or("").trim();
                let mut out: String = line.chars().take(40).collect();
                if line.chars().count() > 40 || t.trim().lines().count() > 1 {
                    out.push('\u{2026}');
                }
                out
            }
        },
        SettingKey::Temperature => or(s.temperature.map(|t| t.to_string()), "(provider default)"),
        SettingKey::MaxTokens => or(s.max_tokens.map(|n| n.to_string()), "(provider default)"),
        SettingKey::Reasoning => or(s.reasoning.clone(), "(provider default)"),
        SettingKey::Messages => match s.messages {
            MessageScope::All => "all".to_string(),
            MessageScope::Last => "last".to_string(),
            MessageScope::Trailing(n) => n.to_string(),
        },
        SettingKey::Tools => {
            if s.tools.is_empty() {
                "none".to_string()
            } else {
                s.tools.iter().cloned().collect::<Vec<_>>().join(",")
            }
        }
        SettingKey::Destination => or(s.destination.clone(), "(default)"),
        SettingKey::Profile => or(s.profile.clone(), "(none)"),
        SettingKey::FoldReasoning | SettingKey::FoldTools => String::new(),
    }
}

/// Build the menu for `buffer`. `label` names the buffer when it has no chat.
pub fn build(
    chats: &LlmChats,
    ids: &MenuIds,
    buffer: Option<BufferId>,
    label: &str,
) -> TransientSpec {
    let buffer = buffer.unwrap_or(BufferId(0));
    let scope = chats.menu_scope(buffer);
    let session = match chats.for_buffer(buffer) {
        Some(chat) => chat.name.clone(),
        None => format!("new session from {label}"),
    };
    let (settings, origins, problem) = match chats.resolve_for(buffer) {
        Ok(r) => (r.values, r.origins, None),
        Err(e) => (Settings::default(), Default::default(), Some(e.to_string())),
    };
    let pending = chats.next_request(buffer);

    let mut rows = Vec::with_capacity(ROWS.len());
    for (key, setting, name) in ROWS {
        let origin = origins
            .get(&setting)
            .cloned()
            .unwrap_or(OptionOrigin::Default);
        let mut description = format!("{}  ({origin})", display_value(&settings, setting));
        if let Some(next) = pending.get(&setting) {
            description.push_str(&format!("  -> {next} on the next request"));
        }
        rows.push(TransientItem {
            key: vec![key.to_string()],
            label: name.to_string(),
            description,
            kind: TransientItemKind::Action {
                command: ids.edit,
                args: Args::String(setting.option_name().to_string()),
            },
        });
    }

    let title = match problem {
        Some(p) => format!("LLM settings for {session} -- {p}"),
        None => format!("LLM settings for {session}"),
    };
    TransientSpec {
        title,
        groups: vec![
            TransientGroup {
                label: format!("Scope: {}", scope_word(scope)),
                items: vec![TransientItem {
                    key: vec!["=".to_string()],
                    label: "switch scope".to_string(),
                    description: match scope {
                        MenuScope::Session => {
                            "now: changes last for the session; = for the next request only"
                        }
                        MenuScope::NextRequest => {
                            "now: changes apply to the next request only; = for the session"
                        }
                    }
                    .to_string(),
                    kind: TransientItemKind::action(ids.scope),
                }],
            },
            TransientGroup {
                label: "Settings".to_string(),
                items: rows,
            },
            TransientGroup {
                label: "Overrides".to_string(),
                items: vec![TransientItem {
                    key: vec!["x".to_string()],
                    label: "clear".to_string(),
                    description: "drop this session's overrides and any pending choice".to_string(),
                    kind: TransientItemKind::action(ids.clear),
                }],
            },
        ],
        preview: None,
        footer: Some("= scope  q dismiss".to_string()),
    }
}

/// Register `llm-menu` with the transient source registry. `store` names a
/// buffer that has no chat.
pub fn register_source(
    sources: &TransientSourceRegistry,
    chats: LlmChatsHandle,
    ids: MenuIds,
    store: BufferStoreHandle,
) {
    sources.register(MENU_SOURCE, move |ctx| {
        let label = ctx
            .buffer
            .map(|b| {
                store
                    .path_for(b)
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .or_else(|| store.name_for(b))
                    .unwrap_or_else(|| format!("buffer {}", b.0))
            })
            .unwrap_or_else(|| "this buffer".to_string());
        build(&chats, &ids, ctx.buffer, &label)
    });
}

fn echo(level: EchoLevel, text: String) -> Effect {
    Effect::Echo { level, text }
}

/// Apply one menu choice from `buffer`, in the scope the menu is in. Empty
/// `value` clears the setting. Returns the effects to run on the buffer the
/// menu was opened over.
pub fn apply_choice(chats: &LlmChats, buffer: BufferId, key: SettingKey, value: &str) -> Effect {
    let value = value.trim();
    if let Err(e) = validate(key, value) {
        return echo(EchoLevel::Error, format!("llm: {e}"));
    }
    let scope = chats.menu_scope(buffer);
    if key == SettingKey::Profile {
        if value.is_empty() {
            return echo(EchoLevel::Error, "llm: name a profile".to_string());
        }
        return match scope {
            MenuScope::Session => switch_profile(chats, buffer, value),
            MenuScope::NextRequest => {
                if !chats.profile_names().iter().any(|p| p == value) {
                    return echo(EchoLevel::Error, format!("llm: unknown profile `{value}`"));
                }
                chats.set_next_request(buffer, key, value);
                echo(
                    EchoLevel::Info,
                    format!("llm: profile {value} for the next request"),
                )
            }
        };
    }
    match scope {
        MenuScope::Session => {
            let spec = if value.is_empty() {
                format!("{}&", key.option_name())
            } else {
                format!("{}={value}", key.option_name())
            };
            Effect::Many(vec![
                Effect::SetLocalOption { spec },
                echo(
                    EchoLevel::Info,
                    if value.is_empty() {
                        format!("llm: {} cleared for the session", key.option_name())
                    } else {
                        format!("llm: {}={value} for the session", key.option_name())
                    },
                ),
            ])
        }
        MenuScope::NextRequest => {
            chats.set_next_request(buffer, key, value);
            echo(
                EchoLevel::Info,
                if value.is_empty() {
                    format!("llm: {} cleared for the next request", key.option_name())
                } else {
                    format!("llm: {}={value} for the next request", key.option_name())
                },
            )
        }
    }
}

/// The prompt buffer's name: how the apply handler learns what the prompt was for.
fn prompt_name(buffer: BufferId, key: SettingKey) -> String {
    format!("*llm-menu:{}:{}*", buffer.0, key.option_name())
}

fn parse_prompt_name(name: &str) -> Option<(BufferId, SettingKey)> {
    let inner = name.strip_prefix("*llm-menu:")?.strip_suffix('*')?;
    let (buffer, option) = inner.split_once(':')?;
    Some((
        BufferId(buffer.parse().ok()?),
        SettingKey::from_option_name(option)?,
    ))
}

fn arg_key(args: &Args) -> Option<SettingKey> {
    use lattice_grammar::args::ArgValue;
    let name = match args {
        Args::String(s) => s.as_str(),
        Args::List(items) => match items.first() {
            Some(ArgValue::String(s)) => s.as_str(),
            _ => return None,
        },
        _ => return None,
    };
    SettingKey::from_option_name(name)
}

/// What the prompt starts with: the choice already made in this scope.
fn initial_text(chats: &LlmChats, buffer: BufferId, key: SettingKey) -> String {
    let set = match chats.menu_scope(buffer) {
        MenuScope::Session => chats.session_overrides(buffer),
        MenuScope::NextRequest => chats.next_request(buffer),
    };
    set.get(&key).cloned().unwrap_or_default()
}

fn edit_handler(chats: LlmChatsHandle) -> ActionHandler {
    Arc::new(move |ctx: &ActionContext<'_>| {
        let key = arg_key(&ctx.args)?;
        let buffer = BufferId(ctx.buffer_id.0 as u32);
        if key == SettingKey::System {
            return Some(open_system(&chats, buffer, None));
        }
        let scope = chats.menu_scope(buffer);
        Some(Effect::OpenPrompt {
            prompt: format!("{} ({}): ", key.option_name(), scope_word(scope)),
            initial: initial_text(&chats, buffer, key),
            on_submit_action: ACTION_APPLY.to_string(),
            buffer_name: Some(prompt_name(buffer, key)),
        })
    })
}

fn apply_handler(chats: LlmChatsHandle) -> ActionHandler {
    Arc::new(move |ctx: &ActionContext<'_>| {
        let store = ctx.services.get::<BufferStoreHandle>()?;
        let prompt = BufferId(ctx.buffer_id.0 as u32);
        let (buffer, key) = parse_prompt_name(&store.name_for(prompt)?)?;
        Some(apply_choice(
            &chats,
            buffer,
            key,
            ctx.prompt_value.unwrap_or(""),
        ))
    })
}

fn scope_handler(chats: LlmChatsHandle) -> ActionHandler {
    Arc::new(move |ctx: &ActionContext<'_>| {
        let buffer = BufferId(ctx.buffer_id.0 as u32);
        let now = chats.toggle_menu_scope(buffer);
        // The menu closes when a row fires; reopen it where the user was.
        Some(Effect::Many(vec![
            echo(
                EchoLevel::Info,
                format!("llm menu: changes now go to the {}", scope_word(now)),
            ),
            Effect::OpenTransient {
                source: MENU_SOURCE.to_string(),
                args: Args::None,
            },
        ]))
    })
}

fn clear_handler(chats: LlmChatsHandle) -> ActionHandler {
    Arc::new(move |ctx: &ActionContext<'_>| {
        Some(clear_overrides(&chats, BufferId(ctx.buffer_id.0 as u32)))
    })
}

/// The handler bodies, contributed by `llm-chat-mode`.
pub fn handlers(chats: &LlmChatsHandle) -> Vec<ActionHandlerContribution> {
    vec![
        ActionHandlerContribution {
            action_name: ACTION_EDIT,
            handler: edit_handler(chats.clone()),
        },
        ActionHandlerContribution {
            action_name: ACTION_APPLY,
            handler: apply_handler(chats.clone()),
        },
        ActionHandlerContribution {
            action_name: ACTION_SCOPE,
            handler: scope_handler(chats.clone()),
        },
        ActionHandlerContribution {
            action_name: ACTION_CLEAR,
            handler: clear_handler(chats.clone()),
        },
    ]
}

/// Declare the action commands so the rows' ids resolve. The specs are shells;
/// the bodies are [`handlers`].
pub fn register_actions(registry: &mut CommandRegistry) {
    for (name, doc) in [
        (ACTION_EDIT, "llm-menu: change one setting."),
        (ACTION_APPLY, "llm-menu: apply the typed value."),
        (
            ACTION_SCOPE,
            "llm-menu: switch between session and next request.",
        ),
        (ACTION_CLEAR, "llm-menu: clear this session's overrides."),
    ] {
        registry.register_action(
            name,
            doc,
            ActionSpec {
                apply: Arc::new(|_| Ok(Effect::None)),
                args_schema: vec![],
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prompt_name_round_trips_its_buffer_and_key() {
        let name = prompt_name(BufferId(7), SettingKey::MaxTokens);
        assert_eq!(
            parse_prompt_name(&name),
            Some((BufferId(7), SettingKey::MaxTokens))
        );
        assert_eq!(parse_prompt_name("*scratch*"), None);
        assert_eq!(parse_prompt_name("*llm-menu:x:llm.model*"), None);
    }

    #[test]
    fn a_long_system_prompt_is_cut_for_the_menu() {
        let s = Settings {
            system: Some("a".repeat(80)),
            ..Settings::default()
        };
        let shown = display_value(&s, SettingKey::System);
        assert!(
            shown.ends_with('\u{2026}') && shown.chars().count() == 41,
            "{shown}"
        );
    }
}
