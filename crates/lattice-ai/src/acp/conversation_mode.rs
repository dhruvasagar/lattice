//! `ai-conversation` major mode (AU‑2).
//!
//! The read-only `*ai:opencode*` buffer backing the agent conversation view.
//! Mirrors `lattice_agent::log::modes::AiLogMode`: `on_activate` reads the
//! [`ConversationStore`](crate::conversation::ConversationStore) service,
//! seeds the buffer from the current snapshot, subscribes to
//! [`ConversationUpdated`](crate::conversation::ConversationUpdated), and
//! spawns a drain task that re-projects on every change. The returned guard
//! unsubscribes on drop.
//!
//! The buffer machinery (anchors, the line-granular re-projection, the drain,
//! the guard) is shared with the native `llm-chat` mode and lives in
//! [`crate::conversation::transcript`]; what stays here is what is ACP's: the
//! headerline's content, the send / interrupt / trust bodies that talk to the
//! agent, and the mode declaration.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use lattice_cells::{Cell, Headerline, HeaderlineRow};
use lattice_grammar::effect::{EchoLevel, Effect};
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerContribution, CapabilitySet, EditableTail, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet, ReplMode,
    keymap_entry,
};

use crate::acp::handle::AiClientHandle;
use crate::conversation::ConversationStore;
use crate::conversation::transcript::{
    self, PromptAnchors, TranscriptBinding, TranscriptGuard, humanize, newline_handler, read_prompt,
};
#[cfg(test)]
use crate::conversation::{Role, transcript::suffix_replace};

/// AUX‑2/4: headerline that reads the usage snapshot, processing status, and
/// queue length, and formats them as a sticky row above the buffer.
struct ConversationHeaderline {
    store: ConversationStore,
    version: Arc<AtomicU64>,
    queue_len: Arc<AtomicUsize>,
}

impl Headerline for ConversationHeaderline {
    fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    fn render(&self) -> Option<HeaderlineRow> {
        let snap = self.store.snapshot();
        let mut text = String::new();
        // AUX‑3: status prefix.
        text.push_str(&snap.status.to_string());
        // AUX‑4: queue count when prompts are queued.
        let ql = self.queue_len.load(Ordering::Relaxed);
        if ql > 0 {
            use std::fmt::Write;
            let _ = write!(text, " \u{231B} {} queued", ql); // ⌛ N queued
        }
        // AUX‑2: usage suffix (tokens/cost).
        if let Some(usage) = &snap.usage {
            use std::fmt::Write;
            let _ = write!(
                text,
                " \u{2502} CPU: {}",
                format_tokens(usage.used, usage.size)
            );
            if let Some(cost) = &usage.cost {
                let _ = write!(&mut text, " \u{00B7} ${:.3} {}", cost.amount, cost.currency);
            }
        }
        let cells: Arc<[Cell]> = text
            .chars()
            .map(|c| Cell::new(c as u32, 0, 0, 0))
            .collect::<Vec<_>>()
            .into();
        Some(HeaderlineRow { cells, bg: None })
    }
}

/// Format token count in human-readable form: `31.4K` or `1.2M`.
fn format_tokens(used: u64, size: u64) -> String {
    let used_s = humanize(used);
    let size_s = humanize(size);
    format!("{used_s}/{size_s}")
}

/// The synthetic buffer name for the (single, v1) opencode conversation.
pub fn conversation_buffer_name() -> String {
    "*ai:opencode*".to_string()
}

/// `ai-conversation-mode` -- major mode for the `*ai:opencode*` buffer.
///
/// Holds the prompt's editable-region anchors: per buffer, the absolute line
/// where the prompt begins (== the transcript line count). The drain updates
/// each on re-projection; [`editable_tail`](Mode::editable_tail) publishes it so
/// the read-only gate lets the user edit a multi-line prompt (`<C-j>` inserts a
/// newline) while the transcript above stays frozen. One instance backs every
/// conversation buffer; each buffer's guard removes its own entry on deactivate.
///
/// AUX‑2: `headerline_version` is bumped by the drain on each re-projection so
/// the `ConversationHeaderline` widget republishes its row when usage changes.
#[derive(Clone)]
pub struct AiConversationMode {
    anchors: PromptAnchors,
    headerline_version: Arc<AtomicU64>,
}

