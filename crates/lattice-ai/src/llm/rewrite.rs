//! The rewrite operator (`<leader>ar{motion}`, LL.11b): change text through
//! diff review, never by a direct write (design §2).
//!
//! The operator captures its span, tracks it in the document so it follows
//! edits made while the model works, and opens an instruction prompt. Submitting
//! sends the span and the instruction on a **hidden session** (design §5: the
//! answer can always be seen and followed up), waits for the reply, and opens
//! it as a side-by-side diff against the span's text through
//! [`lattice_agent::review_text`]: the baseline is the buffer's text, which may
//! be unsaved, and nothing is written to disk.
//!
//! On accept, the accepted text (the user's edits to the proposed side
//! included) replaces the tracked span as one edit, in the buffer's own undo
//! history. Then:
//!
//! - **The span changed while the review was open:** accepting does not apply;
//!   the diff reopens against what the span holds now, with the accepted text
//!   as the proposal, so the deliberate confirm (design §5) shows what it
//!   would overwrite. A yes/no dialog could not. Accepting that applies.
//! - **The span was deleted:** nothing is applied; the answer stays in its
//!   session buffer, which is shown.
//! - **The source buffer closed:** the answer is shown in its session buffer
//!   rather than lost.
//! - **A reply wrapped in a code fence** is unwrapped: models fence code even
//!   when told not to.
//!
//! **Diffs open one at a time.** A second rewrite (another operator, or a macro
//! replaying one) waits for the first review to close.
//!
//! Refused up front, with the reason: a read-only buffer (the operator
//! mutates, so the host refuses it), a composed view such as a multibuffer
//! (its lines are not its own text), an `llm.exclude` file, an empty span.
//!
//! **`.` repeats a rewrite** (LL.11c): the operator declares `repeatable`, so
//! the host records it though its effect is a prompt, and `.` opens that
//! prompt over the same motion at the caret, pre-filled with the instruction
//! typed last time; `<CR>` sends. Nothing is sent unseen. A macro replays it
//! too, and the diffs queue.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use lattice_core::BufferId;
use lattice_core::CoreError;
use lattice_core::document::BaseMismatch;
use lattice_core::tracked::{Stickiness, TrackedRangeChange, TrackedRangeId};
use lattice_grammar::CommandError;
use lattice_grammar::effect::{EchoLevel, Effect};
use lattice_grammar::registry::{
    ActionSpec, CommandRegistry, OperatorContext, OperatorId, OperatorSpec,
};
use lattice_mode::inbound::InboundBus;
use lattice_mode::{ActionContext, ActionHandler, BufferStoreHandle};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::Range;
use tokio::runtime::Handle;

use super::chat::{ChatSession, LlmChats, LlmChatsHandle};
use super::modes::chat::LlmChatMode;
use super::operators::{AskSource, captured, echo, label_of, line_span, refusal, request_args};
use crate::conversation::{Block, SourceLink};

pub const REWRITE_OPERATOR: &str = "operator:llm-rewrite";
/// The rewrite operator that opens `:llm-menu` before the instruction prompt.
pub const REWRITE_MENU_OPERATOR: &str = "operator:llm-rewrite-menu";
/// The instruction prompt's submit action.
pub const ACTION_REWRITE_SEND: &str = "action:llm-rewrite-send";

/// Rewrites waiting for their instruction past this many are dropped, oldest
/// first, as cancelled asks are.
const PENDING_CAP: usize = 16;

/// A captured span, tracked in its document.
#[derive(Debug, Clone)]
struct Held {
    source: AskSource,
    tracked: TrackedRangeId,
}

/// Effects a rewrite's background task sends back to the editor.
pub struct RewriteEffects(pub Vec<Effect>);

