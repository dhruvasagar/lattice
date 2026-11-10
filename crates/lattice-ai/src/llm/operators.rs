//! The ask operator (`<leader>aa{motion}`) and `:llm-ask` (LL.10b).
//!
//! One request (design §2): the text a motion, text object or Visual selection
//! covers, plus an instruction, sent to a **new dedicated buffer** that opens
//! beside the source without taking focus. Both front-ends end in
//! [`ask_effect`]; they differ only in where the instruction comes from.
//!
//! - The operator captures the text when it runs, so an edit made while the
//!   instruction is being typed does not change what is asked about, then
//!   opens a one-line prompt. Its submit action finds the captured text again
//!   through the prompt buffer's name (`*llm-ask:<token>*`), the way magit's
//!   prompts carry their target. `<Esc>` fires nothing, so nothing is sent.
//! - `:'<,'>llm-ask [key=value…] instruction` takes the instruction from its
//!   arguments, for scripts and bindings.
//!
//! **The operator does not mutate,** so a read-only source (`*compilation*`,
//! `*messages*`, a help page) runs it. **A blockwise selection collapses to one
//! span** (`blockwise_per_row: false`): an ask is about the text, not per row.
//!
//! LL.10c adds the edges every ask passes through, in this order:
//!
//! - **`llm.exclude`** refuses a source whose file matches, by name, before the
//!   prompt opens, so no instruction is typed for nothing.
//! - **`<leader>aA`** is the same operator opening `:llm-menu` first: the menu
//!   holds the ask and gains an `a` row that opens the instruction prompt, so
//!   choices made in it (next-request scope, or the session) reach this ask.
//! - **An oversized source asks first** ([`ASK_CONFIRM_BYTES`]): a yes sends it,
//!   a no sends nothing.
//! - **`destination=<buffer>`** continues that chat instead of opening a new
//!   one. A kind not built yet (`review`) opens a new buffer and says so
//!   rather than refuse.
//!
//! LL.13b builds **`destination=below`**: the answer streams into the source
//! buffer under the span ([`super::sink`]). A read-only or composed source
//! cannot take it, so the answer opens in a new buffer and the echo says so.
//!
//! LL.14a builds **`destination=echo`** and **`destination=register`**: the
//! complete reply goes to the echo area or a register. The register is the
//! one typed before the operator (`"a<leader>aaj`), the unnamed register
//! otherwise. **A request that may use tools needs a chat** (design §5): aimed
//! at `below`, `echo` or a register, it opens in a new buffer and says so,
//! because a tool call has nowhere to be shown or approved outside one.
//!
//! **`.` does not repeat an ask** (design §11, decided here). `.` repeats the
//! last change, and an ask changes nothing, as `y` changes nothing; making it
//! repeatable would also re-send a prompt the user typed once. A macro records
//! the invocations and replays them; the request is asynchronous, so the macro
//! never waits for it.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use lattice_core::BufferId;
use lattice_grammar::CommandError;
use lattice_grammar::effect::{EchoLevel, Effect};
use lattice_grammar::registry::{
    ActionSpec, CommandRegistry, ExCommandContext, OperatorContext, OperatorId, OperatorSpec,
};
use lattice_mode::{ActionContext, ActionHandler, BufferStoreHandle};
use lattice_protocol::position::Range;

use lattice_grammar::args::{ArgValue, Args};

use super::chat::{LlmChats, LlmChatsHandle, SendPlan};
use super::commands::parse_request_args;
use super::menu::MENU_SOURCE;
use super::modes::chat::LlmChatMode;
use super::settings::{RawSet, RequestOverrides, SettingKey, is_buffer_name, parse_tools};
use super::sink::Quiet;
use crate::conversation::{Block, SourceLink};