impl Default for AiConversationMode {
    fn default() -> Self {
        Self {
            anchors: PromptAnchors::default(),
            headerline_version: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl AiConversationMode {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn mode_id() -> ModeId {
        ModeId::new("ai-conversation-mode")
    }
}

/// The activation guard. Shared with the native chat mode.
pub type AiConversationGuard = TranscriptGuard;

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

    /// AU‑3 gap fix: claim the invocation runner so vim operators respect the
    /// editable tail. The host registers `Editor::run_editable_tail_invocation`
    /// under this mode id (`editor_boot.rs`); it gates mutating operators
    /// (`x` / `dd` / `dw`) started in the frozen transcript and lets them
    /// through in the prompt. Without a runner, operators fell through to the
    /// ungated document path and mutated the read-only transcript.
    fn invocation_runner(&self) -> Option<ModeId> {
        Some(Self::mode_id())
    }

    /// Pull in `repl-mode` on the conversation buffer: it owns the generic
    /// insert-entry surface (`i`/`a`/`o`/… → jump-to-prompt-and-Insert). The
    /// activation cascade turns it on wherever this major is active and off
    /// when the buffer's major changes, so the affordance is scoped to the
    /// REPL buffer without a bespoke keymap on this mode.
    fn implies(&self) -> &[ModeId] {
        ai_conversation_implies()
    }

    /// AU‑3: the prompt is the single trailing line, editable only after the
    /// `"> "` marker (2 bytes). Consulted by the host's read-only edit gate so
    /// Insert/operator keystrokes land only in the prompt; the transcript above
    /// stays owner-written. No anchor means this buffer is not (or no longer) a
    /// seeded conversation: fully read-only rather than another buffer's prompt.
    fn editable_tail(&self, buffer: lattice_core::BufferId) -> Option<EditableTail> {
        self.anchors.editable_tail(buffer)
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
                action_name: "action:ai-conv-send",
                handler: send_handler(self.anchors.clone()),
            },
            ActionHandlerContribution {
                action_name: "action:ai-conv-newline",
                handler: newline_handler(),
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

            // AUX‑2: the conversation headerline (token/cost display).
            let hl_version = self.headerline_version.clone();
            let queue_len = ctx
                .service::<AiClientHandle>()
                .map(|h| h.queue_len.clone())
                .unwrap_or_default();
            let headerline: Arc<dyn Headerline> = Arc::new(ConversationHeaderline {
                store: (*conv_store).clone(),
                version: hl_version.clone(),
                queue_len,
            });

            let guard = transcript::bind(TranscriptBinding {
                ctx: &ctx,
                buffer_id,
                handle,
                store: (*conv_store).clone(),
                anchors: self.anchors.clone(),
                headerline: Some(headerline),
                headerline_version: hl_version,
                only_session: None,
                runtime,
                projection: Default::default(),
            })
            .await;
            Ok(Some(guard))
        })
    }
}

// ──────────────────────────────────────────────────────────────
// AU‑3: modal-input surface — keymap entries + action handlers
// ──────────────────────────────────────────────────────────────

/// The minor modes `ai-conversation-mode` implies: `repl-mode` (the generic
/// REPL input surface, owner of the Normal-mode insert-entry chords
/// `i`/`a`/`o`/`A`/`I`/`O`). A process-wide static so [`Mode::implies`] can hand
/// back a `&'static [ModeId]`.
fn ai_conversation_implies() -> &'static [ModeId] {
    static IMPLIES: OnceLock<Vec<ModeId>> = OnceLock::new();
    IMPLIES.get_or_init(|| vec![ReplMode::mode_id()])
}

/// The Insert- and Normal-mode chords the `ai-conversation` mode contributes.
/// `<CR>` sends; `<C-j>` inserts a newline; `<C-c>` interrupts; `<C-t>` toggles
/// trust — all ACP-specific, so they stay here.
fn ai_conversation_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! {
                mode: Insert, chord: "<CR>",
                doc: "ai-conversation: send the prompt to the agent",
                cmd: "action:ai-conv-send"
            },
            // `<C-j>` inserts a literal newline in the prompt (multi-line
            // input), matching the `:terminal` / `:claude` convention — `<CR>`
            // is taken by send, so newline needs its own chord.
            keymap_entry! {
                mode: Insert, chord: "<C-j>",
                doc: "ai-conversation: insert a newline in the prompt",
                cmd: "action:ai-conv-newline"
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
            // PU-B: permission requests are decided in the `*ai-permission*`
            // popup menu (`ai-permission-mode`), opened by `:ai-permission` or
            // (PU-B.3) auto-opened — not by chords on the conversation buffer.
        ]
    })
}

