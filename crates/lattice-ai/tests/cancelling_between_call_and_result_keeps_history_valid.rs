//! LL.16, review focus 3: stopping a request while a tool runs leaves the
//! chat's history valid. The call is answered with an error result, so the
//! next prompt's request is one a strict server accepts.

#![allow(clippy::unwrap_used, clippy::panic)]

mod llm_support;

use std::sync::Arc;

use lattice_ai::conversation::ToolStatus;
use lattice_ai::llm::RequestState;
use llm_support::*;

#[tokio::test]
async fn cancelling_between_call_and_result_keeps_history_valid() {
    let (addr, log) = scripted(vec![
        openai_tool_call("call_w", "lattice__wait", "{}"),
        openai_text("fine"),
    ])
    .await;
    let tools = Arc::new(TestTools::default());
    let chats = tool_chats(addr, "lattice.*", tools.clone());
    let session = chats.create();
    chats.submit(&session, "wait for it".into());

    // The call is running: the request waits on the tool.
    until("the tool is running", || {
        tool_blocks(&session)
            .first()
            .is_some_and(|b| b.1 == ToolStatus::Running)
    })
    .await;
    let state = chats.requests().snapshot()[0].state.clone();
    assert_eq!(state, RequestState::AwaitingTool);

    chats.interrupt(&session);
    until("the turn ends", || !session.is_busy()).await;
    assert_eq!(
        chats.requests().snapshot()[0].state,
        RequestState::Cancelled
    );
    let blocks = tool_blocks(&session);
    assert_eq!(
        blocks[0].1,
        ToolStatus::Err,
        "nothing left showing as running"
    );
    assert_eq!(
        blocks[0].2.as_deref(),
        Some(lattice_ai::llm::tool_loop::STOPPED)
    );

    chats.submit(&session, "never mind, say fine".into());
    until("the second request", || log.lock().unwrap().len() == 2).await;
    until("the reply", || !session.is_busy()).await;

    let sent = log.lock().unwrap().clone();
    strictly_valid(&sent[1]).unwrap();
    let body = body_json(&sent[1]);
    let msgs = body["messages"].as_array().unwrap();
    let result = msgs.iter().find(|m| m["role"] == "tool").unwrap();
    assert_eq!(result["tool_call_id"], "call_w");
    assert_eq!(result["content"], lattice_ai::llm::tool_loop::STOPPED);
    assert_eq!(msgs.last().unwrap()["content"], "never mind, say fine");
}
