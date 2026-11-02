//! `llm-chat-mode`: the dedicated buffer of a native LLM chat.
//!
//! A transcript with a prompt (design §3): history is read-only, only the prompt
//! at the bottom is editable, and the insert-entry keys (`repl-mode`, implied)
//! jump to it. `<CR>` sends, `<C-j>` adds a newline, `<C-c>` interrupts, `gq`
//! closes. The structured conversation is the source of truth and the buffer is
//! its projection, so a reply streams in through the model and the shared drain
//! (`conversation::transcript`) without a keystroke.
//!
//! # Read-only, and why not `read-only-mode`
//!
//! The brief for this slice named `implies read-only-mode` for the two-declarations
//! rule. That mode's runner refuses every mutating operator in the buffer,
//! including in the prompt, which would make the prompt uneditable by `dd`/`cw`.
//! This mode follows `ai-conversation-mode` instead: `ReadOnly = true` (gates
//! typing) plus the editable-tail `invocation_runner` (gates operators to the
//! prompt). Together they are the two declarations.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use lattice_cells::{Cell, Headerline, HeaderlineRow};
use lattice_grammar::effect::Effect;
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerContribution, BufferStoreHandle, CapabilitySet,
    EditableTail, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    OptionOverrideSet, ReplMode, keymap_entry,
};

use crate::conversation::transcript::{
    self, PromptAnchors, TranscriptBinding, TranscriptGuard, editable_tail_runner, humanize,
    newline_handler, read_prompt,
};
use crate::conversation::{ConversationStore, ProjectionSource, SessionStatus};
use crate::llm::chat::{ChatSession, LlmChatsHandle, SendPlan};
use crate::llm::menu;
use crate::llm::settings::RequestOverrides;
use lattice_grammar::args::{ArgValue, Args};

/// The yes half of the confirm shown when `@profile` would widen tools.
const ACTION_SEND_WIDENED: &str = "action:llm-chat-send-widened";

/// Registered with the host so a key reaches [`ChatHeaderline`] even before the
/// buffer has content.
pub struct LlmChatMode {
    chats: LlmChatsHandle,
    anchors: PromptAnchors,
    headerline_version: Arc<AtomicU64>,
}

impl LlmChatMode {
    pub fn new(chats: LlmChatsHandle) -> Self {
        Self {
            chats,
            anchors: PromptAnchors::default(),
            headerline_version: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn mode_id() -> ModeId {
        ModeId::new("llm-chat-mode")
    }
}

/// The activation guard: the shared transcript guard, plus the chat's release.
/// Dropped when the buffer closes or its major changes, which cancels the
/// request in flight.
pub struct LlmChatGuard {
    _transcript: TranscriptGuard,
    chats: LlmChatsHandle,
    buffer: lattice_core::BufferId,
    generation: u64,
}

impl Drop for LlmChatGuard {
    fn drop(&mut self) {
        self.chats.release(self.buffer, self.generation);
    }
}

impl Mode for LlmChatMode {
    type Guard = Option<LlmChatGuard>;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }

    /// The transcript is markdown. This binds the grammar only: `markdown-mode`
    /// itself is not activated, so its link-following chords stay out of a
    /// buffer whose `<CR>` sends.
    fn target_language(&self) -> Option<&str> {
        Some("markdown")
    }

    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    /// The host registers its editable-tail runner under the ACP conversation
    /// mode's id; this mode shares it rather than adding a second registration
    /// for the same body.
    fn invocation_runner(&self) -> Option<ModeId> {
        Some(editable_tail_runner())
    }

    fn implies(&self) -> &[ModeId] {
        static IMPLIES: OnceLock<Vec<ModeId>> = OnceLock::new();
        IMPLIES.get_or_init(|| vec![ReplMode::mode_id()])
    }

    fn editable_tail(&self, buffer: lattice_core::BufferId) -> Option<EditableTail> {
        self.anchors.editable_tail(buffer)
    }

    fn keymap(&self) -> Keymap {
        Keymap::from_entries(keymap_entries())
    }

    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        // The menu's handler bodies are contributed here, by the mode that owns
        // the chat; they act on whatever buffer the menu was opened over.
        let mut handlers = menu::handlers(&self.chats);
        handlers.extend([
            ActionHandlerContribution {
                action_name: "action:llm-chat-send",
                handler: send_handler(self.chats.clone(), self.anchors.clone()),
            },
            ActionHandlerContribution {
                action_name: "action:llm-chat-newline",
                handler: newline_handler(),
            },
            ActionHandlerContribution {
                action_name: "action:llm-chat-interrupt",
                handler: interrupt_handler(self.chats.clone()),
            },
            ActionHandlerContribution {
                action_name: "action:llm-chat-close",
                handler: Arc::new(|_| Some(Effect::KillBuffer)),
            },
            ActionHandlerContribution {
                action_name: ACTION_SEND_WIDENED,
                handler: send_widened_handler(self.chats.clone()),
            },
            ActionHandlerContribution {
                action_name: "action:llm-chat-menu",
                handler: Arc::new(|_| {
                    Some(Effect::OpenTransient {
                        source: menu::MENU_SOURCE.to_string(),
                        args: Args::None,
                    })
                }),
            },
        ]);
        handlers
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(None);
            };
            let (Some(handle), Some(name)) =
                (store.handle_for(buffer_id), store.name_for(buffer_id))
            else {
                return Ok(None);
            };
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                return Ok(None);
            };
            // The chat is made by `:llm` before the buffer opens, keyed by the
            // buffer's name. No chat means this is not a buffer `:llm` made.
            let Some((session, generation)) = self.chats.bind(&name, buffer_id) else {
                return Ok(None);
            };
            let headerline: Arc<dyn Headerline> = Arc::new(ChatHeaderline {
                session: session.clone(),
                version: self.headerline_version.clone(),
            });
            let transcript = transcript::bind(TranscriptBinding {
                ctx: &ctx,
                buffer_id,
                handle,
                store: session.store.clone(),
                anchors: self.anchors.clone(),
                headerline: Some(headerline),
                headerline_version: self.headerline_version.clone(),
                only_session: Some(session.key.clone()),
                runtime,
                projection: ProjectionSource::new({
                    let chats = self.chats.clone();
                    move || chats.projection_options_for(buffer_id)
                }),
            })
            .await;
            Ok(Some(LlmChatGuard {
                _transcript: transcript,
                chats: self.chats.clone(),
                buffer: buffer_id,
                generation,
            }))
        })
    }
}