pub const ASK_OPERATOR: &str = "operator:llm-ask";
/// The ask operator that opens `:llm-menu` before the instruction prompt.
pub const ASK_MENU_OPERATOR: &str = "operator:llm-ask-menu";
/// The instruction prompt's submit action.
pub const ACTION_ASK_SEND: &str = "action:llm-ask-send";
/// The menu row that moves a held ask on to its instruction prompt.
pub const ACTION_ASK_CONTINUE: &str = "action:llm-ask-continue";
/// The yes half of the oversized-source confirm.
pub const ACTION_ASK_CONFIRMED: &str = "action:llm-ask-confirmed";

/// A source larger than this asks before it is sent (about 8k tokens). Large
/// enough that no paragraph, function or file of ordinary size meets it; small
/// enough that a `ggVG` over a generated file does not go out unasked.
pub const ASK_CONFIRM_BYTES: usize = 32 * 1024;

/// Asks waiting for their instruction. A prompt cancelled with `<Esc>` leaves
/// its ask behind, so the oldest are dropped past this many.
const PENDING_CAP: usize = 16;

/// What an ask is about, captured when the operator ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskSource {
    pub buffer: BufferId,
    pub range: Range,
    pub text: String,
    /// The register typed before the operator, for `destination=register`.
    pub register: lattice_grammar::Register,
}

/// Asks the operator captured, until their prompt is submitted.
#[derive(Default)]
pub struct LlmAsks {
    next: Mutex<u64>,
    pending: Mutex<VecDeque<(u64, AskSource)>>,
    /// Where `destination=below` answers stream from (LL.13b); unset in a
    /// harness without one, which then opens a new buffer instead.
    inline: std::sync::OnceLock<super::sink::LlmInlineHandle>,
}

pub type LlmAsksHandle = Arc<LlmAsks>;

impl LlmAsks {
    /// Give `destination=below` its writer. Set once, at install.
    pub fn set_inline(&self, inline: super::sink::LlmInlineHandle) {
        let _ = self.inline.set(inline);
    }

    /// Hold `source` and return the token its prompt carries.
    pub fn hold(&self, source: AskSource) -> u64 {
        let token = {
            let mut next = self.next.lock().unwrap_or_else(|e| e.into_inner());
            *next += 1;
            *next
        };
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.push_back((token, source));
        while pending.len() > PENDING_CAP {
            pending.pop_front();
        }
        token
    }

    /// Take the ask `token` names, once.
    pub fn take(&self, token: u64) -> Option<AskSource> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let i = pending.iter().position(|(t, _)| *t == token)?;
        pending.remove(i).map(|(_, s)| s)
    }

    /// Whether ask `token` is still held.
    pub fn holds(&self, token: u64) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|(t, _)| *t == token)
    }

    /// How many asks wait for an instruction.
    #[doc(hidden)]
    pub fn pending_count(&self) -> usize {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

/// The prompt buffer's name for ask `token`.
pub fn prompt_buffer_name(token: u64) -> String {
    format!("*llm-ask:{token}*")
}

fn token_of(name: &str) -> Option<u64> {
    name.strip_prefix("*llm-ask:")?
        .strip_suffix('*')?
        .parse()
        .ok()
}

/// The text `range` covers in `ctx`'s document. A linewise span ends at the
/// next line's start, so its final newline is not part of what was asked about.
pub(crate) fn captured(ctx: &OperatorContext) -> Option<AskSource> {
    let mut text = ctx.document.buffer().slice(ctx.range).ok()?;
    if ctx.linewise && text.ends_with('\n') {
        text.pop();
    }
    if text.trim().is_empty() {
        return None;
    }
    Some(AskSource {
        buffer: ctx.buffer_id,
        range: ctx.range,
        text,
        register: ctx.register,
    })
}

/// The one-line prompt that takes ask `token`'s instruction.
fn instruction_prompt(token: u64, lines: &str) -> Effect {
    Effect::OpenPrompt {
        prompt: format!("Ask about {lines}: "),
        initial: String::new(),
        on_submit_action: ACTION_ASK_SEND.to_string(),
        buffer_name: Some(prompt_buffer_name(token)),
    }
}

pub(crate) fn echo(level: EchoLevel, text: String) -> Effect {
    Effect::Echo { level, text }
}

/// Why `buffer`'s text may not be sent, if `llm.exclude` names its file.
pub(crate) fn refusal(
    chats: &LlmChats,
    store: Option<&BufferStoreHandle>,
    buffer: BufferId,
) -> Option<Effect> {
    if let Some(why) = chats.composed_refusal(buffer) {
        return Some(echo(
            EchoLevel::Error,
            format!("llm: {why}; nothing was sent"),
        ));
    }
    let path = store?.path_for(buffer)?;
    let glob = chats.excluded(&path)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    Some(echo(
        EchoLevel::Error,
        format!("llm: {name} matches llm.exclude (`{glob}`); nothing was sent"),
    ))
}

/// Why `destination=below` cannot write into `buffer`, if it cannot: the
/// answer would be text the user cannot edit (a read-only buffer), or there
/// is no single text to put it in (a composed view of several files).
fn below_refusal(
    chats: &LlmChats,
    store: Option<&BufferStoreHandle>,
    buffer: BufferId,
    has_writer: bool,
) -> Option<&'static str> {
    if !has_writer {
        return Some("destination=below is not set up here");
    }
    if chats.is_read_only(buffer) {
        return Some("the buffer is read-only");
    }
    let handle = store.and_then(|s| s.handle_for(buffer))?;
    if handle.display_line_numbers().is_some() {
        return Some("this view shows several files");
    }
    None
}

