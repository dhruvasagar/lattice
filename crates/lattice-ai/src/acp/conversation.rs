//! Structured conversation model for the ACP agent UI (AU‑1).
//!
//! The ACP supervisor receives structured `SessionUpdate`s (message chunks,
//! thought chunks, tool calls, tool-call updates). AU‑1 stops flattening the
//! *conversation* ones to text `AiLogger` records and instead folds them into a
//! [`Conversation`] — a turn/block tree the `ai-conversation` mode (AU‑2)
//! projects into the `*ai:opencode*` buffer. Trace sources (`Client` /
//! `Lifecycle`) still flow to `AiLogger`; this completes the conversation/trace
//! split.
//!
//! [`Conversation::apply`] is pure and unit-testable (no I/O, no locks).
//! [`ConversationStore`] wraps it with a shared mutex + a [`ConversationUpdated`]
//! bus publish so the mode can live-tail.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::v1::{
    ContentBlock, PermissionOption, PermissionOptionId, PermissionOptionKind, SessionUpdate,
    ToolCallStatus, ToolKind,
};
use std::fmt;
use tokio::sync::oneshot;

use lattice_agent::SessionKey;

/// Who produced a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// Execution state of a tool call, mapped from ACP's `ToolCallStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Pending,
    Running,
    Ok,
    Err,
}

impl ToolStatus {
    fn from_acp(status: ToolCallStatus) -> Self {
        match status {
            ToolCallStatus::Pending => ToolStatus::Pending,
            ToolCallStatus::InProgress => ToolStatus::Running,
            ToolCallStatus::Completed => ToolStatus::Ok,
            ToolCallStatus::Failed => ToolStatus::Err,
            // `ToolCallStatus` is `#[non_exhaustive]`.
            _ => ToolStatus::Pending,
        }
    }
}

/// Review state of an agent-proposed file edit. AU‑4 drives the transitions and
/// attaches the `review_diff` session; AU‑1 only defines the shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditStatus {
    Proposed,
    Accepted,
    Rejected,
}

/// AUX‑1: status of an inline permission request shown in the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionStatus {
    Pending,
    Allowed,
    Denied,
}

/// AUX‑3: global processing status of the current agent session — shown in the
/// conversation buffer's headerline.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SessionStatus {
    /// No active turn: the agent is not processing anything.
    #[default]
    Idle,
    /// The agent is streaming a text/thought response.
    Thinking,
    /// The agent is executing a tool call.
    Executing {
        /// Human-readable tool name (e.g. "edit parse.rs").
        tool: String,
    },
    /// The agent is awaiting the user's decision on a permission request.
    AwaitingPermission,
}

impl fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionStatus::Idle => write!(f, "Ready"),
            SessionStatus::Thinking => write!(f, "Thinking\u{2026}"),
            SessionStatus::Executing { tool } => write!(f, "Working: {tool}"),
            SessionStatus::AwaitingPermission => write!(f, "Awaiting your approval\u{2026}"),
        }
    }
}

/// AUX‑2: token usage snapshot from a `UsageUpdate` notification.
#[derive(Debug, Clone, PartialEq)]
pub struct Cost {
    pub amount: f64,
    pub currency: String,
}

/// AUX‑2: latest token and cost snapshot from the ACP `usage_update`.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageSnapshot {
    pub used: u64,
    pub size: u64,
    pub cost: Option<Cost>,
}

/// One renderable unit within a turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// Streamed message text.
    Text(String),
    /// Streamed reasoning / thinking. TCF: the projection prefixes each line
    /// with `│`, and a multi-line block is folded **closed by default** via
    /// [`ReasoningFoldSource`](crate::acp::tool_fold::ReasoningFoldSource) — the
    /// fold keeps the head line visible and hides the rest until `za`. (Before
    /// TCF this comment claimed folding the projection did not do.)
    Reasoning(String),
    /// A tool invocation and its live status.
    ///
    /// TCF: `kind`, `input` and `output` are the detail an expanded tool call
    /// shows. `input`/`output` are the agent's raw JSON, pretty-printed at
    /// ingest (stored as `String`, not `serde_json::Value`, so `Block` stays
    /// `Eq`). Detail commonly arrives on a later `ToolCallUpdate`, not the
    /// initial call, so it is merged in as it lands.
    ToolCall {
        id: String,
        title: String,
        status: ToolStatus,
        kind: ToolKind,
        input: Option<String>,
        output: Option<String>,
    },
    /// An agent-proposed file edit (AU‑4 wires the diff review).
    Edit { path: String, status: EditStatus },
    /// AUX‑1: an inline permission request awaiting or reflecting user action.
    Permission {
        id: String,
        title: String,
        description: Option<String>,
        options: Vec<PermissionOption>,
        status: PermissionStatus,
    },
}