/// The rewrites in flight: held spans, and the gate that keeps diffs opening
/// one at a time.
pub struct LlmRewrites {
    runtime: Handle,
    next: Mutex<u64>,
    pending: Mutex<VecDeque<(u64, Held)>>,
    /// One diff at a time, shared with the edit tools (LL.17).
    review_gate: Arc<tokio::sync::Mutex<()>>,
    /// Where the task's echoes reach the editor, with a wake.
    effects: Mutex<Option<InboundBus<RewriteEffects>>>,
    /// The host's diff bus. `None` in a harness without one: rewrites refuse.
    diffs: Option<lattice_diff::ProgrammaticDiffBus>,
    /// The host's buffers, to untrack a span whose prompt was dismissed or
    /// pushed out. Set at install; absent, such a span stays tracked.
    store: std::sync::OnceLock<BufferStoreHandle>,
}

pub type LlmRewritesHandle = Arc<LlmRewrites>;

impl LlmRewrites {
    pub fn new(runtime: Handle, diffs: Option<lattice_diff::ProgrammaticDiffBus>) -> Self {
        Self {
            runtime,
            next: Mutex::new(0),
            pending: Mutex::new(VecDeque::new()),
            store: std::sync::OnceLock::new(),
            review_gate: Arc::new(tokio::sync::Mutex::new(())),
            effects: Mutex::new(None),
            diffs,
        }
    }

    /// Connect the bus a task's effects return on.
    pub fn set_effects(&self, bus: InboundBus<RewriteEffects>) {
        *self.effects.lock().unwrap_or_else(|e| e.into_inner()) = Some(bus);
    }

    fn token(&self) -> u64 {
        let mut next = self.next.lock().unwrap_or_else(|e| e.into_inner());
        *next += 1;
        *next
    }

    /// Connect the host's buffers.
    pub fn set_store(&self, store: BufferStoreHandle) {
        let _ = self.store.set(store);
    }

    /// Hold a span until its prompt is answered. A span whose prompt is no
    /// longer shown (dismissed with `<Esc>`: nothing calls back) is let go
    /// here, since a new rewrite starting means its prompt is not the one
    /// being typed in, and so is
    /// the oldest past the cap; either way its tracked range is untracked,
    /// or it would follow every later edit of its buffer for good.
    fn hold(&self, held: Held) -> u64 {
        let token = self.token();
        let dropped: Vec<Held> = {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            let mut dropped = Vec::new();
            if let Some(store) = self.store.get() {
                pending.retain(|(t, h)| {
                    // A dismissed prompt's buffer stays registered, hidden;
                    // shown is what says it is still being answered.
                    let open = store
                        .find_by_name(&prompt_buffer_name(*t))
                        .is_some_and(|b| store.is_displayed(b));
                    if !open {
                        dropped.push(h.clone());
                    }
                    open
                });
            }
            pending.push_back((token, held));
            while pending.len() > PENDING_CAP {
                if let Some((_, h)) = pending.pop_front() {
                    dropped.push(h);
                }
            }
            dropped
        };
        if let Some(store) = self.store.get() {
            for h in dropped {
                untrack(store, h.source.buffer, h.tracked);
            }
        }
        token
    }

    fn take(&self, token: u64) -> Option<Held> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let i = pending.iter().position(|(t, _)| *t == token)?;
        pending.remove(i).map(|(_, h)| h)
    }

    /// Whether rewrite `token` still waits for its instruction.
    pub fn holds(&self, token: u64) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|(t, _)| *t == token)
    }

    /// How many rewrites wait for an instruction.
    #[doc(hidden)]
    pub fn pending_count(&self) -> usize {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// The gate every llm diff queues at: rewrites and edit tools alike.
    pub fn review_gate(&self) -> Arc<tokio::sync::Mutex<()>> {
        self.review_gate.clone()
    }

    /// Hand `effects` to the editor from a task, with a wake.
    pub fn send(&self, effects: Vec<Effect>) {
        if let Some(bus) = self
            .effects
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let _ = bus.send(RewriteEffects(effects));
        }
    }
}

/// The prompt buffer's name for rewrite `token`.
pub fn prompt_buffer_name(token: u64) -> String {
    format!("*llm-rewrite:{token}*")
}

fn token_of(name: &str) -> Option<u64> {
    name.strip_prefix("*llm-rewrite:")?
        .strip_suffix('*')?
        .parse()
        .ok()
}

