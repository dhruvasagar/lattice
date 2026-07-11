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

use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use agent_client_protocol::schema::v1::PermissionOptionKind;

use lattice_cells::{Cell, Headerline, HeaderlineProvider, HeaderlineRow, ProviderId, VirtualRowProvider};
use lattice_grammar::ModalState;
use lattice_grammar::effect::{EchoLevel, Effect};
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerContribution, BufferStoreHandle, CapabilitySet,
    EditableTail, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    OptionOverrideSet, Subscription,
    VirtualRowRegistrar, keymap_entry,
};

use crate::acp::conversation::{
    Block, Conversation, ConversationProjected, ConversationStore, ConversationUpdated,
    PermissionOutcome, PermissionStatus, Role,
};
use crate::acp::handle::AiClientHandle;

/// AUX‑2: provider id for the conversation headerline. Derived from a fixed
/// tag so the host can unregister it on buffer teardown.
const CONV_HEADERLINE_PROVIDER_ID: ProviderId = 0x4155_0002;

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
            let _ = write!(text, " \u{2502} CPU: {}", format_tokens(usage.used, usage.size));
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

fn humanize(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

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
///
/// Holds the prompt's editable-region `anchor`: the absolute line where the
/// prompt begins (== the transcript line count). The drain updates it on each
/// re-projection; [`editable_tail`](Mode::editable_tail) publishes it so the
/// read-only gate lets the user edit a multi-line prompt (`<C-j>` inserts a
/// newline) while the transcript above stays frozen. One instance backs the
/// single (v1) conversation buffer, so one shared anchor suffices.
///
/// AUX‑1: `current_permission_id` tracks the most recently projected pending
/// permission block, set by the drain on each re-projection. The allow/deny
/// action handlers read it to resolve the right permission request.
/// AUX‑2: `headerline_version` is bumped by the drain on each re-projection so
/// the `ConversationHeaderline` widget republishes its row when usage changes.
#[derive(Clone)]
pub struct AiConversationMode {
    anchor: Arc<AtomicU32>,
    current_permission_id: Arc<Mutex<Option<String>>>,
    headerline_version: Arc<AtomicU64>,
}

impl Default for AiConversationMode {
    fn default() -> Self {
        Self {
            anchor: Arc::new(AtomicU32::new(0)),
            current_permission_id: Arc::new(Mutex::new(None)),
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

/// The absolute line where the prompt begins for a given transcript: the
/// transcript's line count (each rendered turn ends in `\n`, so the trailing
/// `PROMPT_MARKER` lands on the next line). Pure — the seed and the drain both
/// feed this into the mode's anchor.
fn prompt_anchor_line(transcript: &str) -> u32 {
    transcript.matches('\n').count() as u32
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
                Block::Permission {
                    title,
                    description,
                    options,
                    status,
                    ..
                } => {
                    out.push_str("  ");
                    out.push_str(permission_prefix(status));
                    out.push_str(title);
                    out.push_str(" [");
                    out.push_str(permission_tag(status));
                    out.push_str("]\n");
                    if let Some(desc) = description {
                        out.push_str("    ");
                        out.push_str(desc);
                        out.push('\n');
                    }
                    if status == &PermissionStatus::Pending {
                        for (i, opt) in options.iter().enumerate() {
                            let key = match opt.kind {
                                PermissionOptionKind::AllowOnce => "a",
                                PermissionOptionKind::AllowAlways => "A",
                                PermissionOptionKind::RejectOnce => "r",
                                PermissionOptionKind::RejectAlways => "R",
                                _ => "?",
                            };
                            out.push_str(&format!(
                                "    {}: {} ({})\n",
                                i + 1,
                                opt.name,
                                key,
                            ));
                        }
                    }
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

fn permission_prefix(status: &PermissionStatus) -> &'static str {
    use PermissionStatus::*;
    match status {
        Pending => "\u{25cc} ", // ◌
        Allowed => "\u{2713} ", // ✓
        Denied => "\u{2717} ",  // ✗
    }
}

fn permission_tag(status: &PermissionStatus) -> &'static str {
    use PermissionStatus::*;
    match status {
        Pending => "pending",
        Allowed => "allowed",
        Denied => "denied",
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

/// AU‑3+ activation guard: the `ConversationUpdated` subscription. Dropping
/// it on deactivation unsubscribes, so a stopped mode contributes no further
/// work.
pub struct AiConversationGuard {
    _subscription: Subscription,
    /// AUX‑2: the headerline provider registration. Removed on deactivate so the
    /// mode owns its full surface — nothing else in the host knows this provider
    /// exists, so nothing else can clean it up.
    headerline: Option<(Arc<dyn VirtualRowRegistrar>, lattice_core::BufferId)>,
}

impl Drop for AiConversationGuard {
    fn drop(&mut self) {
        if let Some((registrar, buffer_id)) = self.headerline.take() {
            registrar.unregister(buffer_id, CONV_HEADERLINE_PROVIDER_ID);
        }
    }
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
            // Absolute anchor = the transcript-end line, kept current by the
            // drain. Lets the read-only gate cover a MULTI-line prompt (`<C-j>`):
            // every line at or below the anchor is editable, the transcript
            // above is frozen. The `trailing_lines: 1` above is the inert
            // fallback for the first frame before the anchor is seeded.
            first_editable_line: Some(self.anchor.load(Ordering::Relaxed)),
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
        let pid = self.current_permission_id.clone();
        vec![
            ActionHandlerContribution {
                action_name: "action:ai-conv-focus-prompt",
                handler: focus_prompt_handler(),
            },
            ActionHandlerContribution {
                action_name: "action:ai-conv-send",
                handler: send_handler(self.anchor.clone()),
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
            // AUX‑1: permission allow/deny
            ActionHandlerContribution {
                action_name: "action:ai-conv-allow",
                handler: permission_handler(pid.clone(), PermissionOutcome::AllowOnce),
            },
            ActionHandlerContribution {
                action_name: "action:ai-conv-allow-always",
                handler: permission_handler(pid.clone(), PermissionOutcome::AllowAlways),
            },
            ActionHandlerContribution {
                action_name: "action:ai-conv-deny",
                handler: permission_handler(pid.clone(), PermissionOutcome::DenyOnce),
            },
            ActionHandlerContribution {
                action_name: "action:ai-conv-deny-always",
                handler: permission_handler(pid, PermissionOutcome::DenyAlways),
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

            // AU‑3+ (`<C-j>` multi-line prompt): the editable-region anchor, the
            // absolute line where the prompt begins. `editable_tail` publishes it
            // to the read-only gate; the drain keeps it current as the transcript
            // streams. Seed it from the initial transcript.
            let anchor = self.anchor.clone();
            anchor.store(prompt_anchor_line(&last), Ordering::Relaxed);

            // AUX‑2: register the conversation headerline (token/cost display).
            // `conv_store` is `Arc<ConversationStore>` from the service lookup;
            // cloning the Arc gives another reference to the same store.
            let hl_version = self.headerline_version.clone();
            let headerline_registration = ctx
                .service::<Arc<dyn VirtualRowRegistrar>>()
                .map(|registrar| {
                    let queue_len = ctx
                        .service::<AiClientHandle>()
                        .map(|h| h.queue_len.clone())
                        .unwrap_or_default();
                    let headerline = ConversationHeaderline {
                        store: (*conv_store).clone(),
                        version: hl_version.clone(),
                        queue_len,
                    };
                    let provider = Arc::new(HeaderlineProvider::new(
                        CONV_HEADERLINE_PROVIDER_ID,
                        Arc::new(headerline),
                    ));
                    let registrar: Arc<dyn VirtualRowRegistrar> = (*registrar).clone();
                    // The provider id is a fixed tag, and `register` refuses to
                    // replace a live id. Clear any registration a previous
                    // activation left behind so a re-opened `:opencode` binds its
                    // own headerline rather than silently keeping the stale one.
                    registrar.unregister(buffer_id, CONV_HEADERLINE_PROVIDER_ID);
                    registrar.register(buffer_id, provider as Arc<dyn VirtualRowProvider>);
                    (registrar, buffer_id)
                });

            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ConversationUpdated>();
            let sub_id = ctx.events().subscribe_typed::<ConversationUpdated>(tx);
            let bus_handle = ctx.events_handle();
            // A separate handle for the drain to *publish* `ConversationProjected`
            // once its edit lands (see below).
            let projected_bus = ctx.events_handle();

            let drain_anchor = anchor;
            let drain_perm_id = self.current_permission_id.clone();
            let drain_hl_version = hl_version;
            runtime.spawn(async move {
                // Coalesce a burst of updates into one re-projection.
                while rx.recv().await.is_some() {
                    while rx.try_recv().is_ok() {}
                    let snap = conv_store.snapshot();
                    let new = render_conversation(&snap);
                    let user_turns = user_turn_count(&snap);
                    // AUX‑1: track the most recent pending permission block id
                    // so action handlers can resolve it.
                    *drain_perm_id.lock().expect("perm id mutex poisoned") = snap
                        .turns
                        .iter()
                        .rev()
                        .flat_map(|t| t.blocks.iter().rev())
                        .find_map(|b| match b {
                            Block::Permission { id, status: PermissionStatus::Pending, .. } => {
                                Some(id.clone())
                            }
                            _ => None,
                        });
                    // Keep the editable-region anchor on the transcript-end line:
                    // a re-projection may grow the transcript above the prompt
                    // (streaming) or reset it (send), moving where the prompt
                    // begins. Store before the edit so the gate is consistent
                    // with the new content on the next keystroke.
                    drain_anchor.store(prompt_anchor_line(&new), Ordering::Relaxed);
                    // A new User turn means the user just sent a prompt. Rewrite
                    // the tail through EOF and re-append an empty prompt marker,
                    // clearing the prompt *atomically* with the transcript update.
                    // The send handler deliberately emits no clear edit: a
                    // separate clear would race this re-projection (which shifts
                    // the prompt line down) and land on the wrong, now-read-only
                    // line. On agent-only updates we leave the user's in-progress
                    // prompt untouched.
                    let _did_clear = user_turns > last_user_turns;
                    let _changed = reproject(&handle, &last, &new, _did_clear).await;
                    // AUX‑2: bump the headerline version on every re-projection so
                    // the ConversationHeaderline widget republishes its row (usage
                    // may have changed).
                    drain_hl_version.fetch_add(1, Ordering::Release);

                    // Wake the editor actor now that the edit has LANDED, so the
                    // streamed response repaints (and the focus tick callback
                    // runs) WITHOUT a keystroke. Published after the await — not
                    // via `ConversationUpdated`, which fires before the buffer is
                    // re-projected — so the wake never repaints stale content.
                    //
                    // AUX‑2: published unconditionally, NOT gated on the
                    // transcript text changing. The headerline reads status,
                    // usage and queue length — none of which appear in the
                    // transcript — so a `usage_update` re-projects to identical
                    // text yet must still repaint. Gating this on text change
                    // froze the headerline at "Ready" forever.
                    projected_bus.publish_typed(ConversationProjected);
                    last = new;
                    last_user_turns = user_turns;
                }
            });

            Ok(Some(AiConversationGuard {
                _subscription: Subscription::new(bus_handle, sub_id),
                headerline: headerline_registration,
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
            // AUX‑1: permission allow/deny chords are NOT in the static keymap
            // because `a`/`A`/`r`/`R` conflict with existing focus-prompt and
            // other chords. They are available as ex-commands
            // (`:ai-allow` / `:ai-deny` etc.) and the action handlers are
            // registered for a future transient-keymap gate.
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

/// Join the prompt lines (`anchor..=last`) into the text to send: strip the
/// `PROMPT_MARKER` from the first (anchor) line, take continuation lines (added
/// via `<C-j>`) verbatim, and join with `\n`. Pure so the multi-line read is
/// unit-testable without a live buffer.
fn assemble_prompt<I: IntoIterator<Item = String>>(lines: I) -> String {
    lines
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let line = line.trim_end_matches('\n');
            if i == 0 {
                line.strip_prefix(PROMPT_MARKER).unwrap_or(line).to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `action:ai-conv-send` — read the full (possibly multi-line) prompt and hand
/// it to the agent via the `AiClientHandle` service, staying in Insert at the
/// prompt (only <Esc> returns to Normal). The prompt spans `anchor..EOF`: the
/// anchor line carries the `PROMPT_MARKER` (stripped), continuation lines from
/// `<C-j>` do not. An empty prompt is a no-op (Enter does nothing); with no
/// running session it echoes an error and keeps the prompt. The prompt clear +
/// caret re-park are owned by the drain's re-projection and the per-tick focus
/// callback (see `on_activate`), not by this handler — so there is no clear edit
/// here to race the re-projection.
fn send_handler(anchor: Arc<AtomicU32>) -> ActionHandler {
    Arc::new(move |ctx: &ActionContext<'_>| -> Option<Effect> {
        let store = ctx.services.get::<BufferStoreHandle>()?;
        let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
        let handle = store.handle_for(buffer_id)?;
        let snap = handle.snapshot();
        let last_line = snap.buffer.line_count().saturating_sub(1);
        // The prompt is `anchor..=last_line` (marker on the anchor line,
        // `<C-j>` continuation lines below).
        let anchor_line = anchor.load(Ordering::Relaxed).min(last_line);
        let prompt =
            assemble_prompt((anchor_line..=last_line).map(|l| snap.buffer.line(l).unwrap_or_default()));
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

/// `action:ai-conv-newline` — insert a literal newline at the cursor, growing
/// the multi-line prompt. Bound to `<C-j>` (Insert) because `<CR>` is taken by
/// send, matching the `:terminal` / `:claude` convention. Uses the generic
/// `Effect::ApplyEdit` edit primitive; the caret parks at column 0 of the new
/// line. The cursor is always within the prompt (Insert only edits the tail),
/// so the insert stays inside the editable region.
fn newline_handler() -> ActionHandler {
    Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
        let target = lattice_core::BufferId(ctx.buffer_id.0 as u32);
        let edit = lattice_protocol::edit::Edit::insert(ctx.cursor, "\n".to_string());
        Some(Effect::ApplyEdit {
            target,
            edit,
            cursor: Some(ctx.cursor.line + 1),
        })
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

// ──────────────────────────────────────────────────────────────
// AUX‑1: permission allow/deny action handlers
// ──────────────────────────────────────────────────────────────

/// Build an action handler that resolves the current pending permission with
/// `outcome`. No-op when no permission is pending.
fn permission_handler(
    current_id: Arc<Mutex<Option<String>>>,
    outcome: PermissionOutcome,
) -> ActionHandler {
    Arc::new(move |ctx: &ActionContext<'_>| -> Option<Effect> {
        let store = ctx.services.get::<ConversationStore>()?;
        let id = current_id.lock().ok()?.clone()?;
        store.resolve_permission(&id, outcome);
        None
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
        ("action:ai-conv-newline", "ai-conversation: insert a newline in the prompt."),
        ("action:ai-conv-interrupt", "ai-conversation: interrupt the active turn."),
        (
            "action:ai-conv-toggle-trust",
            "ai-conversation: toggle trust mode (auto-accept vs diff review).",
        ),
        (
            "action:ai-conv-allow",
            "ai-conversation: allow the pending permission once.",
        ),
        (
            "action:ai-conv-allow-always",
            "ai-conversation: allow the pending permission always.",
        ),
        (
            "action:ai-conv-deny",
            "ai-conversation: deny the pending permission once.",
        ),
        (
            "action:ai-conv-deny-always",
            "ai-conversation: deny the pending permission always.",
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
    use crate::acp::conversation::{Conversation, SessionStatus, ToolStatus, Turn};
    use lattice_agent::SessionKey;

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
            ..Default::default()
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

    /// AU‑3+ (`<C-j>`): the anchor is the transcript-end line — the number of
    /// newlines in the rendered transcript (each turn ends in `\n`, so the
    /// trailing prompt marker lands on the next line).
    #[test]
    fn prompt_anchor_line_is_transcript_line_count() {
        assert_eq!(prompt_anchor_line(""), 0); // empty ⇒ prompt on line 0
        assert_eq!(prompt_anchor_line("you:\nhi\n"), 2); // prompt on line 2
        assert_eq!(prompt_anchor_line("a\nb\nc\n"), 3);
    }

    /// AU‑3+ (`<C-j>`): the send read strips the marker from the first prompt
    /// line, keeps continuation lines verbatim, and joins with newlines.
    #[test]
    fn assemble_prompt_reads_multiline_prompt() {
        assert_eq!(assemble_prompt(["> hello".to_string()]), "hello");
        assert_eq!(
            assemble_prompt(["> line1".to_string(), "line2".to_string(), "line3".to_string()]),
            "line1\nline2\nline3",
        );
        // Per-line trailing newlines are trimmed; a bare (unmarked) first line
        // is taken as-is.
        assert_eq!(assemble_prompt(["> a\n".to_string(), "b\n".to_string()]), "a\nb");
        assert_eq!(assemble_prompt(["plain".to_string()]), "plain");
    }

    /// AU‑3: the mode declares a prompt tail editable after the 2-byte `"> "`
    /// marker — the input the host's read-only gate consults. The absolute
    /// anchor (seeded by the drain to the transcript-end line) starts at 0 for a
    /// fresh, empty conversation.
    #[test]
    fn editable_tail_is_anchored_prompt() {
        let mode = AiConversationMode::new();
        assert_eq!(
            <AiConversationMode as Mode>::editable_tail(&mode),
            Some(EditableTail {
                trailing_lines: 1,
                first_line_min_byte: 2,
                first_editable_line: Some(0),
            }),
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
                ("<C-j>", Some("action:ai-conv-newline")),
                ("<C-c>", Some("action:ai-conv-interrupt")),
                ("<C-t>", Some("action:ai-conv-toggle-trust")),
            ],
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
                "action:ai-conv-focus-prompt",
                "action:ai-conv-send",
                "action:ai-conv-newline",
                "action:ai-conv-interrupt",
                "action:ai-conv-toggle-trust",
                "action:ai-conv-allow",
                "action:ai-conv-allow-always",
                "action:ai-conv-deny",
                "action:ai-conv-deny-always",
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
            "action:ai-conv-allow",
            "action:ai-conv-allow-always",
            "action:ai-conv-deny",
            "action:ai-conv-deny-always",
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
                            kind: Default::default(),
                            input: None,
                            output: None,
                        },
                    ],
                },
            ],
            ..Default::default()
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

    // ── AUX‑1: permission block rendering tests ──

    use agent_client_protocol::schema::v1::{PermissionOption, PermissionOptionKind as POK};
    use crate::acp::conversation::PermissionStatus as PS;

    fn test_permission_option(id: &'static str, name: &'static str, kind: POK) -> PermissionOption {
        PermissionOption::new(id, name, kind)
    }

    #[test]
    fn render_permission_block_pending_shows_circle_and_options() {
        let conv = Conversation {
            turns: vec![Turn {
                role: Role::Assistant,
                blocks: vec![Block::Permission {
                    id: "perm-1".to_string(),
                    title: "Allow cargo test?".to_string(),
                    description: None,
                    options: vec![
                        test_permission_option("a1", "Allow once", POK::AllowOnce),
                        test_permission_option("r1", "Reject", POK::RejectOnce),
                    ],
                    status: PS::Pending,
                }],
            }],
            ..Default::default()
        };
        let text = render_conversation(&conv);
        assert!(text.contains("\u{25cc} Allow cargo test? [pending]"));
        assert!(text.contains("1: Allow once (a)"));
        assert!(text.contains("2: Reject (r)"));
    }

    #[test]
    fn render_permission_block_allowed_shows_checkmark() {
        let conv = Conversation {
            turns: vec![Turn {
                role: Role::Assistant,
                blocks: vec![Block::Permission {
                    id: "perm-1".to_string(),
                    title: "Allow cargo test?".to_string(),
                    description: None,
                    options: vec![],
                    status: PS::Allowed,
                }],
            }],
            ..Default::default()
        };
        let text = render_conversation(&conv);
        assert!(text.contains("\u{2713} Allow cargo test? [allowed]"));
        assert!(!text.contains("1:"), "no options after resolution");
    }

    #[test]
    fn render_permission_block_denied_shows_x() {
        let conv = Conversation {
            turns: vec![Turn {
                role: Role::Assistant,
                blocks: vec![Block::Permission {
                    id: "perm-1".to_string(),
                    title: "Allow cargo test?".to_string(),
                    description: None,
                    options: vec![],
                    status: PS::Denied,
                }],
            }],
            ..Default::default()
        };
        let text = render_conversation(&conv);
        assert!(text.contains("\u{2717} Allow cargo test? [denied]"));
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
        let text: String = row.cells.iter().map(|c| char::from_u32(c.codepoint).unwrap_or('�')).collect();
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
            store.apply(
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
        let text: String = row.cells.iter().map(|c| char::from_u32(c.codepoint).unwrap_or('�')).collect();
        assert!(text.contains("Ready"), "status prefix: {text}");
        assert!(text.contains("31.4K"), "headerline shows used tokens: {text}");
        assert!(text.contains("200.0K"), "headerline shows context size: {text}");
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
        store.apply(
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
            store.apply(
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
        let text: String = row.cells.iter().map(|c| char::from_u32(c.codepoint).unwrap_or('�')).collect();
        assert!(text.contains("Ready"), "status prefix: {text}");
        assert!(text.contains("CPU:"), "headerline has CPU prefix: {text}");
        assert!(text.contains("5.0K/16.0K"), "headerline shows tokens: {text}");
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
        let text: String = row.cells.iter().map(|c| char::from_u32(c.codepoint).unwrap_or('�')).collect();
        assert!(text.contains("Thinking"), "headerline shows Thinking: {text}");
    }

    #[test]
    fn headerline_shows_executing_status() {
        let store = {
            let store = ConversationStore::new(Arc::new(|_| {}));
            store.set_status(
                &SessionKey::new("test", 0),
                SessionStatus::Executing { tool: "edit parse.rs".into() },
            );
            store
        };
        let hl = ConversationHeaderline {
            store,
            version: Arc::new(AtomicU64::new(1)),
            queue_len: Arc::new(AtomicUsize::new(0)),
        };
        let row = hl.render().expect("headerline row");
        let text: String = row.cells.iter().map(|c| char::from_u32(c.codepoint).unwrap_or('�')).collect();
        assert!(text.contains("Working: edit parse.rs"), "headerline shows tool: {text}");
    }

    #[test]
    fn headerline_shows_awaiting_permission() {
        let store = {
            let store = ConversationStore::new(Arc::new(|_| {}));
            store.set_status(&SessionKey::new("test", 0), SessionStatus::AwaitingPermission);
            store
        };
        let hl = ConversationHeaderline {
            store,
            version: Arc::new(AtomicU64::new(1)),
            queue_len: Arc::new(AtomicUsize::new(0)),
        };
        let row = hl.render().expect("headerline row");
        let text: String = row.cells.iter().map(|c| char::from_u32(c.codepoint).unwrap_or('�')).collect();
        assert!(text.contains("Awaiting your approval"), "headerline shows awaiting: {text}");
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
        let text: String = row.cells.iter().map(|c| char::from_u32(c.codepoint).unwrap_or('�')).collect();
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
        let text: String = row.cells.iter().map(|c| char::from_u32(c.codepoint).unwrap_or('�')).collect();
        assert!(!text.contains("queued"), "no queued text when empty: {text}");
        assert!(text.contains("Ready"), "still shows status: {text}");
    }
}