/// One turn: a role and its ordered blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub role: Role,
    pub blocks: Vec<Block>,
}

/// The full conversation as a turn/block tree. Streaming *extends* the last
/// block; earlier turns are never rewritten.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Conversation {
    pub turns: Vec<Turn>,
    /// AUX‑2: latest token usage snapshot from `usage_update` notifications.
    pub usage: Option<UsageSnapshot>,
    /// AUX‑3: global processing status derived by the supervisor from the active
    /// turn's state. Set via [`ConversationStore::set_status`].
    pub status: SessionStatus,
}

impl Conversation {
    /// Fold one ACP `SessionUpdate` into the model. Pure: no I/O, no locks.
    /// Non-conversation updates (plans, mode changes, ...) are ignored;
    /// `SessionUpdate` and `ContentBlock` are `#[non_exhaustive]`, so a
    /// catch-all closes each match.
    pub fn apply(&mut self, update: &SessionUpdate) {
        match update {
            SessionUpdate::UserMessageChunk(chunk) => {
                if let ContentBlock::Text(t) = &chunk.content {
                    self.extend_text(Role::User, &t.text);
                }
            }
            SessionUpdate::AgentMessageChunk(chunk) => {
                if let ContentBlock::Text(t) = &chunk.content {
                    self.extend_text(Role::Assistant, &t.text);
                }
            }
            SessionUpdate::AgentThoughtChunk(chunk) => {
                if let ContentBlock::Text(t) = &chunk.content {
                    self.extend_reasoning(&t.text);
                }
            }
            SessionUpdate::ToolCall(tc) => {
                self.push_tool_call(
                    tc.tool_call_id.0.to_string(),
                    tc.title.clone(),
                    ToolStatus::from_acp(tc.status),
                    tc.kind,
                    tc.raw_input.as_ref().map(pretty_json),
                    tc.raw_output.as_ref().map(pretty_json),
                );
            }
            SessionUpdate::ToolCallUpdate(u) => {
                self.merge_tool_update(
                    &u.tool_call_id.0.to_string(),
                    u.fields.status.map(ToolStatus::from_acp),
                    u.fields.kind,
                    u.fields.raw_input.as_ref().map(pretty_json),
                    u.fields.raw_output.as_ref().map(pretty_json),
                );
            }
            // AUX‑2: accumulate the latest usage snapshot.
            SessionUpdate::UsageUpdate(u) => {
                self.usage = Some(UsageSnapshot {
                    used: u.used,
                    size: u.size,
                    cost: u.cost.as_ref().map(|c| Cost {
                        amount: c.amount,
                        currency: c.currency.clone(),
                    }),
                });
            }
            _ => {}
        }
    }

    /// AU‑3: append a complete user prompt as a new `User` turn. Each Enter is
    /// a distinct turn (the terminal-REPL model), so — unlike chunk-streamed
    /// agent output — this always opens a fresh turn rather than extending.
    pub fn push_user_text(&mut self, text: &str) {
        self.turns.push(Turn {
            role: Role::User,
            blocks: vec![Block::Text(text.to_string())],
        });
    }

    /// Append `text` to the last block if it is `Text` in a turn of `role`;
    /// otherwise open a new block / turn as needed.
    fn extend_text(&mut self, role: Role, text: &str) {
        match self.turns.last_mut() {
            Some(turn) if turn.role == role => match turn.blocks.last_mut() {
                Some(Block::Text(s)) => s.push_str(text),
                _ => turn.blocks.push(Block::Text(text.to_string())),
            },
            _ => self.turns.push(Turn {
                role,
                blocks: vec![Block::Text(text.to_string())],
            }),
        }
    }

