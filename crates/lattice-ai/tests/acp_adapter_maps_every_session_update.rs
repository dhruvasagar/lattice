//! LL.1: the ACP adapter folds every `SessionUpdate` the conversation model
//! cares about into the protocol-neutral `Conversation`, and ignores the rest.
#![cfg(feature = "acp")]
#![allow(clippy::unwrap_used, clippy::panic)]

use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, Cost as AcpCost, Plan, SessionUpdate, TextContent,
    ToolCall as AcpToolCall, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields,
    ToolKind as AcpToolKind, UsageUpdate,
};
use lattice_ai::acp::conversation::ApplySessionUpdate;
use lattice_ai::conversation::{Block, Conversation, Role, ToolKind, ToolStatus};

fn chunk(text: &str) -> ContentChunk {
    ContentChunk::new(ContentBlock::Text(TextContent::new(text)))
}

#[test]
fn every_conversation_update_lands_and_the_rest_are_ignored() {
    let mut c = Conversation::default();

    c.apply_update(&SessionUpdate::UserMessageChunk(chunk("do it")));
    c.apply_update(&SessionUpdate::AgentThoughtChunk(chunk("hmm")));
    c.apply_update(&SessionUpdate::AgentMessageChunk(chunk("ok")));

    let mut tc = AcpToolCall::new("t1", "edit a.rs");
    tc.kind = AcpToolKind::Delete;
    tc.status = ToolCallStatus::InProgress;
    c.apply_update(&SessionUpdate::ToolCall(tc));
    c.apply_update(&SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
        "t1",
        ToolCallUpdateFields::new()
            .status(ToolCallStatus::Failed)
            .kind(AcpToolKind::Fetch),
    )));
    c.apply_update(&SessionUpdate::UsageUpdate(
        UsageUpdate::new(10, 100).cost(AcpCost::new(0.5, "USD")),
    ));

    let before = c.clone();
    c.apply_update(&SessionUpdate::Plan(Plan::new(vec![])));
    assert_eq!(c, before, "a plan update is not conversation content");

    // User turn, then one assistant turn holding reasoning, text, tool call.
    assert_eq!(c.turns.len(), 2);
    assert_eq!(c.turns[0].role, Role::User);
    assert_eq!(c.turns[0].blocks, vec![Block::Text("do it".into())]);
    assert_eq!(c.turns[1].role, Role::Assistant);
    assert_eq!(c.turns[1].blocks[0], Block::Reasoning("hmm".into()));
    assert_eq!(c.turns[1].blocks[1], Block::Text("ok".into()));
    match &c.turns[1].blocks[2] {
        Block::ToolCall {
            id,
            origin,
            status,
            kind,
            ..
        } => {
            assert_eq!(id, "t1");
            assert!(origin.is_none(), "ACP never sets origin");
            assert_eq!(*status, ToolStatus::Err);
            assert_eq!(*kind, ToolKind::Search, "update's kind overwrites");
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }
    let usage = c.usage.expect("usage recorded");
    assert_eq!((usage.used, usage.size), (10, 100));
    assert_eq!(usage.cost.map(|c| c.currency), Some("USD".to_string()));
}