/// Whether this request may call tools: its own `tools=`, else the source
/// session's. Tools reach the model only through a chat (design §5).
fn may_use_tools(chats: &LlmChats, buffer: BufferId, values: &RawSet) -> bool {
    match values.get(&SettingKey::Tools) {
        Some(text) => parse_tools(text).is_some_and(|t| !t.is_empty()),
        None => chats
            .resolve_for(buffer)
            .is_ok_and(|r| !r.values.tools.is_empty()),
    }
}

/// Capture the span, or say why there is nothing to ask about.
fn capture_for_ask(
    chats: &LlmChats,
    store: &BufferStoreHandle,
    ctx: &OperatorContext,
) -> Result<AskSource, Effect> {
    if let Some(refused) = refusal(chats, Some(store), ctx.buffer_id) {
        return Err(refused);
    }
    captured(ctx).ok_or_else(|| {
        echo(
            EchoLevel::Info,
            "llm: nothing to ask about (the span is empty)".to_string(),
        )
    })
}

fn operator_ask(
    chats: &LlmChats,
    asks: &LlmAsks,
    store: &BufferStoreHandle,
    ctx: &mut OperatorContext,
) -> Result<Effect, CommandError> {
    let source = match capture_for_ask(chats, store, ctx) {
        Ok(source) => source,
        Err(why) => return Ok(why),
    };
    let lines = line_span(source.range);
    let token = asks.hold(source);
    Ok(instruction_prompt(token, &lines))
}

/// `<leader>aA`: hold the ask and open the menu over the source, armed with it.
fn operator_ask_menu(
    chats: &LlmChats,
    asks: &LlmAsks,
    store: &BufferStoreHandle,
    ctx: &mut OperatorContext,
) -> Result<Effect, CommandError> {
    let source = match capture_for_ask(chats, store, ctx) {
        Ok(source) => source,
        Err(why) => return Ok(why),
    };
    let buffer = source.buffer;
    let lines = line_span(source.range);
    let token = asks.hold(source);
    chats.arm_menu_ask(buffer, token, lines);
    Ok(Effect::OpenTransient {
        source: MENU_SOURCE.to_string(),
        args: Args::None,
    })
}