/// A reply wrapped in one code fence, unwrapped; anything else unchanged.
pub fn unwrap_fence(reply: &str) -> &str {
    let trimmed = reply.trim();
    let Some(after_open) = trimmed.strip_prefix("```") else {
        return reply;
    };
    let Some(body_start) = after_open.find('\n') else {
        return reply;
    };
    let body = &after_open[body_start + 1..];
    match body.strip_suffix("```") {
        Some(inner) if !inner.contains("\n```") => inner,
        _ => reply,
    }
}

/// The reply as the span's replacement: unfenced, and ending in a newline
/// exactly when the span did.
fn replacement(reply: &str, original: &str) -> String {
    let mut text = unwrap_fence(reply).to_string();
    let wants_newline = original.ends_with('\n');
    while text.ends_with('\n') {
        text.pop();
    }
    if wants_newline {
        text.push('\n');
    }
    text
}

fn operator_rewrite(
    chats: &LlmChats,
    rewrites: &LlmRewrites,
    store: &BufferStoreHandle,
    ctx: &mut OperatorContext,
) -> Result<Effect, CommandError> {
    Ok(match hold_rewrite(chats, rewrites, store, ctx)? {
        Ok((token, lines)) => instruction_prompt(token, &lines),
        Err(why) => why,
    })
}

/// `<leader>aR`: hold the rewrite and open `:llm-menu` first; its `a` row
/// moves it on to the instruction prompt, as `<leader>aA` does an ask.
fn operator_rewrite_menu(
    chats: &LlmChats,
    rewrites: &LlmRewrites,
    store: &BufferStoreHandle,
    ctx: &mut OperatorContext,
) -> Result<Effect, CommandError> {
    let buffer = ctx.buffer_id;
    Ok(match hold_rewrite(chats, rewrites, store, ctx)? {
        Ok((token, lines)) => {
            chats.arm_menu_rewrite(buffer, token, lines);
            Effect::OpenTransient {
                source: super::menu::MENU_SOURCE.to_string(),
                args: lattice_grammar::args::Args::None,
            }
        }
        Err(why) => why,
    })
}

/// Capture and hold the span a rewrite operator ran over: its token and the
/// lines it covers, or the effect that refuses it.
fn hold_rewrite(
    chats: &LlmChats,
    rewrites: &LlmRewrites,
    store: &BufferStoreHandle,
    ctx: &mut OperatorContext,
) -> Result<Result<(u64, String), Effect>, CommandError> {
    if let Some(refused) = refusal(chats, Some(store), ctx.buffer_id) {
        return Ok(Err(refused));
    }
    if matches!(
        ctx.document.selections().primary().visual,
        Some(lattice_protocol::selection::VisualMode::Blockwise)
    ) {
        return Ok(Err(echo(
            EchoLevel::Error,
            "llm: a rewrite replaces one span; a block is a column of pieces, so select \
             it charwise or linewise"
                .to_string(),
        )));
    }
    if store
        .handle_for(ctx.buffer_id)
        .is_some_and(|h| h.display_line_numbers().is_some())
    {
        return Ok(Err(echo(
            EchoLevel::Error,
            "llm: a rewrite changes a file's own text; this view is composed from several, \
             so rewrite in the file itself"
                .to_string(),
        )));
    }
    let Some(source) = captured(ctx) else {
        return Ok(Err(echo(
            EchoLevel::Info,
            "llm: nothing to rewrite (the span is empty)".to_string(),
        )));
    };
    let buffer = ctx.document.buffer();
    let (Ok(start), Ok(end)) = (
        buffer.position_to_byte(source.range.start),
        buffer.position_to_byte(source.range.end),
    ) else {
        return Ok(Err(echo(
            EchoLevel::Error,
            "llm: the span is outside the buffer".to_string(),
        )));
    };
    // The span as captured: a linewise capture dropped its final newline from
    // the text, so the tracked bytes stop where the text does.
    let end = end.min(start + source.text.len());
    let tracked = TrackedRangeId::next();
    ctx.document
        .update_tracked_ranges(|t| t.insert(tracked, start..end, Stickiness::NeverGrows));
    let lines = line_span(source.range);
    let token = rewrites.hold(Held { source, tracked });
    Ok(Ok((token, lines)))
}