fn keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! {
                mode: Insert, chord: "<CR>",
                doc: "llm-chat: send the prompt",
                cmd: "action:llm-chat-send"
            },
            // `<CR>` is taken by send, so a newline needs its own chord, as in
            // `:opencode-acp` and the terminal buffers.
            keymap_entry! {
                mode: Insert, chord: "<C-j>",
                doc: "llm-chat: insert a newline in the prompt",
                cmd: "action:llm-chat-newline"
            },
            keymap_entry! {
                mode: Insert, chord: "<C-c>",
                doc: "llm-chat: interrupt the reply",
                cmd: "action:llm-chat-interrupt"
            },
            keymap_entry! {
                mode: Normal, chord: "gq",
                doc: "llm-chat: close the chat",
                cmd: "action:llm-chat-close"
            },
            keymap_entry! {
                mode: Normal, chord: "<M-m>",
                doc: "llm-chat: open the settings menu",
                cmd: "action:llm-chat-menu"
            },
            keymap_entry! {
                mode: Insert, chord: "<M-m>",
                doc: "llm-chat: open the settings menu",
                cmd: "action:llm-chat-menu"
            },
        ]
    })
}

/// `<CR>`: send the prompt, or queue it behind the reply that is streaming.
/// Returns nothing: the prompt is cleared and the caret re-parked by the drain's
/// re-projection when the user turn (or the queued row) lands, not by an edit
/// here that would race it. An empty prompt, or a buffer with no seeded anchor,
/// sends nothing.
fn send_handler(chats: LlmChatsHandle, anchors: PromptAnchors) -> ActionHandler {
    Arc::new(move |ctx: &ActionContext<'_>| -> Option<Effect> {
        let buffer = lattice_core::BufferId(ctx.buffer_id.0 as u32);
        let session = chats.for_buffer(buffer)?;
        let prompt = read_prompt(ctx, &anchors)?;
        // The whole prompt here is what the user typed, so it is what is scanned.
        match chats.plan_send(buffer, &prompt) {
            SendPlan::Plain => {
                chats.submit(&session, prompt);
                None
            }
            SendPlan::Profile(profile) => {
                chats.submit_with(
                    &session,
                    prompt,
                    RequestOverrides {
                        profile: Some(profile),
                        pasted_profile: true,
                        ..RequestOverrides::default()
                    },
                );
                None
            }
            // Nothing is sent and the prompt stays in the buffer, so a no
            // loses nothing; only a yes sends it.
            SendPlan::NeedsConfirm(profile) => Some(Effect::Confirm {
                prompt: format!(
                    "@{profile} enables tools this session does not have. Send this one request with it?"
                ),
                yes_action: ACTION_SEND_WIDENED.to_string(),
                args: Args::List(vec![ArgValue::String(profile), ArgValue::String(prompt)]),
            }),
        }
    })
}

