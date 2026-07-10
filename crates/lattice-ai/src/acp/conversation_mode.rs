//! `ai-conversation` major mode (AU‑2).
//!
//! The read-only `*ai:opencode*` buffer backing the agent conversation view.
//! Mirrors `lattice_agent::log::modes::AiLogMode`: `on_activate` reads the
//! [`ConversationStore`](crate::acp::conversation::ConversationStore) service,
//! seeds the buffer from the current snapshot, subscribes to
//! [`ConversationUpdated`](crate::acp::conversation::ConversationUpdated), and
//! spawns a drain task that re-projects on every change. The returned
//! `Subscription` guard unsubscribes on drop.
//!
//! [`render_conversation`] is pure and unit-testable. Projection is a
//! **line-granular suffix replace**: unchanged leading lines are never
//! rewritten, so streaming appends touch only the tail and a tool-call status
//! change rewrites only from its line down (AU‑2 shows status inline as text; a
//! decoration-based in-place update is a follow-up).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use lattice_grammar::ModalState;
use lattice_grammar::effect::{EchoLevel, Effect};
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerContribution, BufferStoreHandle, CapabilitySet,
    EditableTail, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    OptionOverrideSet, Subscription, TickCallbackRegistration, TickCallbackRegistryHandle,
    keymap_entry,
};

use crate::acp::conversation::{
    Block, Conversation, ConversationProjected, ConversationStore, ConversationUpdated, Role,
};
use crate::acp::handle::AiClientHandle;

/// AU‑3: the prompt marker rendered at the head of the editable tail line.
/// Two bytes, so [`EditableTail::first_line_min_byte`] is `2`: the marker
/// itself is not user-editable (backspace at the prompt start is refused by
/// the read-only gate), only the text after it.
const PROMPT_MARKER: &str = "> ";

/// The synthetic buffer name for the (single, v1) opencode conversation.
pub fn conversation_buffer_name() -> String {
    "*ai:opencode*".to_string()
}

/// `ai-conversation-mode` -- major mode for the `*ai:opencode*` buffer.
pub struct AiConversationMode;

impl AiConversationMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("ai-conversation-mode")
    }
}

/// Render the whole conversation to plain text. Pure; the projection diffs this
/// against the buffer to compute a minimal edit.
pub fn render_conversation(conv: &Conversation) -> String {
    let mut out = String::new();
    for turn in &conv.turns {
        let who = match turn.role {
            Role::User => "you",
            Role::Assistant => "opencode",
        };
        out.push_str(who);
        out.push_str(":\n");
        for block in &turn.blocks {
            match block {
                Block::Text(s) => {
                    for line in s.split('\n') {
                        out.push_str(line);
                        out.push('\n');
                    }
                }
                Block::Reasoning(s) => {
                    for line in s.split('\n') {
                        out.push_str("  \u{2502} ");
                        out.push_str(line);
                        out.push('\n');
                    }
                }
                Block::ToolCall { title, status, .. } => {
                    out.push_str("  \u{25b8} ");
                    out.push_str(title);
                    out.push_str(" [");
                    out.push_str(status_tag(status));
                    out.push_str("]\n");
                }
                Block::Edit { path, status } => {
                    out.push_str("  \u{270e} ");
                    out.push_str(path);
                    out.push_str(" [");
                    out.push_str(edit_tag(status));
                    out.push_str("]\n");
                }
            }
        }
        out.push('\n');
    }
    out
}

fn status_tag(status: &crate::acp::conversation::ToolStatus) -> &'static str {
    use crate::acp::conversation::ToolStatus::*;
    match status {
        Pending => "pending",
        Running => "running",
        Ok => "ok",
        Err => "error",
    }
}

fn edit_tag(status: &crate::acp::conversation::EditStatus) -> &'static str {
    use crate::acp::conversation::EditStatus::*;
    match status {
        Proposed => "proposed",
        Accepted => "accepted",
        Rejected => "rejected",
    }
}