/// The 1-based lines `range` covers: `3`, or `3-10`. A span ending at a line's
/// start (linewise) does not include that line.
pub(crate) fn line_span(range: Range) -> String {
    let first = range.start.line;
    let last = if range.end.byte == 0 && range.end.line > first {
        range.end.line - 1
    } else {
        range.end.line
    };
    if last == first {
        format!("line {}", first + 1)
    } else {
        format!("lines {}-{}", first + 1, last + 1)
    }
}

/// `foo.rs:3` or `foo.rs:3-10`, and the name hint (`foo.rs`), for `source`.
/// A file is named by its file name; any other buffer by its buffer name.
pub(crate) fn label_of(
    store: Option<&BufferStoreHandle>,
    source: &AskSource,
) -> (String, String, String) {
    let path = store.and_then(|s| s.path_for(source.buffer));
    let name = path
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .or_else(|| store.and_then(|s| s.name_for(source.buffer)))
        .unwrap_or_else(|| "buffer".to_string());
    let lines = line_span(source.range);
    let at = lines
        .trim_start_matches("lines ")
        .trim_start_matches("line ");
    let lang = lattice_core::language::language_id_for_path(path.as_deref());
    let lang = if lang.is_empty() {
        "text".to_string()
    } else {
        lang
    };
    (format!("{name}:{at}"), name, lang)
}

/// Split a typed instruction into its leading `key=value` overrides and the
/// text, and decide a typed `@profile`. Only the typed text is scanned, never
/// the source. A profile that widens tools is refused: an ask or a rewrite has
/// no prompt left to confirm into once it is sent.
pub(crate) fn request_args(
    chats: &LlmChats,
    buffer: BufferId,
    instruction: &str,
) -> Result<(super::settings::RawSet, String, RequestOverrides), Effect> {
    let (args, typed) = parse_request_args(instruction).map_err(|e| echo(EchoLevel::Error, e))?;
    let mut overrides = RequestOverrides::default();
    match chats.plan_send(buffer, &typed) {
        SendPlan::Plain => {}
        SendPlan::Profile(p) => {
            overrides.profile = Some(p);
            overrides.pasted_profile = true;
        }
        SendPlan::NeedsConfirm(p) => {
            return Err(echo(
                EchoLevel::Error,
                format!(
                    "llm: @{p} enables tools, which this request cannot confirm; open a chat with :llm {p}"
                ),
            ));
        }
    }
    Ok((args, typed, overrides))
}