/// The instruction prompt for held rewrite `token`, covering `lines`.
pub(crate) fn instruction_prompt(token: u64, lines: &str) -> Effect {
    Effect::OpenPrompt {
        prompt: format!("Rewrite {lines}: "),
        initial: String::new(),
        on_submit_action: ACTION_REWRITE_SEND.to_string(),
        buffer_name: Some(prompt_buffer_name(token)),
    }
}

/// What the model is asked, around the user's instruction.
fn framed(instruction: &str) -> String {
    format!(
        "Rewrite the text above as follows: {}\n\nReply with only the rewritten text: no \
         explanation, no code fence.",
        instruction.trim()
    )
}

pub fn send_handler(chats: LlmChatsHandle, rewrites: LlmRewritesHandle) -> ActionHandler {
    Arc::new(move |ctx: &ActionContext<'_>| -> Option<Effect> {
        let store = ctx.services.get::<BufferStoreHandle>()?;
        let prompt = BufferId(ctx.buffer_id.0 as u32);
        let token = store.name_for(prompt).and_then(|n| token_of(&n))?;
        let Some(held) = rewrites.take(token) else {
            return Some(echo(
                EchoLevel::Error,
                "llm: that rewrite was dropped; ask again".to_string(),
            ));
        };
        let instruction = ctx.prompt_value.unwrap_or_default();
        Some(start(&chats, &rewrites, &store, held, instruction))
    })
}

fn untrack(store: &BufferStoreHandle, buffer: BufferId, tracked: TrackedRangeId) {
    if let Some(handle) = store.handle_for(buffer) {
        drop(handle.update_tracked_ranges(vec![TrackedRangeChange::Untrack(tracked)]));
    }
}

fn start(
    chats: &Arc<LlmChats>,
    rewrites: &Arc<LlmRewrites>,
    store: &BufferStoreHandle,
    held: Held,
    instruction: &str,
) -> Effect {
    let Some(diffs) = rewrites.diffs.clone() else {
        untrack(store, held.source.buffer, held.tracked);
        return echo(
            EchoLevel::Error,
            "llm: rewrites need diff review, which this editor has not set up".to_string(),
        );
    };
    let (args, typed, mut overrides) = match request_args(chats, held.source.buffer, instruction) {
        Ok(parsed) => parsed,
        Err(why) => {
            untrack(store, held.source.buffer, held.tracked);
            return why;
        }
    };
    if typed.trim().is_empty() {
        untrack(store, held.source.buffer, held.tracked);
        return echo(
            EchoLevel::Info,
            "llm: a rewrite needs an instruction; nothing was sent".to_string(),
        );
    }
    let mut values = chats.next_request(held.source.buffer);
    chats.clear_next_request(held.source.buffer);
    values.extend(args);
    overrides.values = values;

    let (label, hint, lang) = label_of(Some(store), &held.source);
    let session = chats.create_for_source(held.source.buffer, &hint);
    session.set_destination(format!("rewrite {label}"));
    let block = Block::Source {
        label: label.clone(),
        lang,
        text: held.source.text.clone(),
        link: Some(SourceLink {
            buffer: held.source.buffer,
            line: held.source.range.start.line,
        }),
    };
    let ended = session.wait_turn_end();
    chats.submit_source(&session, block, framed(&typed), overrides);

    let task = Task {
        rewrites: rewrites.clone(),
        store: store.clone(),
        diffs,
        session,
        held,
        label,
    };
    rewrites.runtime.spawn(task.run(ended));
    echo(
        EchoLevel::Info,
        "llm: rewriting; the diff opens when the answer arrives".to_string(),
    )
}