/// The minimal line-granular replacement turning `old` into `new`: the index of
/// the first differing line, and the replacement text for everything from that
/// line onward. `None` when the texts are already equal.
///
/// Pure and unit-testable; the mode turns this into an
/// [`Edit`](lattice_protocol::edit::Edit) against the live buffer positions.
pub fn suffix_replace(old: &str, new: &str) -> Option<LineSuffixReplace> {
    if old == new {
        return None;
    }
    let old_lines: Vec<&str> = old.split('\n').collect();
    let new_lines: Vec<&str> = new.split('\n').collect();
    let common = old_lines
        .iter()
        .zip(new_lines.iter())
        .take_while(|(a, b)| a == b)
        .count();
    Some(LineSuffixReplace {
        first_diff_line: common,
        old_line_count: old_lines.len(),
        replacement: new_lines[common..].join("\n"),
    })
}

/// Result of [`suffix_replace`]: replace lines `first_diff_line..old_line_count`
/// (0-based, the split-on-`\n` line grid) with `replacement`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineSuffixReplace {
    pub first_diff_line: usize,
    pub old_line_count: usize,
    pub replacement: String,
}

/// AU‑3+ activation guard: the `ConversationUpdated` subscription plus the
/// per-tick prompt-focus callback registration. Dropping it on deactivation
/// unsubscribes AND removes the callback, so a stopped mode contributes no
/// further per-tick work.
pub struct AiConversationGuard {
    _subscription: Subscription,
    _focus_tick: Option<TickCallbackRegistration>,
}

impl Mode for AiConversationMode {
    type Guard = Option<AiConversationGuard>;
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Major
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

    /// AU‑3: the prompt is the single trailing line, editable only after the
    /// `"> "` marker (2 bytes). Consulted by the host's read-only edit gate so
    /// Insert/operator keystrokes land only in the prompt; the transcript above
    /// stays owner-written.
    fn editable_tail(&self) -> Option<EditableTail> {
        Some(EditableTail {
            trailing_lines: 1,
            first_line_min_byte: PROMPT_MARKER.len() as u32,
        })
    }