/// `action:ai-conv-send` — read the full (possibly multi-line) prompt and hand
/// it to the agent via the `AiClientHandle` service, staying in Insert at the
/// prompt (only <Esc> returns to Normal). An empty prompt is a no-op (Enter does
/// nothing); a buffer with no seeded anchor sends nothing; with no running
/// session it echoes an error and keeps the prompt. The prompt clear + caret
/// re-park are owned by the drain's re-projection and the host's focus
/// handling, not by this handler — so there is no clear edit here to race the
/// re-projection (which shifts the prompt line down and would land the clear on
/// the wrong, now-read-only line).
fn send_handler(anchors: PromptAnchors) -> ActionHandler {
    Arc::new(move |ctx: &ActionContext<'_>| -> Option<Effect> {
        let prompt = read_prompt(ctx, &anchors)?;
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
        // `<CR>` resolved to a mode handler, so returning `None` consumes the key
        // WITHOUT inserting a newline, and leaves the modal state untouched
        // (Insert).
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
            "action:ai-conv-send",
            "ai-conversation: send the prompt to the agent.",
        ),
        (
            "action:ai-conv-newline",
            "ai-conversation: insert a newline in the prompt.",
        ),
        (
            "action:ai-conv-interrupt",
            "ai-conversation: interrupt the active turn.",
        ),
        (
            "action:ai-conv-toggle-trust",
            "ai-conversation: toggle trust mode (auto-accept vs diff review).",
        ),
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
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::acp::conversation::ApplySessionUpdateToStore;
    use crate::conversation::render_conversation;
    use crate::conversation::{Block, Conversation, SessionStatus, ToolStatus, Turn};
    use lattice_agent::SessionKey;

    #[test]
    fn mode_id_is_ai_conversation_mode() {
        assert_eq!(
            AiConversationMode::mode_id(),
            ModeId::new("ai-conversation-mode")
        );
    }

    /// AU‑3: the mode declares a prompt tail editable after the 2-byte `"> "`
    /// marker — the input the host's read-only gate consults. The absolute
    /// anchor (seeded by the drain to the transcript-end line) starts at 0 for a
    /// fresh, empty conversation.
    #[test]
    fn editable_tail_is_anchored_prompt() {
        let mode = AiConversationMode::new();
        let a = lattice_core::BufferId(1);
        // Unseeded buffer: fully read-only, never someone else's prompt.
        assert_eq!(<AiConversationMode as Mode>::editable_tail(&mode, a), None);
        mode.anchors.seed(a, 0);
        assert_eq!(
            <AiConversationMode as Mode>::editable_tail(&mode, a),
            Some(EditableTail {
                trailing_lines: 1,
                first_line_min_byte: 2,
                first_editable_line: Some(0),
            }),
        );
    }

    fn first_line(mode: &AiConversationMode, b: lattice_core::BufferId) -> Option<u32> {
        <AiConversationMode as Mode>::editable_tail(mode, b).and_then(|t| t.first_editable_line)
    }

    /// LL.2: one mode instance backs many conversation buffers; each keeps
    /// its own prompt anchor, streaming into one leaves the other alone, and
    /// closing one leaves no stale entry (a late drain cannot resurrect it).
    #[test]
    fn two_conversation_buffers_keep_their_own_prompt() {
        let mode = AiConversationMode::new();
        let (a, b) = (lattice_core::BufferId(1), lattice_core::BufferId(2));
        let gen_a = mode.anchors.seed(a, 3);
        let gen_b = mode.anchors.seed(b, 7);
        let guard_a = AiConversationGuard::for_test(&mode.anchors, a, gen_a);
        let _guard_b = AiConversationGuard::for_test(&mode.anchors, b, gen_b);

        // Streaming into A moves A's anchor only.
        mode.anchors.update(a, gen_a, 12);
        assert_eq!(first_line(&mode, a), Some(12));
        assert_eq!(first_line(&mode, b), Some(7));

        // Closing A removes exactly A's entry.
        drop(guard_a);
        assert_eq!(first_line(&mode, a), None);
        assert_eq!(first_line(&mode, b), Some(7));

        // A drain landing after deactivation must not resurrect it.
        mode.anchors.update(a, gen_a, 20);
        assert_eq!(first_line(&mode, a), None);
        assert_eq!(mode.anchors.len(), 1);
    }

    /// AU‑3 gap fix: the mode claims its own invocation runner so the host
    /// wires `Editor::run_editable_tail_invocation` (which gates vim operators
    /// like `x`/`dd` to the editable prompt) instead of falling through to the
    /// ungated document path. `editor_boot.rs` registers the runner fn under
    /// exactly this mode id.
    #[test]
    fn claims_invocation_runner_for_the_editable_tail_gate() {
        let mode = AiConversationMode::new();
        assert_eq!(
            <AiConversationMode as Mode>::invocation_runner(&mode),
            Some(AiConversationMode::mode_id()),
        );
    }

    /// The ai-conversation mode contributes only its ACP-specific chords:
    /// `<CR>` sends, `<C-j>` newline, `<C-c>` interrupts, `<C-t>` trust. The
    /// insert-entry keys (`i`/`a`/`o`/…) are NOT here — `repl-mode` (implied)
    /// owns that generic surface now. Catches a dropped chord or a name swap,
    /// and a regression that re-adds the insert-entry keys to this major.
    #[test]
    fn keymap_binds_only_acp_chords_not_insert_entry() {
        let pairs: Vec<(&str, Option<&str>)> = ai_conversation_keymap_entries()
            .iter()
            .map(|e| (e.chord, e.command))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("<CR>", Some("action:ai-conv-send")),
                ("<C-j>", Some("action:ai-conv-newline")),
                ("<C-c>", Some("action:ai-conv-interrupt")),
                ("<C-t>", Some("action:ai-conv-toggle-trust")),
            ],
        );
        assert!(
            !pairs
                .iter()
                .any(|(chord, _)| matches!(*chord, "i" | "a" | "o" | "A" | "I" | "O")),
            "insert-entry keys must live in repl-mode, not the ai-conversation major",
        );
    }

    /// The major implies `repl-mode` — that is what brings the generic
    /// insert-entry surface onto the conversation buffer.
    #[test]
    fn implies_repl_mode() {
        let mode = AiConversationMode::new();
        assert_eq!(
            <AiConversationMode as Mode>::implies(&mode),
            &[ReplMode::mode_id()],
        );
    }

    /// AU‑3: the mode contributes exactly the handler bodies, keyed to the SAME
    /// names the keymap binds, so the host's boot walk resolves each.
    #[test]
    fn action_handlers_contribute_focus_send_interrupt() {
        let names: Vec<&str> = AiConversationMode::new()
            .action_handlers()
            .iter()
            .map(|c| c.action_name)
            .collect();
        assert_eq!(
            names,
            vec![
                "action:ai-conv-send",
                "action:ai-conv-newline",
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
            "action:ai-conv-send",
            "action:ai-conv-interrupt",
            "action:ai-conv-toggle-trust",
        ] {
            assert!(registry.id_by_name(name).is_some(), "{name} registered");
        }
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
                        origin: None,
                        title: "run".to_string(),
                        status: ToolStatus::Running,
                        kind: Default::default(),
                        input: None,
                        output: None,
                    },
                ],
            }],
            ..Default::default()
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

    // ── AUX‑2: headerline / humanize tests ──

    #[test]
    fn humanize_thousands() {
        assert_eq!(humanize(0), "0");
        assert_eq!(humanize(500), "500");
        assert_eq!(humanize(1000), "1.0K");
        assert_eq!(humanize(31400), "31.4K");
        assert_eq!(humanize(200000), "200.0K");
    }

    #[test]
    fn humanize_millions() {
        assert_eq!(humanize(1_000_000), "1.0M");
        assert_eq!(humanize(2_500_000), "2.5M");
    }

    #[test]
    fn format_tokens_joins_used_and_size() {
        assert_eq!(format_tokens(31400, 200000), "31.4K/200.0K");
        assert_eq!(format_tokens(0, 1000), "0/1.0K");
    }

    #[test]
    fn headerline_always_shows_status_without_usage() {
        let hl = ConversationHeaderline {
            store: ConversationStore::new(Arc::new(|_| {})),
            version: Arc::new(AtomicU64::new(0)),
            queue_len: Arc::new(AtomicUsize::new(0)),
        };
        let row = hl.render().expect("status always present → headerline row");
        let text: String = row
            .cells
            .iter()
            .map(|c| char::from_u32(c.codepoint).unwrap_or('�'))
            .collect();
        assert!(text.contains("Ready"), "status shows Idle→Ready: {text}");
        assert!(!text.contains("CPU:"), "no usage → no CPU segment: {text}");
    }

    #[test]
    fn headerline_shows_status_and_usage() {
        let store = {
            let published = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let p = published.clone();
            let store = ConversationStore::new(Arc::new(move |_| {
                p.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }));
            let u = agent_client_protocol::schema::v1::UsageUpdate::new(31400, 200000)
                .cost(agent_client_protocol::schema::v1::Cost::new(0.045, "USD"));
            store.apply_update(
                &SessionKey::new("test", 0),
                &agent_client_protocol::schema::v1::SessionUpdate::UsageUpdate(u),
            );
            store
        };
        let hl = ConversationHeaderline {
            store,
            version: Arc::new(AtomicU64::new(1)),
            queue_len: Arc::new(AtomicUsize::new(0)),
        };
        let row = hl.render().expect("headerline row");
        let text: String = row
            .cells
            .iter()
            .map(|c| char::from_u32(c.codepoint).unwrap_or('�'))
            .collect();
        assert!(text.contains("Ready"), "status prefix: {text}");
        assert!(
            text.contains("31.4K"),
            "headerline shows used tokens: {text}"
        );
        assert!(
            text.contains("200.0K"),
            "headerline shows context size: {text}"
        );
        assert!(text.contains("$0.045"), "headerline shows cost: {text}");
        assert!(text.contains("USD"), "headerline shows currency: {text}");
    }

    /// Why the drain must not gate its repaint wake on transcript-text change:
    /// a `usage_update` moves the headerline but leaves `render_conversation`
    /// byte-identical. Gating the wake on text change (the AUX‑2 bug) freezes
    /// the headerline at its last text-driven repaint — "Ready", never a cost.
    #[test]
    fn usage_only_update_moves_headerline_but_not_transcript() {
        fn row_text(hl: &ConversationHeaderline) -> String {
            hl.render()
                .expect("row")
                .cells
                .iter()
                .map(|c| char::from_u32(c.codepoint).unwrap_or('�'))
                .collect()
        }

        let store = ConversationStore::new(Arc::new(|_| {}));
        store.push_user_text(&SessionKey::new("test", 0), "hello");
        let hl = ConversationHeaderline {
            store: store.clone(),
            version: Arc::new(AtomicU64::new(0)),
            queue_len: Arc::new(AtomicUsize::new(0)),
        };

        let transcript_before = render_conversation(&store.snapshot());
        let headerline_before = row_text(&hl);

        let u = agent_client_protocol::schema::v1::UsageUpdate::new(31400, 200000)
            .cost(agent_client_protocol::schema::v1::Cost::new(0.045, "USD"));
        store.apply_update(
            &SessionKey::new("test", 0),
            &agent_client_protocol::schema::v1::SessionUpdate::UsageUpdate(u),
        );

        assert_eq!(
            transcript_before,
            render_conversation(&store.snapshot()),
            "usage never appears in the transcript text",
        );
        assert_ne!(
            headerline_before,
            row_text(&hl),
            "usage DOES change the headerline — so a text-unchanged \
             re-projection still has to wake the renderer",
        );
        assert!(row_text(&hl).contains("31.4K"), "{}", row_text(&hl));
    }

    #[test]
    fn headerline_omits_cost_when_missing() {
        let store = {
            let store = ConversationStore::new(Arc::new(|_| {}));
            let u = agent_client_protocol::schema::v1::UsageUpdate::new(5000, 16000);
            store.apply_update(
                &SessionKey::new("test", 0),
                &agent_client_protocol::schema::v1::SessionUpdate::UsageUpdate(u),
            );
            store
        };
        let hl = ConversationHeaderline {
            store,
            version: Arc::new(AtomicU64::new(1)),
            queue_len: Arc::new(AtomicUsize::new(0)),
        };
        let row = hl.render().expect("usage present");
        let text: String = row
            .cells
            .iter()
            .map(|c| char::from_u32(c.codepoint).unwrap_or('�'))
            .collect();
        assert!(text.contains("Ready"), "status prefix: {text}");
        assert!(text.contains("CPU:"), "headerline has CPU prefix: {text}");
        assert!(
            text.contains("5.0K/16.0K"),
            "headerline shows tokens: {text}"
        );
        assert!(!text.contains("$"), "no cost segment: {text}");
    }

    // ── AUX‑3: status-in-headerline tests ──

    #[test]
    fn headerline_shows_thinking_status() {
        let store = {
            let store = ConversationStore::new(Arc::new(|_| {}));
            store.set_status(&SessionKey::new("test", 0), SessionStatus::Thinking);
            store
        };
        let hl = ConversationHeaderline {
            store,
            version: Arc::new(AtomicU64::new(1)),
            queue_len: Arc::new(AtomicUsize::new(0)),
        };
        let row = hl.render().expect("headerline row");
        let text: String = row
            .cells
            .iter()
            .map(|c| char::from_u32(c.codepoint).unwrap_or('�'))
            .collect();
        assert!(
            text.contains("Thinking"),
            "headerline shows Thinking: {text}"
        );
    }

    #[test]
    fn headerline_shows_executing_status() {
        let store = {
            let store = ConversationStore::new(Arc::new(|_| {}));
            store.set_status(
                &SessionKey::new("test", 0),
                SessionStatus::Executing {
                    tool: "edit parse.rs".into(),
                },
            );
            store
        };
        let hl = ConversationHeaderline {
            store,
            version: Arc::new(AtomicU64::new(1)),
            queue_len: Arc::new(AtomicUsize::new(0)),
        };
        let row = hl.render().expect("headerline row");
        let text: String = row
            .cells
            .iter()
            .map(|c| char::from_u32(c.codepoint).unwrap_or('�'))
            .collect();
        assert!(
            text.contains("Working: edit parse.rs"),
            "headerline shows tool: {text}"
        );
    }

    #[test]
    fn headerline_shows_awaiting_permission() {
        let store = {
            let store = ConversationStore::new(Arc::new(|_| {}));
            store.set_status(
                &SessionKey::new("test", 0),
                SessionStatus::AwaitingPermission,
            );
            store
        };
        let hl = ConversationHeaderline {
            store,
            version: Arc::new(AtomicU64::new(1)),
            queue_len: Arc::new(AtomicUsize::new(0)),
        };
        let row = hl.render().expect("headerline row");
        let text: String = row
            .cells
            .iter()
            .map(|c| char::from_u32(c.codepoint).unwrap_or('�'))
            .collect();
        assert!(
            text.contains("Awaiting your approval"),
            "headerline shows awaiting: {text}"
        );
    }

    // ── AUX‑4: queue-in-headerline tests ──

    #[test]
    fn headerline_shows_queue_count() {
        let hl = ConversationHeaderline {
            store: ConversationStore::new(Arc::new(|_| {})),
            version: Arc::new(AtomicU64::new(0)),
            queue_len: Arc::new(AtomicUsize::new(2)),
        };
        let row = hl.render().expect("headerline row");
        let text: String = row
            .cells
            .iter()
            .map(|c| char::from_u32(c.codepoint).unwrap_or('�'))
            .collect();
        assert!(text.contains("⌛"), "queue icon present: {text}");
        assert!(text.contains("2 queued"), "queue count shown: {text}");
    }

    #[test]
    fn headerline_hides_queue_when_empty() {
        let hl = ConversationHeaderline {
            store: ConversationStore::new(Arc::new(|_| {})),
            version: Arc::new(AtomicU64::new(0)),
            queue_len: Arc::new(AtomicUsize::new(0)),
        };
        let row = hl.render().expect("headerline row");
        let text: String = row
            .cells
            .iter()
            .map(|c| char::from_u32(c.codepoint).unwrap_or('�'))
            .collect();
        assert!(
            !text.contains("queued"),
            "no queued text when empty: {text}"
        );
        assert!(text.contains("Ready"), "still shows status: {text}");
    }
}
