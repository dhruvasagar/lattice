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

use std::sync::{Arc, Mutex};

use agent_client_protocol::schema::v1::{ContentBlock, SessionUpdate, ToolCallStatus};

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

/// Shared, mutable conversation state plus a bus publisher. The supervisor holds
/// one clone and calls [`ConversationStore::apply`]; the `ai-conversation` mode
/// reads [`ConversationStore::snapshot`] on each `ConversationUpdated`.
#[derive(Clone)]
pub struct ConversationStore {
    inner: Arc<Mutex<Conversation>>,
    publish: Arc<dyn Fn(ConversationUpdated) + Send + Sync>,
}

impl ConversationStore {
    /// Build a store whose mutations publish `ConversationUpdated` via `publish`.
    pub fn new(publish: Arc<dyn Fn(ConversationUpdated) + Send + Sync>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Conversation::default())),
            publish,
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
        ContentChunk, TextContent, ToolCall as AcpToolCall, ToolCallUpdate, ToolCallUpdateFields,
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
}