    /// AU‑3: the modal-input surface. Insert-entering chords relocate the
    /// cursor into the prompt first (so Insert only ever edits the prompt, and
    /// history is unreachable from Insert); `<CR>` sends; `<C-c>` interrupts.
    /// The host's K.2.4 translate pass pushes these under
    /// `MajorMode(ai-conversation-mode)`, gated by K.1.c to this buffer.
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(ai_conversation_keymap_entries())
    }

    /// AU‑3: the handler bodies for the chords above, mode-owned (no host
    /// `Editor::` method, no `Action` variant). Bound at boot by the host's
    /// `register_mode_action_handlers` walk. Each reads the buffer / services
    /// from the [`ActionContext`] and returns an [`Effect`] the host applies.
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![
            ActionHandlerContribution {
                action_name: "action:ai-conv-focus-prompt",
                handler: focus_prompt_handler(),
            },
            ActionHandlerContribution {
                action_name: "action:ai-conv-send",
                handler: send_handler(),
            },
            ActionHandlerContribution {
                action_name: "action:ai-conv-interrupt",
                handler: interrupt_handler(),
            },
            ActionHandlerContribution {
                action_name: "action:ai-conv-toggle-trust",
                handler: toggle_trust_handler(),
            },
        ]
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<lattice_mode::BufferStoreHandle>() else {
                return Ok(None);
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(None);
            };
            let Some(conv_store) = ctx.service::<ConversationStore>() else {
                return Ok(None);
            };
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                return Ok(None);
            };

            // Sync the buffer to the current snapshot with a single full replace
            // (handles a fresh OR reopened buffer uniformly). `last` then tracks
            // the buffer content so subsequent updates diff self-consistently --
            // no fragile buffer-text round-trip.
            // AU‑3: `last` tracks the *conversation zone* only; the buffer is
            // `{conversation}{PROMPT_MARKER}` with the editable prompt as the
            // trailing line. Seeding appends the marker once; the update path
            // below is unchanged because `suffix_edit`'s replace range ends at
            // `text_end(last)` — the start of the prompt line — so re-projecting
            // the transcript never rewrites the user's in-progress prompt.
            let seed = conv_store.snapshot();
            let mut last = render_conversation(&seed);
            let mut last_user_turns = user_turn_count(&seed);
            full_replace(&handle, &format!("{last}{PROMPT_MARKER}")).await;

            // AU‑3+: after a send, the async re-projection grows the transcript
            // above the prompt and shifts the (now-cleared) prompt line down.
            // The host clamps but does NOT move the caret to follow an
            // owner-write edit, so it would go stale in the read-only
            // transcript. `<CR>` deliberately stays in Insert (vim / `:terminal`
            // / `:claude` parity — only <Esc> leaves); this per-tick callback
            // re-parks the caret at the fresh prompt tail once the drain signals
            // a send-triggered re-projection landed. The callback runs on the
            // host thread, so it can read the post-edit buffer and emit a
            // `SelectionChange`; the flag is the drain→host handoff. Set only on
            // a User turn, which is added only when the user just sent from THIS
            // buffer, so the buffer is guaranteed active when it fires.
            let focus_flag = Arc::new(AtomicBool::new(false));
            let focus_tick = ctx.service::<TickCallbackRegistryHandle>().map(|registry| {
                let flag = focus_flag.clone();
                let store = store.clone();
                registry.register(Box::new(move || {
                    if !flag.swap(false, Ordering::AcqRel) {
                        return Vec::new();
                    }
                    let Some(h) = store.handle_for(buffer_id) else {
                        return Vec::new();
                    };
                    let snap = h.snapshot();
                    let last_line = snap.buffer.line_count().saturating_sub(1);
                    let end_byte = snap
                        .buffer
                        .line(last_line)
                        .unwrap_or_default()
                        .trim_end_matches('\n')
                        .len() as u32;
                    let pos = lattice_protocol::position::Position::new(last_line, end_byte);
                    vec![Effect::SelectionChange(
                        lattice_protocol::selection::SelectionSet::single(
                            lattice_protocol::selection::Selection::cursor(pos),
                        ),
                    )]
                }))
            });

            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ConversationUpdated>();
            let sub_id = ctx.events().subscribe_typed::<ConversationUpdated>(tx);
            let bus_handle = ctx.events_handle();
            // A separate handle for the drain to *publish* `ConversationProjected`
            // once its edit lands (see below).
            let projected_bus = ctx.events_handle();

            let drain_flag = focus_flag;
            runtime.spawn(async move {
                // Coalesce a burst of updates into one re-projection.
                while rx.recv().await.is_some() {
                    while rx.try_recv().is_ok() {}
                    let snap = conv_store.snapshot();
                    let new = render_conversation(&snap);
                    let user_turns = user_turn_count(&snap);
                    // A new User turn means the user just sent a prompt. Rewrite
                    // the tail through EOF and re-append an empty prompt marker,
                    // clearing the prompt *atomically* with the transcript update.
                    // The send handler deliberately emits no clear edit: a
                    // separate clear would race this re-projection (which shifts
                    // the prompt line down) and land on the wrong, now-read-only
                    // line. On agent-only updates we leave the user's in-progress
                    // prompt untouched.
                    let did_clear = user_turns > last_user_turns;
                    let changed = reproject(&handle, &last, &new, did_clear).await;
                    if did_clear {
                        // Prompt just reset — ask the host to re-park the caret
                        // at the fresh prompt tail on the next tick.
                        drain_flag.store(true, Ordering::Release);
                    }
                    // Wake the editor actor now that the edit has LANDED, so the
                    // streamed response repaints (and the focus tick callback
                    // runs) WITHOUT a keystroke. Published after the await — not
                    // via `ConversationUpdated`, which fires before the buffer is
                    // re-projected — so the wake never repaints stale content.
                    if changed {
                        projected_bus.publish_typed(ConversationProjected);
                    }
                    last = new;
                    last_user_turns = user_turns;
                }
            });

            Ok(Some(AiConversationGuard {
                _subscription: Subscription::new(bus_handle, sub_id),
                _focus_tick: focus_tick,
            }))
        })
    }
}