/// Send `source` with the typed `instruction` (leading `key=value` words are
/// this request's overrides) to a new dedicated buffer shown beside the source,
/// or to the chat `destination` names.
///
/// The new chat copies the source buffer's session settings and any menu choice
/// waiting for its next request; nothing stays linked (design §4). Only the
/// typed instruction is scanned for `@profile`, never the source text. A source
/// over [`ASK_CONFIRM_BYTES`] is held and confirmed first unless `confirmed`.
pub fn ask_effect(
    chats: &Arc<LlmChats>,
    asks: &LlmAsks,
    store: Option<&BufferStoreHandle>,
    source: AskSource,
    instruction: &str,
    confirmed: bool,
) -> Effect {
    if let Some(refused) = refusal(chats, store, source.buffer) {
        return refused;
    }
    let (args, typed, mut overrides) = match request_args(chats, source.buffer, instruction) {
        Ok(parsed) => parsed,
        Err(why) => return why,
    };
    let (label, hint, lang) = label_of(store, &source);
    if !confirmed && source.text.len() > ASK_CONFIRM_BYTES {
        let size = source.text.len();
        let token = asks.hold(source);
        return Effect::Confirm {
            prompt: format!(
                "Send {} KB from {label} (about {}k tokens)?",
                size.div_ceil(1024),
                size.div_ceil(4000)
            ),
            yes_action: ACTION_ASK_CONFIRMED.to_string(),
            args: Args::List(vec![
                ArgValue::String(token.to_string()),
                ArgValue::String(instruction.to_string()),
            ]),
        };
    }
    let mut values = chats.next_request(source.buffer);
    values.extend(args);
    // Where it goes: this request's `destination=`, else the source session's.
    let destination = values.get(&SettingKey::Destination).cloned().or_else(|| {
        chats
            .resolve_for(source.buffer)
            .ok()
            .and_then(|r| r.values.destination)
    });
    let mut note = None;
    let mut below = None;
    let mut quiet = None;
    let in_place = matches!(destination.as_deref(), Some("below" | "echo" | "register"));
    let target = match destination.as_deref() {
        Some(kind) if in_place && may_use_tools(chats, source.buffer, &values) => {
            note = Some(format!(
                "llm: this request may use tools, which need a chat; destination={kind} \
                 opened a new buffer instead"
            ));
            None
        }
        Some(name) if is_buffer_name(name) => {
            let chat = store
                .and_then(|s| s.find_by_name(name))
                .and_then(|id| chats.for_buffer(id));
            match chat {
                Some(chat) => Some(chat),
                None => {
                    return echo(
                        EchoLevel::Error,
                        format!("llm: destination {name} is not an open chat"),
                    );
                }
            }
        }
        Some("below") => {
            match below_refusal(chats, store, source.buffer, asks.inline.get().is_some()) {
                Some(why) => note = Some(format!("llm: {why}; the answer opened in a new buffer")),
                None => below = asks.inline.get().cloned(),
            }
            None
        }
        Some(kind @ ("echo" | "register")) => {
            match asks.inline.get() {
                Some(_) => {
                    quiet = Some(if kind == "echo" {
                        Quiet::Echo
                    } else {
                        Quiet::Register(source.register)
                    });
                }
                None => {
                    note = Some(format!(
                        "llm: destination={kind} is not set up here; the answer opened in a new buffer"
                    ));
                }
            }
            None
        }
        Some(kind @ "review") => {
            note = Some(format!(
                "llm: destination={kind} is not built yet; the answer opened in a new buffer"
            ));
            None
        }
        _ => None,
    };
    values.remove(&SettingKey::Destination);
    chats.clear_next_request(source.buffer);
    overrides.values = values;

    let block = Block::Source {
        label,
        lang,
        text: source.text.clone(),
        link: Some(SourceLink {
            buffer: source.buffer,
            line: source.range.start.line,
        }),
    };
    if let (Some(quiet), Some(inline)) = (quiet, asks.inline.get()) {
        return inline.start_quiet(
            chats,
            source.buffer,
            &hint,
            block,
            typed.trim().to_string(),
            overrides,
            quiet,
        );
    }
    if let (Some(inline), Some(store)) = (below, store) {
        return inline.start(
            chats,
            store,
            &source,
            &hint,
            block,
            typed.trim().to_string(),
            overrides,
        );
    }
    let session = target.unwrap_or_else(|| chats.create_for_source(source.buffer, &hint));
    chats.submit_source(&session, block, typed.trim().to_string(), overrides);
    let display = Effect::DisplaySyntheticBuffer {
        name: session.name.clone(),
        mode_id: LlmChatMode::mode_id().as_str().to_string(),
        content: None,
        split: lattice_core::ui::pane::SplitOrientation::Vertical,
    };
    match note {
        Some(text) => Effect::Many(vec![display, echo(EchoLevel::Info, text)]),
        None => display,
    }
}

pub fn send_handler(chats: LlmChatsHandle, asks: LlmAsksHandle) -> ActionHandler {
    Arc::new(move |ctx: &ActionContext<'_>| -> Option<Effect> {
        let store = ctx.services.get::<BufferStoreHandle>();
        let prompt = BufferId(ctx.buffer_id.0 as u32);
        let token = store
            .as_ref()
            .and_then(|s| s.name_for(prompt))
            .and_then(|n| token_of(&n))?;
        let Some(source) = asks.take(token) else {
            return Some(Effect::Echo {
                level: EchoLevel::Error,
                text: "llm: that ask was dropped; ask again".to_string(),
            });
        };
        let instruction = ctx.prompt_value.unwrap_or_default();
        Some(ask_effect(
            &chats,
            &asks,
            store.as_deref(),
            source,
            instruction,
            false,
        ))
    })
}

