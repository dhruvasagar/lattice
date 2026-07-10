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
    ContentBlock, PermissionOption, SessionUpdate, ToolCallStatus,
};
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

/// AUX‑1: the user's decision on a pending permission request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionOutcome {
    AllowOnce,
    AllowAlways,
    DenyOnce,
    DenyAlways,
}

/// One renderable unit within a turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// Streamed message text.
    Text(String),
    /// Streamed reasoning / thinking; folded by default in the projection.
    Reasoning(String),
    /// A tool invocation and its live status.
    ToolCall {
        id: String,
        title: String,
        status: ToolStatus,
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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Conversation {
    pub turns: Vec<Turn>,
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
                );
            }
            SessionUpdate::ToolCallUpdate(u) => {
                if let Some(status) = u.fields.status {
                    self.update_tool_status(
                        &u.tool_call_id.0.to_string(),
                        ToolStatus::from_acp(status),
                    );
                }
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
    fn push_tool_call(&mut self, id: String, title: String, status: ToolStatus) {
        let block = Block::ToolCall { id, title, status };
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

    /// Update the status of the tool-call block with `id` (searched newest-first).
    fn update_tool_status(&mut self, id: &str, new_status: ToolStatus) {
        for turn in self.turns.iter_mut().rev() {
            for block in turn.blocks.iter_mut().rev() {
                if let Block::ToolCall {
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
    pending_permissions:
        Arc<Mutex<HashMap<String, (SessionKey, oneshot::Sender<PermissionOutcome>)>>>,
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
        responder: oneshot::Sender<PermissionOutcome>,
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

    /// AUX‑1: resolve a pending permission request. Updates the block status and
    /// sends the outcome through the oneshot channel, then publishes. No-op when
    /// `id` is unknown (already resolved or never registered).
    pub fn resolve_permission(&self, id: &str, outcome: PermissionOutcome) {
        let status = match outcome {
            PermissionOutcome::AllowOnce | PermissionOutcome::AllowAlways => {
                PermissionStatus::Allowed
            }
            PermissionOutcome::DenyOnce | PermissionOutcome::DenyAlways => {
                PermissionStatus::Denied
            }
        };
        let session = {
            let mut conv = self.inner.lock().expect("conversation mutex poisoned");
            conv.update_permission_status(id, status);
            let mut pending = self
                .pending_permissions
                .lock()
                .expect("pending_permissions mutex poisoned");
            let (session, sender) = match pending.remove(id) {
                Some(entry) => entry,
                None => return, // already resolved — no-op
            };
            let _ = sender.send(outcome);
            session
        };
        (self.publish)(ConversationUpdated { session });
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

    fn test_permission_option(id: &'static str, name: &'static str, kind: PermissionOptionKind) -> PermissionOption {
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
            vec![test_permission_option("a1", "Allow once", PermissionOptionKind::AllowOnce)],
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
        c.push_permission_block(
            "perm-1".to_string(),
            "Allow?".to_string(),
            None,
            vec![],
        );
        assert_eq!(c.turns.len(), 1);
        assert_eq!(c.turns[0].role, Role::Assistant);
    }

    #[test]
    fn permission_block_updated_by_id() {
        let mut c = Conversation::default();
        c.push_permission_block(
            "perm-1".to_string(),
            "Allow?".to_string(),
            None,
            vec![],
        );
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

        store.push_permission_request(
            &session,
            "perm-1".to_string(),
            "Allow?".to_string(),
            None,
            vec![],
            tx,
        );

        // Reset publish count — the push already fired once
        published.store(0, std::sync::atomic::Ordering::SeqCst);

        store.resolve_permission("perm-1", PermissionOutcome::AllowOnce);

        // Block updated
        let snap = store.snapshot();
        match &snap.turns[0].blocks[0] {
            Block::Permission { status, .. } => assert_eq!(*status, PermissionStatus::Allowed),
            other => panic!("expected Permission, got {other:?}"),
        }
        // Oneshot delivered
        assert_eq!(
            rx.blocking_recv(),
            Ok(PermissionOutcome::AllowOnce),
            "responder must receive the outcome",
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
        store.resolve_permission("nonexistent", PermissionOutcome::AllowOnce);
        assert_eq!(published.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}