/// (line, col) of the end of `s` on the split-on-`\n` line grid.
fn text_end(s: &str) -> (u32, u32) {
    let last_line = s.split('\n').count().saturating_sub(1);
    let last_len = s.rsplit('\n').next().map(str::len).unwrap_or(0);
    (last_line as u32, last_len as u32)
}

/// Replace the entire buffer with `text`.
async fn full_replace(handle: &std::sync::Arc<dyn lattice_runtime::Document>, text: &str) {
    let snap = handle.snapshot();
    let last_line = snap.buffer.line_count().saturating_sub(1);
    let last_len = snap.buffer.line(last_line).unwrap_or_default().len() as u32;
    let range = lattice_protocol::Range::new(
        lattice_protocol::position::Position::new(0, 0),
        lattice_protocol::position::Position::new(last_line, last_len),
    );
    let edit = lattice_protocol::edit::Edit::replace(range, text.to_string());
    let _ = handle.apply_edit_batch(vec![edit]).await;
}

/// Number of `User`-role turns in the conversation. A send is the only thing
/// that adds one (`ConversationStore::push_user_text`), so an increase between
/// re-projections is the drain's reliable "the prompt was just sent" signal.
fn user_turn_count(conv: &Conversation) -> usize {
    conv.turns.iter().filter(|t| t.role == Role::User).count()
}

/// Re-project the transcript zone `last` → `new` into the buffer with one edit.
///
/// When `clear_prompt` (a send just added a User turn) the edit extends through
/// the *current* prompt line to EOF and re-appends an empty `PROMPT_MARKER`, so
/// the transcript grows and the prompt resets in a single atomic edit — the
/// drain owns the prompt's lifecycle, so there is no separate clear edit to race
/// this re-projection. Otherwise the edit ends at the prompt-line start, leaving
/// the user's in-progress prompt intact while the agent streams.
/// Returns `true` when an edit was applied (the transcript changed), `false`
/// when `last == new` and the buffer was left untouched. The drain uses this to
/// wake the render loop only when something actually changed.
async fn reproject(
    handle: &std::sync::Arc<dyn lattice_runtime::Document>,
    last: &str,
    new: &str,
    clear_prompt: bool,
) -> bool {
    let Some(rep) = suffix_replace(last, new) else {
        return false; // transcript text unchanged
    };
    let (end, replacement) = if clear_prompt {
        let snap = handle.snapshot();
        let last_line = snap.buffer.line_count().saturating_sub(1);
        let last_len = snap.buffer.line(last_line).unwrap_or_default().len() as u32;
        ((last_line, last_len), format!("{}{}", rep.replacement, PROMPT_MARKER))
    } else {
        (text_end(last), rep.replacement)
    };
    let range = lattice_protocol::Range::new(
        lattice_protocol::position::Position::new(rep.first_diff_line as u32, 0),
        lattice_protocol::position::Position::new(end.0, end.1),
    );
    let edit = lattice_protocol::edit::Edit::replace(range, replacement);
    let _ = handle.apply_edit_batch(vec![edit]).await;
    true
}

// ──────────────────────────────────────────────────────────────
// AU‑3: modal-input surface — keymap entries + action handlers
// ──────────────────────────────────────────────────────────────

/// The Normal- and Insert-mode chords the `ai-conversation` mode contributes.
/// Normal-mode insert-entering chords (`i`/`a`/`o`/`A`/`I`/`O`) all route to
/// `focus-prompt` so entering Insert always relocates the cursor into the
/// prompt — history is unreachable from Insert and so cannot be mutated.
/// `<CR>` sends; `<C-c>` interrupts.
fn ai_conversation_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        let focus = |chord: &'static str| keymap_entry! {
            mode: Normal, chord: chord,
            doc: "ai-conversation: move the cursor into the prompt and enter Insert",
            cmd: "action:ai-conv-focus-prompt"
        };
        vec![
            focus("i"),
            focus("a"),
            focus("o"),
            focus("A"),
            focus("I"),
            focus("O"),
            keymap_entry! {
                mode: Insert, chord: "<CR>",
                doc: "ai-conversation: send the prompt to the agent",
                cmd: "action:ai-conv-send"
            },
            keymap_entry! {
                mode: Insert, chord: "<C-c>",
                doc: "ai-conversation: interrupt the active turn",
                cmd: "action:ai-conv-interrupt"
            },
            // AU‑5: trust-mode toggle. `<C-t>` (Normal) mirrors the
            // agent-TUI convention of a single chord cycling the permission
            // mode (Claude Code's Shift-Tab / opencode's Tab); a Ctrl chord
            // is chosen for portable representation and is mode-scoped.
            keymap_entry! {
                mode: Normal, chord: "<C-t>",
                doc: "ai-conversation: toggle trust mode (auto-accept vs review)",
                cmd: "action:ai-conv-toggle-trust"
            },
        ]
    })
}