/// The menu's `a` row: move the ask or rewrite the menu holds on to its
/// instruction prompt.
pub fn continue_handler(
    chats: LlmChatsHandle,
    asks: LlmAsksHandle,
    rewrites: super::rewrite::LlmRewritesHandle,
) -> ActionHandler {
    Arc::new(move |ctx: &ActionContext<'_>| -> Option<Effect> {
        let buffer = BufferId(ctx.buffer_id.0 as u32);
        let Some(held) = chats.disarm_menu_ask(buffer) else {
            return Some(echo(
                EchoLevel::Error,
                "llm: nothing is waiting; use <leader>aA or <leader>aR again".to_string(),
            ));
        };
        match held.kind {
            super::chat::MenuHold::Ask if asks.holds(held.token) => {
                Some(instruction_prompt(held.token, &held.lines))
            }
            super::chat::MenuHold::Rewrite if rewrites.holds(held.token) => {
                Some(super::rewrite::instruction_prompt(held.token, &held.lines))
            }
            _ => Some(echo(
                EchoLevel::Error,
                "llm: that request was dropped; start it again".to_string(),
            )),
        }
    })
}

/// The yes half of the oversized-source confirm. The token and the instruction
/// travel in the confirm's args, so what is confirmed is what is sent.
pub fn confirmed_handler(chats: LlmChatsHandle, asks: LlmAsksHandle) -> ActionHandler {
    Arc::new(move |ctx: &ActionContext<'_>| -> Option<Effect> {
        let Args::List(items) = &ctx.args else {
            return None;
        };
        let (Some(ArgValue::String(token)), Some(ArgValue::String(instruction))) =
            (items.first(), items.get(1))
        else {
            return None;
        };
        let source = token.parse().ok().and_then(|t| asks.take(t));
        let Some(source) = source else {
            return Some(echo(
                EchoLevel::Error,
                "llm: that ask was dropped; ask again".to_string(),
            ));
        };
        let store = ctx.services.get::<BufferStoreHandle>();
        Some(ask_effect(
            &chats,
            &asks,
            store.as_deref(),
            source,
            instruction,
            true,
        ))
    })
}

/// The operator ids `bind_operator` binds.
pub struct AskOperators {
    pub ask: OperatorId,
    pub ask_menu: OperatorId,
}

fn ask_spec(
    apply: impl Fn(&mut OperatorContext) -> Result<Effect, CommandError> + Send + Sync + 'static,
) -> OperatorSpec {
    OperatorSpec {
        // It reads the span and changes nothing, so read-only buffers run it.
        mutates: false,
        // `.` repeats changes; an ask is not one (see the module docs).
        repeatable: false,
        // An ask is about the text: a block selection is one span.
        blockwise_per_row: false,
        post_motion_char: false,
        args_schema: vec![],
        apply: Arc::new(apply),
    }
}