/// One rewrite, from the reply to the edit.
struct Task {
    rewrites: Arc<LlmRewrites>,
    store: BufferStoreHandle,
    diffs: lattice_diff::ProgrammaticDiffBus,
    session: Arc<ChatSession>,
    held: Held,
    label: String,
}

impl Task {
    fn show_session(&self, why: String) -> Vec<Effect> {
        vec![
            Effect::DisplaySyntheticBuffer {
                name: self.session.name.clone(),
                mode_id: LlmChatMode::mode_id().as_str().to_string(),
                content: None,
                split: lattice_core::ui::pane::SplitOrientation::Vertical,
            },
            echo(EchoLevel::Info, why),
        ]
    }

    async fn run(self, ended: tokio::sync::oneshot::Receiver<Result<String, String>>) {
        let buffer = self.held.source.buffer;
        let reply = match ended.await {
            Ok(Ok(reply)) => reply,
            Ok(Err(e)) if e == "interrupted" => {
                untrack(&self.store, buffer, self.held.tracked);
                self.rewrites.send(vec![echo(
                    EchoLevel::Info,
                    "llm: rewrite stopped; nothing changed".to_string(),
                )]);
                return;
            }
            Ok(Err(e)) => {
                untrack(&self.store, buffer, self.held.tracked);
                self.rewrites.send(vec![echo(
                    EchoLevel::Error,
                    format!("llm: rewrite failed ({e}); nothing changed"),
                )]);
                return;
            }
            Err(_) => {
                untrack(&self.store, buffer, self.held.tracked);
                return;
            }
        };
        let proposed = replacement(&reply, &self.held.source.text);

        // One diff at a time: a second rewrite waits here for the first review.
        let _gate = self.rewrites.review_gate.lock().await;
        if !self.store.contains_buffer(buffer) {
            let name = self.session.name.clone();
            self.rewrites.send(
                self.show_session(format!("llm: the source closed; the rewrite is in {name}")),
            );
            return;
        }
        let path = self
            .store
            .path_for(buffer)
            .unwrap_or_else(|| std::path::PathBuf::from(&self.label));
        let mut baseline = self.held.source.text.clone();
        let mut proposed = proposed;
        let mut tab_name = format!("rewrite {}", self.label);
        loop {
            let reviewed = lattice_agent::review_text(
                &self.diffs,
                lattice_agent::TextReviewRequest {
                    path: path.clone(),
                    baseline: baseline.clone(),
                    proposed,
                    tab_name: tab_name.clone(),
                    origin_session: 0,
                },
            )
            .await;
            let accepted = match reviewed {
                Ok(Some(text)) => text,
                Ok(None) | Err(_) => {
                    untrack(&self.store, buffer, self.held.tracked);
                    self.rewrites.send(vec![echo(
                        EchoLevel::Info,
                        "llm: rewrite rejected; nothing changed".to_string(),
                    )]);
                    return;
                }
            };
            match apply_if_unchanged(&self.store, buffer, self.held.tracked, &baseline, &accepted)
                .await
            {
                None => {
                    untrack(&self.store, buffer, self.held.tracked);
                    let name = self.session.name.clone();
                    self.rewrites.send(self.show_session(format!(
                        "llm: the rewritten text was deleted; the rewrite is in {name}"
                    )));
                    return;
                }
                Some(Applied::Yes) => {
                    self.rewrites.send(vec![echo(
                        EchoLevel::Info,
                        "llm: rewrite applied".to_string(),
                    )]);
                    return;
                }
                // Changed under the review: review again against what is
                // there now, so accepting is a deliberate choice made seeing it.
                Some(Applied::Changed(now)) => {
                    self.rewrites.send(vec![echo(
                        EchoLevel::Info,
                        format!(
                            "llm: {} changed during the review; review it again against the text as it is now",
                            self.label
                        ),
                    )]);
                    baseline = now;
                    proposed = accepted;
                    tab_name = format!("rewrite {} (changed)", self.label);
                }
            }
        }
    }
}

/// How [`apply_if_unchanged`] ended, when the span still exists.
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
enum Applied {
    /// The span held the reviewed text, and now holds the accepted text.
    Yes,
    /// The span holds other text: this.
    Changed(String),
}