/// `action:ai-conv-focus-prompt` — place the cursor at the end of the prompt
/// line and enter Insert. Reuses the generic `SelectionChange` + `EnterMode`
/// effects (no new `Action`); reads the buffer through the `BufferStoreHandle`
/// service since the `ActionContext` carries no buffer text.
fn focus_prompt_handler() -> ActionHandler {
    Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
        let store = ctx.services.get::<BufferStoreHandle>()?;
        let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
        let handle = store.handle_for(buffer_id)?;
        let snap = handle.snapshot();
        let last_line = snap.buffer.line_count().saturating_sub(1);
        let end_byte = snap
            .buffer
            .line(last_line)
            .unwrap_or_default()
            .trim_end_matches('\n')
            .len() as u32;
        let pos = lattice_protocol::position::Position::new(last_line, end_byte);
        Some(Effect::Many(vec![
            Effect::SelectionChange(lattice_protocol::selection::SelectionSet::single(
                lattice_protocol::selection::Selection::cursor(pos),
            )),
            Effect::EnterMode(ModalState::Insert),
        ]))
    })
}

/// `action:ai-conv-send` — read the prompt (the tail line after `PROMPT_MARKER`)
/// and hand it to the agent via the `AiClientHandle` service, staying in Insert
/// at the prompt (only <Esc> returns to Normal). An empty prompt is a no-op
/// (Enter does nothing); with no running session it echoes an error and keeps
/// the prompt. The prompt clear + caret re-park are owned by the drain's
/// re-projection and the per-tick focus callback (see `on_activate`), not by
/// this handler — so there is no clear edit here to race the re-projection.
fn send_handler() -> ActionHandler {
    Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
        let store = ctx.services.get::<BufferStoreHandle>()?;
        let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
        let handle = store.handle_for(buffer_id)?;
        let snap = handle.snapshot();
        let last_line = snap.buffer.line_count().saturating_sub(1);
        let line = snap.buffer.line(last_line).unwrap_or_default();
        let line = line.trim_end_matches('\n');
        let prompt = line.strip_prefix(PROMPT_MARKER).unwrap_or(line);
        if prompt.trim().is_empty() {
            return None;
        }
        let ai = ctx.services.get::<AiClientHandle>()?;
        // Surface the "no running agent" case instead of silently dropping the
        // prompt: without a live session the supervisor discards `prompt(..)`, so
        // Enter would appear to do nothing. Stay in Insert with the prompt intact
        // so the user can retry after fixing the agent.
        if !ai.snapshot().running {
            return Some(Effect::Echo {
                level: EchoLevel::Error,
                text: "opencode: no running session — run :opencode; if it fails to \
                       start, see :ai-log"
                    .to_string(),
            });
        }
        ai.prompt(prompt.to_string());
        // AU‑3+: stay in Insert at the prompt (vim / `:terminal` / `:claude`
        // parity — only <Esc> returns to Normal). `<CR>` resolved to a mode
        // handler, so returning `None` consumes the key WITHOUT inserting a
        // newline, and leaves the modal state untouched (Insert). The prompt is
        // cleared and the caret re-parked at the fresh prompt by the drain's
        // re-projection + the per-tick focus callback (see `on_activate`), which
        // fire once the resulting User turn lands — NOT by a clear edit here (a
        // separate clear would race the re-projection that shifts the prompt
        // line down, and land on the wrong, now-read-only line).
        None
    })
}