/// Register both ask operators and declare the actions their prompt, menu row
/// and confirm fire. `store` resolves a source's file for `llm.exclude`.
pub fn register(
    registry: &mut CommandRegistry,
    chats: LlmChatsHandle,
    asks: LlmAsksHandle,
    store: BufferStoreHandle,
) -> AskOperators {
    for (name, doc) in [
        (
            ACTION_ASK_SEND,
            "llm: send the ask the instruction prompt was opened for.",
        ),
        (
            ACTION_ASK_CONTINUE,
            "llm: type the instruction for the ask the menu is holding.",
        ),
        (
            ACTION_ASK_CONFIRMED,
            "llm: send an oversized ask after its confirm.",
        ),
    ] {
        registry.register_action(
            name,
            doc,
            ActionSpec {
                apply: Arc::new(|_| Ok(Effect::None)),
                // A confirm's args travel through the dialog's transient
                // state, keyed by this schema: a value with no slot is
                // dropped. The oversized confirm carries two.
                args_schema: if name == ACTION_ASK_CONFIRMED {
                    vec![
                        lattice_grammar::ArgSpec::optional(
                            "token",
                            lattice_grammar::ArgKind::String,
                            "the held ask",
                        ),
                        lattice_grammar::ArgSpec::optional(
                            "instruction",
                            lattice_grammar::ArgKind::String,
                            "the instruction typed for it",
                        ),
                    ]
                } else {
                    vec![]
                },
            },
        );
    }
    let (c, a, st) = (chats.clone(), asks.clone(), store.clone());
    let ask = registry.register_operator(
        ASK_OPERATOR,
        "Ask a language model about the {motion}/{text-object} that follows: type an \
         instruction, and the answer opens in a new buffer beside this one, which keeps \
         the focus. `<leader>aaip` asks about a paragraph; in Visual, about the selection.",
        ask_spec(move |ctx| operator_ask(&c, &a, &st, ctx)),
    );
    let ask_menu = registry.register_operator(
        ASK_MENU_OPERATOR,
        "Ask about the {motion}/{text-object} that follows, opening `:llm-menu` first: \
         change the model, temperature or destination for this ask, then `a` to type the \
         instruction.",
        ask_spec(move |ctx| operator_ask_menu(&chats, &asks, &store, ctx)),
    );
    AskOperators { ask, ask_menu }
}

/// The lines an ex-command's range names (the caret's line without one), as an
/// ask's source: linewise, the final newline left out of the text.
fn lines_source(ctx: &ExCommandContext) -> Result<AskSource, Effect> {
    use lattice_protocol::position::Position;
    let (a, b) = match super::commands::range_lines(ctx) {
        Ok(Some(lines)) => lines,
        Ok(None) => (ctx.cursor.line, ctx.cursor.line),
        Err(e) => return Err(echo(EchoLevel::Error, e)),
    };
    let text = super::commands::lines_text(ctx, a, b);
    if text.trim().is_empty() {
        return Err(echo(
            EchoLevel::Info,
            "llm: nothing to ask about (the lines are empty)".to_string(),
        ));
    }
    Ok(AskSource {
        buffer: ctx.buffer_id,
        range: Range::new(Position::new(a, 0), Position::new(b + 1, 0)),
        text,
        register: lattice_grammar::Register::Unnamed,
    })
}

/// `:[range]llm-menu ask`: hold an ask over the command's lines for the menu,
/// as `<leader>aA` holds its span: its token and the lines it covers, or the
/// effect that refuses it (an excluded file, a composed view, empty lines).
pub(crate) fn hold_ask_lines(
    chats: &LlmChats,
    asks: &LlmAsks,
    store: &BufferStoreHandle,
    ctx: &ExCommandContext,
) -> Result<(u64, String), Effect> {
    if let Some(refused) = refusal(chats, Some(store), ctx.buffer_id) {
        return Err(refused);
    }
    let source = lines_source(ctx)?;
    let lines = line_span(source.range);
    Ok((asks.hold(source), lines))
}

