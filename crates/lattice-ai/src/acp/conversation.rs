//! ACP adapter for the neutral conversation model (AU‑1, LL.1).
//!
//! The ACP supervisor receives structured `SessionUpdate`s (message chunks,
//! thought chunks, tool calls, tool-call updates). This module folds the
//! *conversation* ones into the protocol-neutral
//! [`Conversation`](crate::conversation::Conversation) and maps ACP's tool and
//! permission types onto the neutral ones; nothing under `crate::conversation`
//! names the protocol. Trace sources (`Client` / `Lifecycle`) still flow to
//! `AiLogger`.
//!
//! [`ApplySessionUpdate::apply_update`] on a `Conversation` is pure and unit-testable
//! (no I/O, no locks); on a [`ConversationStore`] it also publishes.

use agent_client_protocol::schema::v1 as acp;
use agent_client_protocol::schema::v1::{ContentBlock, SessionUpdate};

use lattice_agent::SessionKey;

use crate::conversation::{
    Conversation, ConversationStore, Cost, PermissionKind, PermissionOption, Role, ToolKind,
    ToolStatus, UsageSnapshot,
};

/// Map ACP's tool-call status onto the neutral one.
pub fn tool_status_from_acp(status: acp::ToolCallStatus) -> ToolStatus {
    match status {
        acp::ToolCallStatus::Pending => ToolStatus::Pending,
        acp::ToolCallStatus::InProgress => ToolStatus::Running,
        acp::ToolCallStatus::Completed => ToolStatus::Ok,
        acp::ToolCallStatus::Failed => ToolStatus::Err,
        // `ToolCallStatus` is `#[non_exhaustive]`.
        _ => ToolStatus::Pending,
    }
}

/// Map ACP's tool kind onto the neutral one. ACP distinguishes more kinds than
/// a projection needs: deleting and moving are edits, fetching is a search.
pub fn tool_kind_from_acp(kind: acp::ToolKind) -> ToolKind {
    match kind {
        acp::ToolKind::Read => ToolKind::Read,
        acp::ToolKind::Search | acp::ToolKind::Fetch => ToolKind::Search,
        acp::ToolKind::Edit | acp::ToolKind::Delete | acp::ToolKind::Move => ToolKind::Edit,
        acp::ToolKind::Execute => ToolKind::Execute,
        // Think, SwitchMode, Other, and any future `#[non_exhaustive]` kind.
        _ => ToolKind::Other,
    }
}

/// Map an ACP permission option onto the neutral one.
pub fn permission_option_from_acp(option: &acp::PermissionOption) -> PermissionOption {
    let kind = match option.kind {
        acp::PermissionOptionKind::AllowOnce => PermissionKind::AllowOnce,
        acp::PermissionOptionKind::AllowAlways => PermissionKind::AllowAlways,
        acp::PermissionOptionKind::RejectOnce => PermissionKind::RejectOnce,
        acp::PermissionOptionKind::RejectAlways => PermissionKind::RejectAlways,
        // `PermissionOptionKind` is `#[non_exhaustive]`; fail closed.
        _ => PermissionKind::Other,
    };
    PermissionOption::new(option.option_id.0.to_string(), option.name.clone(), kind)
}