    /// Append `text` to the last `Reasoning` block (assistant turn), opening one
    /// as needed.
    fn extend_reasoning(&mut self, text: &str) {
        match self.turns.last_mut() {
            Some(turn) if turn.role == Role::Assistant => match turn.blocks.last_mut() {
                Some(Block::Reasoning(s)) => s.push_str(text),
                _ => turn.blocks.push(Block::Reasoning(text.to_string())),
            },
            _ => self.turns.push(Turn {
                role: Role::Assistant,
                blocks: vec![Block::Reasoning(text.to_string())],
            }),
        }
    }

    /// Push a new `ToolCall` block onto the current (or a fresh) assistant turn.
    #[allow(clippy::too_many_arguments)]
    fn push_tool_call(
        &mut self,
        id: String,
        title: String,
        status: ToolStatus,
        kind: ToolKind,
        input: Option<String>,
        output: Option<String>,
    ) {
        let block = Block::ToolCall {
            id,
            title,
            status,
            kind,
            input,
            output,
        };
        match self.turns.last_mut() {
            Some(turn) if turn.role == Role::Assistant => turn.blocks.push(block),
            _ => self.turns.push(Turn {
                role: Role::Assistant,
                blocks: vec![block],
            }),
        }
    }

    /// AUX‑1: push a `Permission` block onto the current (or a fresh) assistant turn.
    fn push_permission_block(
        &mut self,
        id: String,
        title: String,
        description: Option<String>,
        options: Vec<PermissionOption>,
    ) {
        let block = Block::Permission {
            id,
            title,
            description,
            options,
            status: PermissionStatus::Pending,
        };
        match self.turns.last_mut() {
            Some(turn) if turn.role == Role::Assistant => turn.blocks.push(block),
            _ => self.turns.push(Turn {
                role: Role::Assistant,
                blocks: vec![block],
            }),
        }
    }

    /// PU-B.2: kind of the option `option_id` within the `Permission` block
    /// `id` (searched newest-first), or `None` if the block or option is gone.
    /// Used by `resolve_permission` to derive the inline block status from the
    /// agent's actual option rather than a fixed 4-way bucket.
    fn permission_option_kind(
        &self,
        id: &str,
        option_id: &PermissionOptionId,
    ) -> Option<PermissionOptionKind> {
        for turn in self.turns.iter().rev() {
            for block in turn.blocks.iter().rev() {
                if let Block::Permission {
                    id: bid, options, ..
                } = block
                    && bid == id
                {
                    return options
                        .iter()
                        .find(|o| &o.option_id == option_id)
                        .map(|o| o.kind);
                }
            }
        }
        None
    }

    /// AUX‑1: update the status of the `Permission` block with `id`.
    fn update_permission_status(&mut self, id: &str, new_status: PermissionStatus) {
        for turn in self.turns.iter_mut().rev() {
            for block in turn.blocks.iter_mut().rev() {
                if let Block::Permission {
                    id: bid, status, ..
                } = block
                    && bid == id
                {
                    *status = new_status;
                    return;
                }
            }
        }
    }

    /// TCF: merge a `ToolCallUpdate` into the tool-call block with `id`
    /// (searched newest-first). Each `Some` field overwrites; `None` leaves the
    /// existing value — detail (input/output/kind) commonly arrives on an
    /// update after the initial call, so this accumulates rather than replaces.
    fn merge_tool_update(
        &mut self,
        id: &str,
        new_status: Option<ToolStatus>,
        new_kind: Option<ToolKind>,
        new_input: Option<String>,
        new_output: Option<String>,
    ) {
        for turn in self.turns.iter_mut().rev() {
            for block in turn.blocks.iter_mut().rev() {
                if let Block::ToolCall {
                    id: bid,
                    status,
                    kind,
                    input,
                    output,
                    ..
                } = block
                    && bid == id
                {
                    if let Some(s) = new_status {
                        *status = s;
                    }
                    if let Some(k) = new_kind {
                        *kind = k;
                    }
                    if new_input.is_some() {
                        *input = new_input;
                    }
                    if new_output.is_some() {
                        *output = new_output;
                    }
                    return;
                }
            }
        }
    }
}