/// Give up re-reading after this many writes refused in a row; each needs a
/// fresh edit to the buffer, so reaching it means the user is typing in the
/// span, and the review is shown again against what is there.
const APPLY_ATTEMPTS: usize = 8;

/// Replace the tracked span with `accepted` if it still holds `baseline`,
/// then stop tracking it. `None` when the span or its buffer is gone.
///
/// The span is read and the write positioned from ONE snapshot, and the write
/// is refused (`BaseMismatch::Refuse`) if anything but a stream's append
/// changed the text after that snapshot. The comparison and the write are so
/// one decision: a key typed between them makes the write refuse, the span is
/// read again, and the key is never overwritten (owner-write-caret §10.1).
async fn apply_if_unchanged(
    store: &BufferStoreHandle,
    buffer: BufferId,
    tracked: TrackedRangeId,
    baseline: &str,
    accepted: &str,
) -> Option<Applied> {
    let handle = store.handle_for(buffer)?;
    apply_to(&*handle, tracked, baseline, accepted, || async {}).await
}

/// [`apply_if_unchanged`] on one document. `between` runs after each read and
/// before its write: nothing in production, the window a key can land in.
/// It is a parameter so a test can put an edit exactly there; the race it
/// pins cannot be timed from outside.
async fn apply_to<F, Fut>(
    handle: &dyn lattice_runtime::Document,
    tracked: TrackedRangeId,
    baseline: &str,
    accepted: &str,
    mut between: F,
) -> Option<Applied>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let mut now = String::new();
    for _ in 0..APPLY_ATTEMPTS {
        let snap = handle.snapshot();
        let range = snap.tracked_ranges.get(tracked)?;
        if range.collapsed {
            return None;
        }
        now = snap
            .buffer
            .as_string()
            .get(range.start..range.end)?
            .to_string();
        if now != baseline {
            return Some(Applied::Changed(now));
        }
        let (Ok(start), Ok(end)) = (
            snap.buffer.byte_to_position(range.start),
            snap.buffer.byte_to_position(range.end),
        ) else {
            return None;
        };
        let version = snap.text_version;
        drop(snap);
        between().await;
        match handle
            .apply_edit_batch_based_on(
                vec![Edit::replace(Range::new(start, end), accepted)],
                version,
                BaseMismatch::Refuse,
            )
            .await
        {
            Ok(_) => {
                drop(handle.update_tracked_ranges(vec![TrackedRangeChange::Untrack(tracked)]));
                return Some(Applied::Yes);
            }
            Err(lattice_runtime::RuntimeError::Core(CoreError::Stale)) => {
                tracing::debug!("llm: rewrite write refused as stale; reading the span again");
            }
            Err(e) => {
                tracing::warn!("llm: rewrite could not be applied: {e}");
                return None;
            }
        }
    }
    Some(Applied::Changed(now))
}

/// Register `operator:llm-rewrite` and the action its prompt fires.
/// The rewrite operators `register` adds.
#[derive(Debug, Clone, Copy)]
pub struct RewriteOperators {
    pub rewrite: OperatorId,
    /// `<leader>aR`: the same, opening `:llm-menu` first.
    pub rewrite_menu: OperatorId,
}