/// TCF: pretty-print a raw tool JSON payload for the expanded view. Falls back
/// to the compact `Display` form if pretty-printing somehow fails.
fn pretty_json(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// Fold ACP `SessionUpdate`s into the neutral model.
pub trait ApplySessionUpdate {
    /// Fold one ACP `SessionUpdate` into the model. Non-conversation updates
    /// (plans, mode changes, ...) are ignored; `SessionUpdate` and
    /// `ContentBlock` are `#[non_exhaustive]`, so a catch-all closes each match.
    fn apply_update(&mut self, update: &SessionUpdate);
}

impl ApplySessionUpdate for Conversation {
    fn apply_update(&mut self, update: &SessionUpdate) {
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
                    None,
                    tc.title.clone(),
                    tool_status_from_acp(tc.status),
                    tool_kind_from_acp(tc.kind),
                    tc.raw_input.as_ref().map(pretty_json),
                    tc.raw_output.as_ref().map(pretty_json),
                );
            }
            SessionUpdate::ToolCallUpdate(u) => {
                self.merge_tool_update(
                    u.tool_call_id.0.as_ref(),
                    u.fields.status.map(tool_status_from_acp),
                    u.fields.kind.map(tool_kind_from_acp),
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
}

/// Fold ACP `SessionUpdate`s into a shared [`ConversationStore`] and publish.
pub trait ApplySessionUpdateToStore {
    /// Fold `update` into the conversation for `session`, then publish.
    fn apply_update(&self, session: &SessionKey, update: &SessionUpdate);
}

impl ApplySessionUpdateToStore for ConversationStore {
    fn apply_update(&self, session: &SessionKey, update: &SessionUpdate) {
        self.mutate(session, |conv| conv.apply_update(update));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::conversation::{Block, SessionStatus};
    use agent_client_protocol::schema::v1::{
        ContentChunk, TextContent, ToolCall as AcpToolCall, ToolCallStatus, ToolCallUpdate,
        ToolCallUpdateFields,
    };

    fn text_chunk(text: &str) -> ContentChunk {
        ContentChunk::new(ContentBlock::Text(TextContent::new(text)))
    }

    fn update(used: u64, size: u64, cost: Option<(f64, &str)>) -> SessionUpdate {
        let mut u = agent_client_protocol::schema::v1::UsageUpdate::new(used, size);
        if let Some((amt, cur)) = cost {
            u = u.cost(agent_client_protocol::schema::v1::Cost::new(amt, cur));
        }
        SessionUpdate::UsageUpdate(u)
    }

    #[test]
    fn agent_text_chunks_extend_one_block() {
        let mut c = Conversation::default();
        c.apply_update(&SessionUpdate::AgentMessageChunk(text_chunk("hi")));
        c.apply_update(&SessionUpdate::AgentMessageChunk(text_chunk(" there")));
        assert_eq!(c.turns.len(), 1);
        assert_eq!(c.turns[0].role, Role::Assistant);
        assert_eq!(c.turns[0].blocks, vec![Block::Text("hi there".to_string())]);
    }

    #[test]
    fn thought_chunks_extend_a_reasoning_block() {
        let mut c = Conversation::default();
        c.apply_update(&SessionUpdate::AgentThoughtChunk(text_chunk("think")));
        c.apply_update(&SessionUpdate::AgentThoughtChunk(text_chunk("ing")));
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
        c.apply_update(&SessionUpdate::ToolCall(tc));
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
        c.apply_update(&SessionUpdate::ToolCallUpdate(update));
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
        c.apply_update(&SessionUpdate::ToolCall(tc));
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
        c.apply_update(&SessionUpdate::ToolCallUpdate(update));
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
        c.apply_update(&SessionUpdate::AgentMessageChunk(text_chunk("before")));
        c.apply_update(&SessionUpdate::ToolCall(AcpToolCall::new("t", "run")));
        c.apply_update(&SessionUpdate::AgentMessageChunk(text_chunk("after")));
        assert_eq!(c.turns.len(), 1, "all assistant activity is one turn");
        assert_eq!(c.turns[0].blocks.len(), 3);
        assert_eq!(c.turns[0].blocks[2], Block::Text("after".to_string()));
    }

    #[test]
    fn usage_update_stored() {
        let mut c = Conversation::default();
        c.apply_update(&update(53000, 200000, Some((0.045, "USD"))));
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
        c.apply_update(&update(1000, 200000, None));
        assert!(c.usage.as_ref().unwrap().cost.is_none());
        // Second update overwrites
        c.apply_update(&update(53000, 200000, Some((0.045, "USD"))));
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
        c.apply_update(&update);
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
        c.apply_update(&update(53000, 200000, None));
        assert_eq!(c.usage.as_ref().unwrap().used, 53000);
        assert_eq!(c.usage.as_ref().unwrap().size, 200000);
        assert!(c.usage.as_ref().unwrap().cost.is_none());
    }

    #[test]
    fn tool_kinds_map_onto_the_neutral_set() {
        use acp::ToolKind as K;
        assert_eq!(tool_kind_from_acp(K::Read), ToolKind::Read);
        assert_eq!(tool_kind_from_acp(K::Search), ToolKind::Search);
        assert_eq!(tool_kind_from_acp(K::Fetch), ToolKind::Search);
        assert_eq!(tool_kind_from_acp(K::Edit), ToolKind::Edit);
        assert_eq!(tool_kind_from_acp(K::Delete), ToolKind::Edit);
        assert_eq!(tool_kind_from_acp(K::Move), ToolKind::Edit);
        assert_eq!(tool_kind_from_acp(K::Execute), ToolKind::Execute);
        assert_eq!(tool_kind_from_acp(K::Think), ToolKind::Other);
        assert_eq!(tool_kind_from_acp(K::Other), ToolKind::Other);
    }

    #[test]
    fn permission_options_map_every_kind_and_keep_wire_ids() {
        use acp::PermissionOptionKind as K;
        for (acp_kind, want) in [
            (K::AllowOnce, PermissionKind::AllowOnce),
            (K::AllowAlways, PermissionKind::AllowAlways),
            (K::RejectOnce, PermissionKind::RejectOnce),
            (K::RejectAlways, PermissionKind::RejectAlways),
        ] {
            let got =
                permission_option_from_acp(&acp::PermissionOption::new("id-1", "Name", acp_kind));
            assert_eq!(got.option_id, "id-1", "wire id survives the mapping");
            assert_eq!(got.name, "Name");
            assert_eq!(got.kind, want);
        }
    }

    /// ACP never sets `origin`; the adapter must not invent one.
    #[test]
    fn acp_tool_calls_carry_no_origin() {
        let mut c = Conversation::default();
        c.apply_update(&SessionUpdate::ToolCall(AcpToolCall::new("t", "run")));
        match &c.turns[0].blocks[0] {
            Block::ToolCall { origin, .. } => assert!(origin.is_none()),
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    /// The store path publishes once per folded update.
    #[test]
    fn store_apply_update_folds_and_publishes() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let published = Arc::new(AtomicUsize::new(0));
        let p = published.clone();
        let store = ConversationStore::new(Arc::new(move |_ev| {
            p.fetch_add(1, Ordering::SeqCst);
        }));
        store.apply_update(
            &SessionKey::new("opencode", 1),
            &SessionUpdate::AgentMessageChunk(text_chunk("hi")),
        );
        assert_eq!(published.load(Ordering::SeqCst), 1);
        assert_eq!(store.snapshot().turns.len(), 1);
    }

    #[test]
    fn status_type_is_reachable_from_the_adapter_tests() {
        assert_eq!(Conversation::default().status, SessionStatus::Idle);
    }
}
