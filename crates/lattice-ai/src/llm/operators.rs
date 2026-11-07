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
    ActionSpec, CommandRegistry, OperatorContext, OperatorId, OperatorSpec,
};
use lattice_mode::{ActionContext, ActionHandler, BufferStoreHandle};
use lattice_protocol::position::Range;

use super::chat::{LlmChats, LlmChatsHandle, SendPlan};
use super::commands::parse_request_args;
use super::modes::chat::LlmChatMode;
use super::settings::RequestOverrides;
use crate::conversation::{Block, SourceLink};

pub const ASK_OPERATOR: &str = "operator:llm-ask";
/// The instruction prompt's submit action.
pub const ACTION_ASK_SEND: &str = "action:llm-ask-send";

/// Asks waiting for their instruction. A prompt cancelled with `<Esc>` leaves
/// its ask behind, so the oldest are dropped past this many.
const PENDING_CAP: usize = 16;

/// What an ask is about, captured when the operator ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskSource {
    pub buffer: BufferId,
    pub range: Range,
    pub text: String,
}

/// Asks the operator captured, until their prompt is submitted.
#[derive(Default)]
pub struct LlmAsks {
    next: Mutex<u64>,
    pending: Mutex<VecDeque<(u64, AskSource)>>,
}

pub type LlmAsksHandle = Arc<LlmAsks>;

impl LlmAsks {
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
fn captured(ctx: &OperatorContext) -> Option<AskSource> {
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
    })
}

fn operator_ask(asks: &LlmAsks, ctx: &mut OperatorContext) -> Result<Effect, CommandError> {
    let Some(source) = captured(ctx) else {
        return Ok(Effect::Echo {
            level: EchoLevel::Info,
            text: "llm: nothing to ask about (the span is empty)".to_string(),
        });
    };
    let lines = line_span(source.range);
    let token = asks.hold(source);
    Ok(Effect::OpenPrompt {
        prompt: format!("Ask about {lines}: "),
        initial: String::new(),
        on_submit_action: ACTION_ASK_SEND.to_string(),
        buffer_name: Some(prompt_buffer_name(token)),
    })
}

/// The 1-based lines `range` covers: `3`, or `3-10`. A span ending at a line's
/// start (linewise) does not include that line.
fn line_span(range: Range) -> String {
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
fn label_of(store: Option<&BufferStoreHandle>, source: &AskSource) -> (String, String, String) {
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

/// Send `source` with the typed `instruction` (leading `key=value` words are
/// this request's overrides) to a new dedicated buffer shown beside the source.
///
/// The new chat copies the source buffer's session settings and any menu choice
/// waiting for its next request; nothing stays linked (design §4). Only the
/// typed instruction is scanned for `@profile`, never the source text.
pub fn ask_effect(
    chats: &Arc<LlmChats>,
    store: Option<&BufferStoreHandle>,
    source: AskSource,
    instruction: &str,
) -> Effect {
    let (args, typed) = match parse_request_args(instruction) {
        Ok(parsed) => parsed,
        Err(e) => {
            return Effect::Echo {
                level: EchoLevel::Error,
                text: e,
            };
        }
    };
    let mut overrides = RequestOverrides::default();
    match chats.plan_send(source.buffer, &typed) {
        SendPlan::Plain => {}
        SendPlan::Profile(p) => {
            overrides.profile = Some(p);
            overrides.pasted_profile = true;
        }
        // Tools reach an ask only through a chat's confirm; an ask has no
        // prompt left to confirm into once it is sent.
        SendPlan::NeedsConfirm(p) => {
            return Effect::Echo {
                level: EchoLevel::Error,
                text: format!(
                    "llm: @{p} enables tools, which an ask cannot confirm; open a chat with :llm {p}"
                ),
            };
        }
    }
    let mut values = chats.next_request(source.buffer);
    chats.clear_next_request(source.buffer);
    values.extend(args);
    overrides.values = values;

    let (label, hint, lang) = label_of(store, &source);
    let session = chats.create_for_source(source.buffer, &hint);
    let block = Block::Source {
        label,
        lang,
        text: source.text,
        link: Some(SourceLink {
            buffer: source.buffer,
            line: source.range.start.line,
        }),
    };
    chats.submit_source(&session, block, typed.trim().to_string(), overrides);
    Effect::DisplaySyntheticBuffer {
        name: session.name.clone(),
        mode_id: LlmChatMode::mode_id().as_str().to_string(),
        content: None,
        split: lattice_core::ui::pane::SplitOrientation::Vertical,
    }
}

/// `action:llm-ask-send`: the instruction prompt was submitted.
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
        Some(ask_effect(&chats, store.as_deref(), source, instruction))
    })
}

/// Register `operator:llm-ask` and declare its prompt's action. Returns the
/// operator's id for `bind_operator`.
pub fn register(registry: &mut CommandRegistry, asks: LlmAsksHandle) -> OperatorId {
    registry.register_action(
        ACTION_ASK_SEND,
        "llm: send the ask the instruction prompt was opened for.",
        ActionSpec {
            apply: Arc::new(|_| Ok(Effect::None)),
            args_schema: vec![],
        },
    );
    registry.register_operator(
        ASK_OPERATOR,
        "Ask a language model about the {motion}/{text-object} that follows: type an \
         instruction, and the answer opens in a new buffer beside this one, which keeps \
         the focus. `<leader>aaip` asks about a paragraph; in Visual, about the selection.",
        OperatorSpec {
            // It reads the span and changes nothing, so read-only buffers run it.
            mutates: false,
            // `.` repeats changes; an ask is not one (see the module docs).
            repeatable: false,
            // An ask is about the text: a block selection is one span.
            blockwise_per_row: false,
            post_motion_char: false,
            args_schema: vec![],
            apply: Arc::new(move |ctx| operator_ask(&asks, ctx)),
        },
    )
}

/// Register `:llm-ask`: `:'<,'>llm-ask [key=value…] instruction` asks about the
/// range's lines (the current line without one), with no prompt, so a script or
/// a binding can ask. `store` names the source buffer, as the action's does.
pub fn register_ex(
    registry: &mut CommandRegistry,
    chats: LlmChatsHandle,
    store: BufferStoreHandle,
) {
    use lattice_agent::parse_rest_as_text;
    use lattice_grammar::args::Args;
    use lattice_grammar::command::LatencyClass;
    use lattice_grammar::registry::{ExCommandSpec, SurfaceForm};
    use lattice_protocol::position::Position;
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
                let (a, b) = match super::commands::range_lines(ctx) {
                    Ok(Some(lines)) => lines,
                    Ok(None) => (ctx.cursor.line, ctx.cursor.line),
                    Err(e) => {
                        return Ok(Effect::Echo {
                            level: EchoLevel::Error,
                            text: e,
                        });
                    }
                };
                let text = super::commands::lines_text(ctx, a, b);
                if text.trim().is_empty() {
                    return Ok(Effect::Echo {
                        level: EchoLevel::Info,
                        text: "llm: nothing to ask about (the lines are empty)".to_string(),
                    });
                }
                let instruction = match &ctx.args {
                    Args::String(t) => t.clone(),
                    _ => String::new(),
                };
                let source = AskSource {
                    buffer: ctx.buffer_id,
                    range: Range::new(Position::new(a, 0), Position::new(b + 1, 0)),
                    text,
                };
                Ok(ask_effect(&chats, Some(&store), source, &instruction))
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
