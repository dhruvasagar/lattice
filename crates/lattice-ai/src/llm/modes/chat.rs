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

/// `gf` on the text an ask sent: go back to where it came from.
/// The chat prompt's send action (`<CR>`, and `:llm-send` in a chat).
pub const ACTION_CHAT_SEND: &str = "action:llm-chat-send";

const ACTION_GOTO_SOURCE: &str = "action:llm-chat-goto-source";

/// The yes half of the confirm shown when `@profile` would widen tools.
const ACTION_SEND_WIDENED: &str = "action:llm-chat-send-widened";
const ACTION_TRUST: &str = "action:llm-chat-trust";
/// `gb` on a past turn: branch a new chat from it (LL.19).
const ACTION_FORK: &str = "action:llm-chat-fork";

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

    /// Where each chat buffer's prompt starts, shared with `:llm-inspect`,
    /// which reads the prompt to show what sending it would send.
    pub fn anchors(&self) -> PromptAnchors {
        self.anchors.clone()
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
                action_name: ACTION_CHAT_SEND,
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
                action_name: ACTION_GOTO_SOURCE,
                handler: goto_source_handler(self.chats.clone()),
            },
            ActionHandlerContribution {
                action_name: ACTION_SEND_WIDENED,
                handler: send_widened_handler(self.chats.clone()),
            },
            ActionHandlerContribution {
                action_name: ACTION_FORK,
                handler: {
                    let chats = self.chats.clone();
                    Arc::new(move |ctx: &ActionContext<'_>| {
                        Some(fork_effect(
                            &chats,
                            lattice_core::BufferId(ctx.buffer_id.0 as u32),
                            ctx.cursor.line,
                        ))
                    })
                },
            },
            ActionHandlerContribution {
                action_name: ACTION_TRUST,
                handler: trust_handler(self.chats.clone(), self.headerline_version.clone()),
            },
            ActionHandlerContribution {
                action_name: "action:llm-chat-menu",
                handler: Arc::new({
                    let chats = self.chats.clone();
                    move |ctx: &ActionContext<'_>| {
                        chats.disarm_menu_ask(lattice_core::BufferId(ctx.buffer_id.0 as u32));
                        Some(Effect::OpenTransient {
                            source: menu::MENU_SOURCE.to_string(),
                            args: Args::None,
                        })
                    }
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
            // The layer's runtime: activation is polled on the editor thread,
            // whose own runtime would run the transcript drain (O(transcript)
            // per burst) beside keystrokes.
            let runtime = self.chats.runtime().clone();
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
                prompt: session.take_prompt_seed(),
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
            // vim's `gf` edits the file under the cursor in this window; on the
            // text an ask sent, the file is where that text came from.
            keymap_entry! {
                mode: Normal, chord: "gf",
                doc: "llm-chat: go to where the sent text came from",
                cmd: "action:llm-chat-goto-source"
            },
            // A branch of the conversation: the chat up to the question under
            // the cursor, in a new buffer, with that question to edit. This
            // chat is left as it is.
            keymap_entry! {
                mode: Normal, chord: "gb",
                doc: "llm-chat: fork a new chat from the question under the cursor",
                cmd: "action:llm-chat-fork"
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
            // Both modes: the prompt is typed in Insert, and a review is
            // decided from there as often as from Normal.
            keymap_entry! {
                mode: Normal, chord: "<C-t>",
                doc: "llm-chat: trust this chat's edits (apply without review), or stop",
                cmd: "action:llm-chat-trust"
            },
            keymap_entry! {
                mode: Insert, chord: "<C-t>",
                doc: "llm-chat: trust this chat's edits (apply without review), or stop",
                cmd: "action:llm-chat-trust"
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

/// `gf`: on a source block, open its buffer in this window at its first line,
/// as vim's `gf` opens a file. Off one, nothing happens.
fn goto_source_handler(chats: LlmChatsHandle) -> ActionHandler {
    Arc::new(move |ctx: &ActionContext<'_>| -> Option<Effect> {
        let buffer = lattice_core::BufferId(ctx.buffer_id.0 as u32);
        let session = chats.for_buffer(buffer)?;
        let conv = session.store.snapshot();
        let opts = chats.projection_options_for(buffer);
        let link = crate::conversation::projection::source_at_line(&conv, &opts, ctx.cursor.line)?;
        Some(Effect::Many(vec![
            Effect::FocusBuffer(link.buffer.0),
            Effect::CursorMoveIn {
                target: link.buffer,
                position: lattice_protocol::position::Position::new(link.line, 0),
            },
        ]))
    })
}

/// `gb` and `:llm-fork`: open a new chat from the question the line `line` of
/// chat `buffer` belongs to. Off a turn (on the prompt, say) it says so.
pub fn fork_effect(chats: &LlmChatsHandle, buffer: lattice_core::BufferId, line: u32) -> Effect {
    let error = |text: String| Effect::Echo {
        level: lattice_grammar::effect::EchoLevel::Error,
        text,
    };
    let Some(session) = chats.for_buffer(buffer) else {
        return error("llm: not a chat buffer".into());
    };
    let conv = session.store.snapshot();
    let opts = chats.projection_options_for(buffer);
    let Some(turn) = crate::conversation::projection::turn_at_line(&conv, &opts, line) else {
        return error("llm: put the cursor on a past turn to fork from it".into());
    };
    match chats.fork(buffer, turn) {
        Ok(fork) => Effect::OpenSyntheticBuffer {
            name: fork.name.clone(),
            mode_id: LlmChatMode::mode_id().as_str().to_string(),
            content: None,
            cursor: None,
            activate_minor: None,
        },
        Err(e) => error(e),
    }
}

/// `<C-t>`: flip this chat's trust. Trusted, its edit tools apply without a
/// diff; the headerline says so for as long as it lasts.
fn trust_handler(chats: LlmChatsHandle, headerline: Arc<AtomicU64>) -> ActionHandler {
    Arc::new(move |ctx: &ActionContext<'_>| -> Option<Effect> {
        let session = chats.for_buffer(lattice_core::BufferId(ctx.buffer_id.0 as u32))?;
        let on = session.toggle_trust();
        headerline.fetch_add(1, Ordering::Release);
        Some(Effect::Echo {
            level: lattice_grammar::effect::EchoLevel::Info,
            text: if on {
                "llm: this chat's edits now apply without review (<C-t> to stop)".into()
            } else {
                "llm: this chat's edits go to review again".into()
            },
        })
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
        (ACTION_CHAT_SEND, "llm-chat: send the prompt."),
        (
            "action:llm-chat-newline",
            "llm-chat: insert a newline in the prompt.",
        ),
        (
            "action:llm-chat-interrupt",
            "llm-chat: interrupt the reply.",
        ),
        ("action:llm-chat-close", "llm-chat: close the chat."),
        (
            ACTION_GOTO_SOURCE,
            "llm-chat: go to where the text an ask sent came from.",
        ),
        ("action:llm-chat-menu", "llm-chat: open the settings menu."),
        (
            ACTION_TRUST,
            "llm-chat: trust this chat's edits, applying them without review; again to stop.",
        ),
        (
            ACTION_FORK,
            "llm-chat: open a new chat with the history before the question under the cursor, \
             that question ready to edit in its prompt.",
        ),
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
    pub(crate) fn text(store: &ConversationStore, model: Option<&str>, trusted: bool) -> String {
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
        if trusted {
            text.push_str(" \u{2502} edits trusted");
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
        let text = Self::text(
            &self.session.store,
            self.session.model().as_deref(),
            self.session.trusted(),
        );
        let cells: Arc<[Cell]> = text
            .chars()
            .map(|c| Cell::new(c as u32, 0, 0, 0))
            .collect::<Vec<_>>()
            .into();
        Some(HeaderlineRow { cells, bg: None })
    }
}