/// TCF: pretty-print a raw tool JSON payload for the expanded view. Falls back
/// to the compact `Display` form if pretty-printing somehow fails.
fn pretty_json(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// Fired after the [`ConversationStore`] mutates. The `ai-conversation` mode
/// subscribes to it and re-projects (mirrors `AiLogPushed`).
#[derive(Debug, Clone)]
pub struct ConversationUpdated {
    pub session: SessionKey,
}

lattice_protocol::register_event!(
    ConversationUpdated,
    "ai.conversation-updated",
    "Fired after the ACP agent conversation model changes; drives live refresh \
     of the *ai:<provider>* conversation buffer.",
    "lattice-ai",
);

/// Fired by the `ai-conversation` mode's drain AFTER a re-projection edit has
/// LANDED in the buffer. Boot wakes the editor actor on this (via
/// `wake_on_event`) so a streamed agent response repaints WITHOUT a keystroke,
/// and the per-tick prompt-focus callback runs.
///
/// Distinct from [`ConversationUpdated`], which the supervisor fires when the
/// *model* changes — that is BEFORE the drain re-projects the buffer, so waking
/// on it would repaint stale content. Sequencing the wake after the owner-write
/// edit (this event) is what makes the last streamed chunk paint reliably.
#[derive(Debug, Clone, Default)]
pub struct ConversationProjected;

lattice_protocol::register_event!(
    ConversationProjected,
    "ai.conversation-projected",
    "Fired after the *ai:<provider>* conversation buffer is re-projected; wakes \
     the render loop so streamed agent responses repaint without a keystroke.",
    "lattice-ai",
);

/// Shared, mutable conversation state plus a bus publisher. The supervisor holds
/// one clone and calls [`ConversationStore::apply`]; the `ai-conversation` mode
/// reads [`ConversationStore::snapshot`] on each `ConversationUpdated`.
#[derive(Clone)]
pub struct ConversationStore {
    inner: Arc<Mutex<Conversation>>,
    publish: Arc<dyn Fn(ConversationUpdated) + Send + Sync>,
    /// AUX‑1: pending permission request responders keyed by tool-call id.
    /// PU-B.2: the oneshot carries the agent's chosen `PermissionOptionId`
    /// (wire order, any arity), not the fixed 4-way `PermissionOutcome` — the
    /// menu resolves by the option the agent actually offered.
    pending_permissions:
        Arc<Mutex<HashMap<String, (SessionKey, oneshot::Sender<PermissionOptionId>)>>>,
}

impl ConversationStore {
    /// Build a store whose mutations publish `ConversationUpdated` via `publish`.
    pub fn new(publish: Arc<dyn Fn(ConversationUpdated) + Send + Sync>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Conversation::default())),
            publish,
            pending_permissions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Fold `update` into the conversation for `session`, then publish.
    pub fn apply(&self, session: &SessionKey, update: &SessionUpdate) {
        {
            let mut conv = self.inner.lock().expect("conversation mutex poisoned");
            conv.apply(update);
        }
        (self.publish)(ConversationUpdated {
            session: session.clone(),
        });
    }

    /// AU‑3: fold a locally-composed user prompt into the conversation as a
    /// `User` turn, then publish. ACP agents don't echo the user's prompt back,
    /// so the supervisor calls this when it sends a prompt to make the user's
    /// turn appear in the transcript immediately.
    pub fn push_user_text(&self, session: &SessionKey, text: &str) {
        {
            let mut conv = self.inner.lock().expect("conversation mutex poisoned");
            conv.push_user_text(text);
        }
        (self.publish)(ConversationUpdated {
            session: session.clone(),
        });
    }

    /// AUX‑1: push a permission block and register its oneshot responder. The
    /// supervisor calls this when `classify_permission` returns `AskUser`; the
    /// receiver is awaited in [`handle_permission`](supervisor::handle_permission).
    pub fn push_permission_request(
        &self,
        session: &SessionKey,
        id: String,
        title: String,
        description: Option<String>,
        options: Vec<PermissionOption>,
        responder: oneshot::Sender<PermissionOptionId>,
    ) {
        {
            let mut conv = self.inner.lock().expect("conversation mutex poisoned");
            conv.push_permission_block(id.clone(), title, description, options);
            self.pending_permissions
                .lock()
                .expect("pending_permissions mutex poisoned")
                .insert(id, (session.clone(), responder));
        }
        (self.publish)(ConversationUpdated {
            session: session.clone(),
        });
    }

    /// PU-B.2: resolve a pending permission request by the agent's chosen
    /// `option_id`. Derives the inline block status from that option's `kind`,
    /// updates the block, and sends the `option_id` through the oneshot the
    /// supervisor is parked on (which answers ACP with `Selected(option_id)`),
    /// then publishes. No-op when `id` is unknown (already resolved, deferred
    /// and re-resolved, or never registered).
    pub fn resolve_permission(&self, id: &str, option_id: PermissionOptionId) {
        let session = {
            let mut conv = self.inner.lock().expect("conversation mutex poisoned");
            // Fail closed: an option whose kind we can't read (missing block, or
            // a future `#[non_exhaustive]` kind) marks the request Denied rather
            // than leaving it Pending or optimistically Allowed.
            let status = match conv.permission_option_kind(id, &option_id) {
                Some(PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways) => {
                    PermissionStatus::Allowed
                }
                _ => PermissionStatus::Denied,
            };
            conv.update_permission_status(id, status);
            let mut pending = self
                .pending_permissions
                .lock()
                .expect("pending_permissions mutex poisoned");
            let (session, sender) = match pending.remove(id) {
                Some(entry) => entry,
                None => return, // already resolved — no-op
            };
            let _ = sender.send(option_id);
            session
        };
        (self.publish)(ConversationUpdated { session });
    }

    /// AUX‑3: set the global processing status and publish a
    /// `ConversationUpdated` so the headerline re-renders.
    pub fn set_status(&self, session: &SessionKey, status: SessionStatus) {
        {
            let mut conv = self.inner.lock().expect("conversation mutex poisoned");
            conv.status = status;
        }
        (self.publish)(ConversationUpdated {
            session: session.clone(),
        });
    }

    /// Cheap-ish clone of the current conversation for projection.
    pub fn snapshot(&self) -> Conversation {
        self.inner
            .lock()
            .expect("conversation mutex poisoned")
            .clone()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use agent_client_protocol::schema::v1::{
        ContentChunk, PermissionOptionKind, TextContent, ToolCall as AcpToolCall, ToolCallUpdate,
        ToolCallUpdateFields,
    };

    fn text_chunk(text: &str) -> ContentChunk {
        ContentChunk::new(ContentBlock::Text(TextContent::new(text)))
    }

    #[test]
    fn agent_text_chunks_extend_one_block() {
        let mut c = Conversation::default();
        c.apply(&SessionUpdate::AgentMessageChunk(text_chunk("hi")));
        c.apply(&SessionUpdate::AgentMessageChunk(text_chunk(" there")));
        assert_eq!(c.turns.len(), 1);
        assert_eq!(c.turns[0].role, Role::Assistant);
        assert_eq!(c.turns[0].blocks, vec![Block::Text("hi there".to_string())]);
    }

    /// AU‑3: a user prompt lands as its own `User` turn (the REPL model:
    /// each Enter is distinct, never merged into agent output).
    #[test]
    fn push_user_text_appends_a_user_turn() {
        let mut c = Conversation::default();
        c.apply(&SessionUpdate::AgentMessageChunk(text_chunk("hello")));
        c.push_user_text("refactor parse_args");
        assert_eq!(c.turns.len(), 2);
        assert_eq!(c.turns[1].role, Role::User);
        assert_eq!(
            c.turns[1].blocks,
            vec![Block::Text("refactor parse_args".to_string())]
        );
        // Two consecutive prompts are two distinct turns, not merged.
        c.push_user_text("again");
        assert_eq!(c.turns.len(), 3);
        assert_eq!(c.turns[2].role, Role::User);
    }

    /// AU‑3: `ConversationStore::push_user_text` mutates the shared store and
    /// publishes a `ConversationUpdated` so the mode's drain re-projects.
    #[test]
    fn store_push_user_text_mutates_and_publishes() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let published = Arc::new(AtomicUsize::new(0));
        let p = published.clone();
        let store = ConversationStore::new(Arc::new(move |_ev| {
            p.fetch_add(1, Ordering::SeqCst);
        }));
        store.push_user_text(&SessionKey::new("opencode", 1), "hi");
        assert_eq!(published.load(Ordering::SeqCst), 1, "one publish");
        let snap = store.snapshot();
        assert_eq!(snap.turns.len(), 1);
        assert_eq!(snap.turns[0].role, Role::User);
    }

    #[test]
    fn thought_chunks_extend_a_reasoning_block() {
        let mut c = Conversation::default();
        c.apply(&SessionUpdate::AgentThoughtChunk(text_chunk("think")));
        c.apply(&SessionUpdate::AgentThoughtChunk(text_chunk("ing")));
        assert_eq!(
            c.turns[0].blocks,
            vec![Block::Reasoning("thinking".to_string())]
        );
    }

    #[test]
    fn tool_call_then_update_mutates_in_place_by_id() {
        let mut c = Conversation::default();
        let mut tc = AcpToolCall::new("tc-1", "edit parse.rs");
        tc.status = ToolCallStatus::InProgress;
        c.apply(&SessionUpdate::ToolCall(tc));
        assert_eq!(c.turns.len(), 1);
        match &c.turns[0].blocks[0] {
            Block::ToolCall { id, status, .. } => {
                assert_eq!(id, "tc-1");
                assert_eq!(*status, ToolStatus::Running);
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }

        let update = ToolCallUpdate::new(
            "tc-1",
            ToolCallUpdateFields::new().status(ToolCallStatus::Completed),
        );
        c.apply(&SessionUpdate::ToolCallUpdate(update));
        // Same block, updated status -- not a second block.
        assert_eq!(c.turns[0].blocks.len(), 1);
        match &c.turns[0].blocks[0] {
            Block::ToolCall { status, .. } => assert_eq!(*status, ToolStatus::Ok),
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    /// TCF: the tool-call detail (args on the initial call, output on a later
    /// update) is captured, pretty-printed, and merged in place — not
    /// discarded. This is what the expanded view will show.
    #[test]
    fn tool_call_captures_and_merges_input_then_output() {
        let mut c = Conversation::default();
        let mut tc = AcpToolCall::new("tc-1", "bash");
        tc.raw_input = Some(serde_json::json!({ "cmd": "echo hello" }));
        c.apply(&SessionUpdate::ToolCall(tc));
        match &c.turns[0].blocks[0] {
            Block::ToolCall { input, output, .. } => {
                let input = input.as_ref().expect("input captured on the initial call");
                assert!(input.contains("\"cmd\""), "pretty JSON input: {input}");
                assert!(input.contains("echo hello"), "input value: {input}");
                assert!(output.is_none(), "no output yet");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }

        // Output arrives on a later update — must merge, not clobber the input.
        let update = ToolCallUpdate::new(
            "tc-1",
            ToolCallUpdateFields::new().raw_output(serde_json::json!("hello\n")),
        );
        c.apply(&SessionUpdate::ToolCallUpdate(update));
        match &c.turns[0].blocks[0] {
            Block::ToolCall { input, output, .. } => {
                assert!(input.is_some(), "input survives the update merge");
                let output = output.as_ref().expect("output captured on the update");
                assert!(output.contains("hello"), "output value: {output}");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn text_after_tool_call_opens_a_new_text_block_same_turn() {
        let mut c = Conversation::default();
        c.apply(&SessionUpdate::AgentMessageChunk(text_chunk("before")));
        c.apply(&SessionUpdate::ToolCall(AcpToolCall::new("t", "run")));
        c.apply(&SessionUpdate::AgentMessageChunk(text_chunk("after")));
        assert_eq!(c.turns.len(), 1, "all assistant activity is one turn");
        assert_eq!(c.turns[0].blocks.len(), 3);
        assert_eq!(c.turns[0].blocks[2], Block::Text("after".to_string()));
    }

    // ── AUX‑1: permission block tests ──

    fn test_permission_option(
        id: &'static str,
        name: &'static str,
        kind: PermissionOptionKind,
    ) -> PermissionOption {
        PermissionOption::new(id, name, kind)
    }

    #[test]
    fn permission_block_created_in_assistant_turn() {
        let mut c = Conversation::default();
        c.apply(&SessionUpdate::AgentMessageChunk(text_chunk("working")));
        c.push_permission_block(
            "perm-1".to_string(),
            "Allow agent to run cargo test?".to_string(),
            None,
            vec![test_permission_option(
                "a1",
                "Allow once",
                PermissionOptionKind::AllowOnce,
            )],
        );
        assert_eq!(c.turns.len(), 1);
        assert_eq!(c.turns[0].role, Role::Assistant);
        assert_eq!(c.turns[0].blocks.len(), 2);
        match &c.turns[0].blocks[1] {
            Block::Permission {
                id,
                title,
                status,
                options,
                ..
            } => {
                assert_eq!(id, "perm-1");
                assert_eq!(title, "Allow agent to run cargo test?");
                assert_eq!(*status, PermissionStatus::Pending);
                assert_eq!(options.len(), 1);
            }
            other => panic!("expected Permission block, got {other:?}"),
        }
    }

    #[test]
    fn permission_block_opens_fresh_assistant_turn_when_no_previous() {
        let mut c = Conversation::default();
        c.push_permission_block("perm-1".to_string(), "Allow?".to_string(), None, vec![]);
        assert_eq!(c.turns.len(), 1);
        assert_eq!(c.turns[0].role, Role::Assistant);
    }

    #[test]
    fn permission_block_updated_by_id() {
        let mut c = Conversation::default();
        c.push_permission_block("perm-1".to_string(), "Allow?".to_string(), None, vec![]);
        c.update_permission_status("perm-1", PermissionStatus::Allowed);
        match &c.turns[0].blocks[0] {
            Block::Permission { status, .. } => assert_eq!(*status, PermissionStatus::Allowed),
            other => panic!("expected Permission, got {other:?}"),
        }
    }

    #[test]
    fn store_push_permission_request_creates_block_and_registers_responder() {
        let published = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let p = published.clone();
        let store = ConversationStore::new(Arc::new(move |_ev| {
            p.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let session = SessionKey::new("opencode", 1);
        let (tx, rx) = tokio::sync::oneshot::channel();

        store.push_permission_request(
            &session,
            "perm-1".to_string(),
            "Allow?".to_string(),
            None,
            vec![],
            tx,
        );

        // One publish on push
        assert_eq!(published.load(std::sync::atomic::Ordering::SeqCst), 1);
        // Block exists in snapshot
        let snap = store.snapshot();
        assert_eq!(snap.turns.len(), 1);
        assert!(matches!(
            &snap.turns[0].blocks[0],
            Block::Permission { id, status: PermissionStatus::Pending, .. } if id == "perm-1"
        ));
        // Responder is registered — dropping rx won't hang the test
        drop(rx);
    }

    #[test]
    fn store_resolve_permission_sends_outcome_and_publishes() {
        let published = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let p = published.clone();
        let store = ConversationStore::new(Arc::new(move |_ev| {
            p.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let session = SessionKey::new("opencode", 1);
        let (tx, rx) = tokio::sync::oneshot::channel();

        // PU-B.2: resolve by the agent's actual option; the block status derives
        // from that option's kind (AllowOnce → Allowed).
        let opt = test_permission_option("a1", "Allow once", PermissionOptionKind::AllowOnce);
        let chosen = opt.option_id.clone();
        store.push_permission_request(
            &session,
            "perm-1".to_string(),
            "Allow?".to_string(),
            None,
            vec![opt],
            tx,
        );

        // Reset publish count — the push already fired once
        published.store(0, std::sync::atomic::Ordering::SeqCst);

        store.resolve_permission("perm-1", chosen.clone());

        // Block updated
        let snap = store.snapshot();
        match &snap.turns[0].blocks[0] {
            Block::Permission { status, .. } => assert_eq!(*status, PermissionStatus::Allowed),
            other => panic!("expected Permission, got {other:?}"),
        }
        // Oneshot delivered the chosen option id
        assert_eq!(
            rx.blocking_recv(),
            Ok(chosen),
            "responder must receive the chosen option id",
        );
        // Publish fired
        assert_eq!(
            published.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "resolve must publish",
        );
    }

    #[test]
    fn store_resolve_permission_noop_for_unknown_id() {
        let published = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let p = published.clone();
        let store = ConversationStore::new(Arc::new(move |_ev| {
            p.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));

        // No pending permission with this id → no-op, no publish
        let opt = test_permission_option("a1", "Allow once", PermissionOptionKind::AllowOnce);
        store.resolve_permission("nonexistent", opt.option_id);
        assert_eq!(published.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    // ── AUX‑2: usage update tests ──

    fn update(used: u64, size: u64, cost: Option<(f64, &str)>) -> SessionUpdate {
        let mut u = agent_client_protocol::schema::v1::UsageUpdate::new(used, size);
        if let Some((amt, cur)) = cost {
            u = u.cost(agent_client_protocol::schema::v1::Cost::new(amt, cur));
        }
        SessionUpdate::UsageUpdate(u)
    }

    #[test]
    fn usage_update_stored() {
        let mut c = Conversation::default();
        c.apply(&update(53000, 200000, Some((0.045, "USD"))));
        assert_eq!(
            c.usage,
            Some(UsageSnapshot {
                used: 53000,
                size: 200000,
                cost: Some(Cost {
                    amount: 0.045,
                    currency: "USD".to_string(),
                }),
            }),
        );
    }

    #[test]
    fn usage_update_overwrites() {
        let mut c = Conversation::default();
        c.apply(&update(1000, 200000, None));
        assert!(c.usage.as_ref().unwrap().cost.is_none());
        // Second update overwrites
        c.apply(&update(53000, 200000, Some((0.045, "USD"))));
        assert_eq!(c.usage.unwrap().used, 53000);
    }

    /// The wire payload opencode 1.17.18 actually emits. The other usage tests
    /// build `SessionUpdate` in Rust, so they never exercise deserialization —
    /// the seam where a schema mismatch would silently drop the update.
    #[test]
    fn usage_update_from_opencode_wire_json() {
        let raw = serde_json::json!({
            "sessionUpdate": "usage_update",
            "used": 27744,
            "size": 200000,
            "cost": { "amount": 0, "currency": "USD" }
        });
        let update: SessionUpdate =
            serde_json::from_value(raw).expect("opencode usage_update should deserialize");
        let mut c = Conversation::default();
        c.apply(&update);
        let usage = c
            .usage
            .expect("usage should be populated from the wire payload");
        assert_eq!(usage.used, 27744);
        assert_eq!(usage.size, 200000);
        assert_eq!(usage.cost.map(|c| c.currency), Some("USD".to_string()));
    }

    #[test]
    fn usage_update_no_cost() {
        let mut c = Conversation::default();
        c.apply(&update(53000, 200000, None));
        assert_eq!(c.usage.as_ref().unwrap().used, 53000);
        assert_eq!(c.usage.as_ref().unwrap().size, 200000);
        assert!(c.usage.as_ref().unwrap().cost.is_none());
    }

    // ── AUX‑3: status tests ──

    #[test]
    fn status_idle_default() {
        let c = Conversation::default();
        assert_eq!(c.status, SessionStatus::Idle);
    }

    #[test]
    fn set_status_updates_and_publishes() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let published = Arc::new(AtomicUsize::new(0));
        let p = published.clone();
        let store = ConversationStore::new(Arc::new(move |_ev| {
            p.fetch_add(1, Ordering::SeqCst);
        }));
        let session = SessionKey::new("opencode", 1);

        store.set_status(&session, SessionStatus::Thinking);
        assert_eq!(published.load(Ordering::SeqCst), 1);
        assert_eq!(store.snapshot().status, SessionStatus::Thinking);

        store.set_status(&session, SessionStatus::Idle);
        assert_eq!(store.snapshot().status, SessionStatus::Idle);
    }

    #[test]
    fn status_display_formats() {
        assert_eq!(SessionStatus::Idle.to_string(), "Ready");
        assert!(SessionStatus::Thinking.to_string().contains("Thinking"));
        let exec = SessionStatus::Executing {
            tool: "edit".into(),
        };
        assert_eq!(exec.to_string(), "Working: edit");
        assert!(
            SessionStatus::AwaitingPermission
                .to_string()
                .contains("Awaiting")
        );
    }
}