/// `action:ai-conv-interrupt` — forward an interrupt to the agent (ACP
/// `session/cancel`) via the `AiClientHandle` service, without leaving Insert.
fn interrupt_handler() -> ActionHandler {
    Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
        if let Some(ai) = ctx.services.get::<AiClientHandle>() {
            ai.interrupt();
        }
        None
    })
}

/// `action:ai-conv-toggle-trust` (AU‑5) — flip trust mode via the
/// `AiClientHandle` service and echo the new state. No buffer edit; the
/// per-request permission tasks read the flag live.
fn toggle_trust_handler() -> ActionHandler {
    Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
        let ai = ctx.services.get::<AiClientHandle>()?;
        let on = ai.toggle_auto_accept();
        Some(Effect::Echo {
            level: EchoLevel::Info,
            text: if on {
                "ai: trust mode on — edits auto-accepted".to_string()
            } else {
                "ai: review mode — edits gated on diff review".to_string()
            },
        })
    })
}

/// AU‑3: register the `ai-conversation` action commands so the mode's keymap
/// `cmd` names resolve (the diff subsystem's `register_diff_actions` pattern).
/// The specs are pure shells returning `Effect::None`: the real bodies live in
/// [`AiConversationMode::action_handlers`], consulted before the CommandSpec.
pub fn register_ai_conversation_actions(registry: &mut lattice_grammar::CommandRegistry) {
    use lattice_grammar::registry::ActionSpec;
    for (name, doc) in [
        (
            "action:ai-conv-focus-prompt",
            "ai-conversation: move the cursor into the prompt and enter Insert.",
        ),
        ("action:ai-conv-send", "ai-conversation: send the prompt to the agent."),
        ("action:ai-conv-interrupt", "ai-conversation: interrupt the active turn."),
        (
            "action:ai-conv-toggle-trust",
            "ai-conversation: toggle trust mode (auto-accept vs diff review).",
        ),
    ] {
        registry.register_action(
            name,
            doc,
            ActionSpec {
                apply: Box::new(|_| Ok(Effect::None)),
                args_schema: vec![],
            },
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::acp::conversation::{Conversation, ToolStatus, Turn};

    fn text_turn(role: Role, text: &str) -> Turn {
        Turn {
            role,
            blocks: vec![Block::Text(text.to_string())],
        }
    }

    /// The drain clears the prompt exactly when the User-turn count rises (a
    /// send), so this count is the load-bearing signal for the clear-vs-preserve
    /// branch — agent turns must not trip it.
    #[test]
    fn user_turn_count_counts_only_user_turns() {
        let conv = Conversation {
            turns: vec![
                text_turn(Role::User, "refactor"),
                text_turn(Role::Assistant, "on it"),
                text_turn(Role::User, "also add tests"),
            ],
        };
        assert_eq!(user_turn_count(&conv), 2);
        assert_eq!(user_turn_count(&Conversation::default()), 0);
    }

    #[test]
    fn mode_id_is_ai_conversation_mode() {
        assert_eq!(
            AiConversationMode::mode_id(),
            ModeId::new("ai-conversation-mode")
        );
    }

    /// AU‑3: the mode declares a single-line prompt tail editable after the
    /// 2-byte `"> "` marker — the input the host's read-only gate consults.
    #[test]
    fn editable_tail_is_single_prompt_line() {
        assert_eq!(
            <AiConversationMode as Mode>::editable_tail(&AiConversationMode),
            Some(EditableTail { trailing_lines: 1, first_line_min_byte: 2 }),
        );
    }

    /// AU‑3: every Normal-mode insert-entering chord routes to `focus-prompt`
    /// (so Insert always relocates to the prompt), `<CR>` sends, `<C-c>`
    /// interrupts. Catches a dropped chord or a name swap.
    #[test]
    fn keymap_binds_insert_entry_to_focus_and_cr_to_send() {
        let pairs: Vec<(&str, Option<&str>)> = ai_conversation_keymap_entries()
            .iter()
            .map(|e| (e.chord, e.command))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("i", Some("action:ai-conv-focus-prompt")),
                ("a", Some("action:ai-conv-focus-prompt")),
                ("o", Some("action:ai-conv-focus-prompt")),
                ("A", Some("action:ai-conv-focus-prompt")),
                ("I", Some("action:ai-conv-focus-prompt")),
                ("O", Some("action:ai-conv-focus-prompt")),
                ("<CR>", Some("action:ai-conv-send")),
                ("<C-c>", Some("action:ai-conv-interrupt")),
                ("<C-t>", Some("action:ai-conv-toggle-trust")),
            ],
        );
    }

    /// AU‑3: the mode contributes exactly the three handler bodies, keyed to
    /// the SAME names the keymap binds, so the host's boot walk resolves each.
    #[test]
    fn action_handlers_contribute_focus_send_interrupt() {
        let names: Vec<&str> = AiConversationMode
            .action_handlers()
            .iter()
            .map(|c| c.action_name)
            .collect();
        assert_eq!(
            names,
            vec![
                "action:ai-conv-focus-prompt",
                "action:ai-conv-send",
                "action:ai-conv-interrupt",
                "action:ai-conv-toggle-trust",
            ],
        );
    }

    /// AU‑3/AU‑5: `register_ai_conversation_actions` registers every action
    /// command so the keymap `cmd` names resolve at boot.
    #[test]
    fn registers_the_action_commands() {
        let mut registry = lattice_grammar::CommandRegistry::new();
        register_ai_conversation_actions(&mut registry);
        for name in [
            "action:ai-conv-focus-prompt",
            "action:ai-conv-send",
            "action:ai-conv-interrupt",
            "action:ai-conv-toggle-trust",
        ] {
            assert!(registry.id_by_name(name).is_some(), "{name} registered");
        }
    }

    #[test]
    fn renders_turn_headers_and_blocks() {
        let conv = Conversation {
            turns: vec![
                text_turn(Role::User, "refactor parse_args"),
                Turn {
                    role: Role::Assistant,
                    blocks: vec![
                        Block::Text("I'll extract a helper.".to_string()),
                        Block::ToolCall {
                            id: "t1".to_string(),
                            title: "edit parse.rs".to_string(),
                            status: ToolStatus::Running,
                        },
                    ],
                },
            ],
        };
        let text = render_conversation(&conv);
        assert!(text.contains("you:\nrefactor parse_args\n"));
        assert!(text.contains("opencode:\nI'll extract a helper.\n"));
        assert!(text.contains("\u{25b8} edit parse.rs [running]"));
    }

    #[test]
    fn streaming_append_only_touches_the_tail() {
        let old = "opencode:\nhello\n\n";
        let new = "opencode:\nhello world\n\n";
        let rep = suffix_replace(old, new).expect("texts differ");
        // Line 0 ("opencode:") is unchanged; the first differing line is line 1.
        assert_eq!(rep.first_diff_line, 1);
        assert!(rep.replacement.starts_with("hello world"));
    }

    #[test]
    fn tool_status_change_rewrites_only_from_that_line() {
        let mut conv = Conversation {
            turns: vec![Turn {
                role: Role::Assistant,
                blocks: vec![
                    Block::Text("working".to_string()),
                    Block::ToolCall {
                        id: "t1".to_string(),
                        title: "run".to_string(),
                        status: ToolStatus::Running,
                    },
                ],
            }],
        };
        let before = render_conversation(&conv);
        if let Block::ToolCall { status, .. } = &mut conv.turns[0].blocks[1] {
            *status = ToolStatus::Ok;
        }
        let after = render_conversation(&conv);
        let rep = suffix_replace(&before, &after).expect("status changed");
        // "opencode:" (0), "working" (1) unchanged; the tool line (2) is first diff.
        assert_eq!(rep.first_diff_line, 2);
        assert!(rep.replacement.contains("[ok]"));
    }

    #[test]
    fn identical_render_yields_no_edit() {
        let t = "opencode:\nhi\n\n";
        assert_eq!(suffix_replace(t, t), None);
    }
}