pub fn register(
    registry: &mut CommandRegistry,
    chats: LlmChatsHandle,
    rewrites: LlmRewritesHandle,
    store: BufferStoreHandle,
) -> RewriteOperators {
    registry.register_action(
        ACTION_REWRITE_SEND,
        "llm: send the rewrite the instruction prompt was opened for.",
        ActionSpec {
            apply: Arc::new(|_| Ok(Effect::None)),
            args_schema: vec![],
        },
    );
    let rewrite = registry.register_operator(
        REWRITE_OPERATOR,
        "Rewrite the {motion}/{text-object} that follows with a language model: type an \
         instruction, and the answer opens as a diff against the text. Accepting applies it \
         as one undoable edit; nothing is written to disk.",
        OperatorSpec {
            // It changes the buffer, so read-only buffers refuse it.
            mutates: true,
            // The change lands later, through review, so the effect `.` sees is
            // the prompt; declaring this is what makes `.` repeat it, the
            // prompt pre-filled with the last instruction (LL.11c).
            repeatable: true,
            blockwise_per_row: false,
            post_motion_char: false,
            args_schema: vec![],
            apply: Arc::new({
                let (chats, rewrites, store) = (chats.clone(), rewrites.clone(), store.clone());
                move |ctx| operator_rewrite(&chats, &rewrites, &store, ctx)
            }),
        },
    );
    let rewrite_menu = registry.register_operator(
        REWRITE_MENU_OPERATOR,
        "Rewrite the {motion}/{text-object} that follows, opening `:llm-menu` first: change          the model, temperature or other settings for this rewrite, then `a` to type the          instruction.",
        OperatorSpec {
            mutates: true,
            // The prompt opens from the menu's row, not from this effect, so
            // there is no submit for `.` to remember; `<leader>ar` repeats.
            repeatable: false,
            blockwise_per_row: false,
            post_motion_char: false,
            args_schema: vec![],
            apply: Arc::new(move |ctx| operator_rewrite_menu(&chats, &rewrites, &store, ctx)),
        },
    );
    RewriteOperators {
        rewrite,
        rewrite_menu,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    /// A key typed after the span was read and before the write: the write
    /// is refused, the span read again, and the change reported, so the
    /// review is shown again instead of the key being overwritten.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_key_between_the_read_and_the_write_is_never_overwritten() {
        use lattice_protocol::position::Position;
        let handle = lattice_runtime::handle::spawn_document(
            BufferId(1),
            lattice_core::Document::from_text("alpha beta\n"),
            Arc::new(arc_swap::ArcSwap::from_pointee(
                lattice_grammar::registry::CommandRegistry::new(),
            )),
        );
        let tracked = TrackedRangeId::next();
        handle
            .update_tracked_ranges(vec![TrackedRangeChange::Track {
                id: tracked,
                bytes: 0..10,
                stickiness: Stickiness::NeverGrows,
            }])
            .await
            .unwrap();
        let mut typed = false;
        let out = apply_to(&handle, tracked, "alpha beta", "NEW", || {
            let first = !typed;
            typed = true;
            let handle = handle.clone();
            async move {
                if first {
                    handle
                        .apply_edit(Edit::insert(Position::new(0, 2), "X"))
                        .await
                        .unwrap();
                }
            }
        })
        .await;
        assert_eq!(out, Some(Applied::Changed("alXpha beta".to_string())));
        assert_eq!(handle.snapshot().buffer.as_string(), "alXpha beta\n");

        // With nothing in the window it applies.
        let out = apply_to(&handle, tracked, "alXpha beta", "NEW", || async {}).await;
        assert_eq!(out, Some(Applied::Yes));
        assert_eq!(handle.snapshot().buffer.as_string(), "NEW\n");
    }

    #[test]
    fn a_fenced_reply_is_unwrapped() {
        assert_eq!(unwrap_fence("```rust\nfn a() {}\n```"), "fn a() {}\n");
        assert_eq!(unwrap_fence("  ```\nx\n```\n"), "x\n");
        assert_eq!(unwrap_fence("plain text"), "plain text");
    }

    #[test]
    fn two_fences_are_not_one() {
        let reply = "```\na\n```\nand\n```\nb\n```";
        assert_eq!(unwrap_fence(reply), reply);
    }

    #[test]
    fn the_replacement_ends_as_the_span_did() {
        assert_eq!(replacement("```\nx\n```", "a\nb"), "x");
        assert_eq!(replacement("x", "a\n"), "x\n");
        assert_eq!(replacement("x\n\n", "a"), "x");
    }

    #[test]
    fn the_prompt_name_carries_the_token() {
        assert_eq!(token_of(&prompt_buffer_name(7)), Some(7));
        assert_eq!(token_of("*llm-ask:7*"), None);
    }
}