/// Register `:llm-ask`: `:'<,'>llm-ask [key=value…] instruction` asks about the
/// range's lines (the current line without one), with no prompt, so a script or
/// a binding can ask. `store` names the source buffer, as the action's does.
pub fn register_ex(
    registry: &mut CommandRegistry,
    chats: LlmChatsHandle,
    asks: LlmAsksHandle,
    store: BufferStoreHandle,
) {
    use lattice_agent::parse_rest_as_text;
    use lattice_grammar::command::LatencyClass;
    use lattice_grammar::registry::{ExCommandSpec, SurfaceForm};
    use lattice_protocol::position::Position;
    let (send_chats, send_asks, send_store) = (chats.clone(), asks.clone(), store.clone());
    registry.register_ex_command(
        "llm-ask",
        "Ask a language model about the range's lines (the current line without a range), \
         with the rest of the line as the instruction: `:'<,'>llm-ask explain this`. Leading \
         `key=value` words set this request only (`model=local/qwen`). The answer opens in a \
         new buffer beside this one, which keeps the focus.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: true,
            parse_args: Arc::new(parse_rest_as_text),
            apply: Arc::new(move |ctx| {
                let source = match lines_source(ctx) {
                    Ok(source) => source,
                    Err(why) => return Ok(why),
                };
                let instruction = match &ctx.args {
                    Args::String(t) => t.clone(),
                    _ => String::new(),
                };
                Ok(ask_effect(
                    &chats,
                    &asks,
                    Some(&store),
                    source,
                    &instruction,
                    false,
                ))
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    registry.register_ex_command(
        "llm-send",
        "Send the buffer from its first line to the cursor's line to a language model, as the \
         prompt itself: write a question in any buffer, then `<leader>as`. The answer goes \
         where `llm.destination` says (a chat beside this buffer by default). In a chat, \
         sends its prompt, as `<CR>` does.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(lattice_agent::commands::parse_no_args),
            apply: Arc::new(move |ctx| {
                if send_chats.for_buffer(ctx.buffer_id).is_some() {
                    return Ok(Effect::InvokeCommand {
                        id: super::modes::chat::ACTION_CHAT_SEND.to_string(),
                        args: Args::None,
                    });
                }
                let last = ctx.cursor.line;
                let text = super::commands::lines_text(ctx, 0, last);
                if text.trim().is_empty() {
                    return Ok(Effect::Echo {
                        level: EchoLevel::Info,
                        text: "llm: nothing to send (the buffer is empty up to the cursor)"
                            .to_string(),
                    });
                }
                let source = AskSource {
                    buffer: ctx.buffer_id,
                    range: Range::new(Position::new(0, 0), Position::new(last + 1, 0)),
                    text,
                    register: lattice_grammar::Register::Unnamed,
                };
                // No instruction: the text is the prompt.
                Ok(ask_effect(
                    &send_chats,
                    &send_asks,
                    Some(&send_store),
                    source,
                    "",
                    false,
                ))
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use lattice_protocol::position::Position;

    fn src(n: u32) -> AskSource {
        AskSource {
            buffer: BufferId(n),
            range: Range::new(Position::new(0, 0), Position::new(0, 1)),
            text: "x".into(),
            register: lattice_grammar::Register::Unnamed,
        }
    }

    #[test]
    fn a_held_ask_is_taken_once() {
        let asks = LlmAsks::default();
        let t = asks.hold(src(1));
        assert_eq!(asks.take(t), Some(src(1)));
        assert_eq!(asks.take(t), None);
    }

    #[test]
    fn cancelled_prompts_do_not_pile_up() {
        let asks = LlmAsks::default();
        let first = asks.hold(src(0));
        for n in 1..40 {
            asks.hold(src(n));
        }
        assert_eq!(asks.pending_count(), PENDING_CAP);
        assert_eq!(asks.take(first), None, "the oldest went first");
    }

    #[test]
    fn the_prompt_name_carries_the_token() {
        assert_eq!(token_of(&prompt_buffer_name(42)), Some(42));
        assert_eq!(token_of("*llm:3*"), None);
    }

    #[test]
    fn a_linewise_span_names_the_lines_it_covers() {
        let r = |a, b, c, d| Range::new(Position::new(a, b), Position::new(c, d));
        assert_eq!(line_span(r(2, 0, 3, 0)), "line 3");
        assert_eq!(line_span(r(2, 0, 9, 0)), "lines 3-9");
        assert_eq!(line_span(r(2, 4, 2, 9)), "line 3");
        assert_eq!(line_span(r(2, 4, 5, 1)), "lines 3-6");
    }
}