/// The yes half of the widening confirm. The profile and the prompt travel in
/// the confirm's args, so the thing confirmed is the thing sent.
fn send_widened_handler(chats: LlmChatsHandle) -> ActionHandler {
    Arc::new(move |ctx: &ActionContext<'_>| -> Option<Effect> {
        let Args::List(items) = &ctx.args else {
            return None;
        };
        let text = |v: Option<&ArgValue>| match v {
            Some(ArgValue::String(s) | ArgValue::Raw(s)) => Some(s.clone()),
            _ => None,
        };
        let (profile, prompt) = (text(items.first())?, text(items.get(1))?);
        let session = chats.for_buffer(lattice_core::BufferId(ctx.buffer_id.0 as u32))?;
        chats.submit_with(
            &session,
            prompt,
            RequestOverrides {
                profile: Some(profile),
                // The user said yes: full trust, for this request only.
                pasted_profile: false,
                ..RequestOverrides::default()
            },
        );
        None
    })
}

fn interrupt_handler(chats: LlmChatsHandle) -> ActionHandler {
    Arc::new(move |ctx: &ActionContext<'_>| -> Option<Effect> {
        if let Some(session) = chats.for_buffer(lattice_core::BufferId(ctx.buffer_id.0 as u32)) {
            chats.interrupt(&session);
        }
        None
    })
}

/// Declare the chords' action commands so the keymap's `cmd` names resolve. The
/// specs are shells; the bodies are [`LlmChatMode::action_handlers`].
pub fn register_actions(registry: &mut lattice_grammar::CommandRegistry) {
    use lattice_grammar::registry::ActionSpec;
    for (name, doc) in [
        ("action:llm-chat-send", "llm-chat: send the prompt."),
        (
            "action:llm-chat-newline",
            "llm-chat: insert a newline in the prompt.",
        ),
        (
            "action:llm-chat-interrupt",
            "llm-chat: interrupt the reply.",
        ),
        ("action:llm-chat-close", "llm-chat: close the chat."),
        ("action:llm-chat-menu", "llm-chat: open the settings menu."),
        (
            ACTION_SEND_WIDENED,
            "llm-chat: send the confirmed prompt with the profile that widens tools.",
        ),
    ] {
        registry.register_action(
            name,
            doc,
            ActionSpec {
                apply: Arc::new(|_| Ok(Effect::None)),
                // A confirm's args travel through the dialog's transient state,
                // keyed by the schema: a value with no slot is dropped. The
                // widening confirm carries two, so it declares two.
                args_schema: if name == ACTION_SEND_WIDENED {
                    ["profile", "prompt"].map(string_slot).to_vec()
                } else {
                    vec![]
                },
            },
        );
    }
}

fn string_slot(name: &'static str) -> lattice_grammar::ArgSpec {
    lattice_grammar::ArgSpec {
        name: name.into(),
        kind: lattice_grammar::ArgKind::String,
        doc: "carried by the confirm that asked".into(),
        prompt: "".into(),
        default: lattice_grammar::ArgDefault::None,
        completion: None,
        picker: None,
    }
}

/// `model │ state │ N tokens │ $cost`, plus `⌛ N queued` when prompts wait.
pub(crate) struct ChatHeaderline {
    session: Arc<ChatSession>,
    version: Arc<AtomicU64>,
}

impl ChatHeaderline {
    pub(crate) fn text(store: &ConversationStore, model: Option<&str>) -> String {
        use std::fmt::Write;
        let snap = store.snapshot();
        let mut text = String::new();
        text.push_str(model.unwrap_or("no model"));
        let _ = write!(text, " \u{2502} {}", snap.status);
        if snap.status == SessionStatus::Idle && snap.error.is_some() {
            text.push_str(" (failed)");
        }
        if !snap.queued.is_empty() {
            let _ = write!(text, " \u{231B} {} queued", snap.queued.len());
        }
        if let Some(usage) = &snap.usage {
            let _ = write!(text, " \u{2502} {} tokens", humanize(usage.used));
            if let Some(cost) = &usage.cost {
                let _ = write!(text, " \u{00B7} ${:.4}", cost.amount);
            }
        }
        text
    }
}

impl Headerline for ChatHeaderline {
    fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    fn render(&self) -> Option<HeaderlineRow> {
        let text = Self::text(&self.session.store, self.session.model().as_deref());
        let cells: Arc<[Cell]> = text
            .chars()
            .map(|c| Cell::new(c as u32, 0, 0, 0))
            .collect::<Vec<_>>()
            .into();
        Some(HeaderlineRow { cells, bg: None })
    }
}
