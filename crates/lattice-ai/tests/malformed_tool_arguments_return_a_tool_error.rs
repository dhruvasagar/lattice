//! LL.16, review focus 5: arguments a tool cannot use come back to the
//! model as an error result it can correct from. Nothing runs on them, the
//! request goes on, and the call is sent back with arguments a strict
//! server accepts.

#![cfg(feature = "llm")]
#![allow(clippy::unwrap_used, clippy::panic)]

mod llm_support;

use std::sync::Arc;

use lattice_ai::conversation::ToolStatus;
use lattice_ai::llm::RequestState;
use llm_support::*;

#[tokio::test]
async fn arguments_that_are_not_json_are_an_error_result() {
    let (addr, log) = scripted(vec![
        openai_tool_call("call_1", "lattice__echo", r#"{"text": "unterminated"#),
        openai_text("let me retry"),
    ])
    .await;
    let tools = Arc::new(TestTools::default());
    let chats = tool_chats(addr, "lattice.echo", tools.clone());
    let session = chats.create();
    chats.submit(&session, "go".into());
    until("the loop ends", || !session.is_busy()).await;

    assert_eq!(
        chats.requests().snapshot()[0].state,
        RequestState::Done,
        "a bad call does not fail the request"
    );
    assert!(tools.ran.lock().unwrap().is_empty(), "nothing ran");
    let blocks = tool_blocks(&session);
    assert_eq!(blocks[0].1, ToolStatus::Err);
    let said = blocks[0].2.clone().unwrap();
    assert!(said.contains("not valid JSON"), "{said}");

    let sent = log.lock().unwrap().clone();
    strictly_valid(&sent[1]).unwrap();
    let body = body_json(&sent[1]);
    let msgs = body["messages"].as_array().unwrap();
    let call = msgs.iter().find(|m| m["tool_calls"].is_array()).unwrap();
    assert_eq!(call["tool_calls"][0]["function"]["arguments"], "{}");
    let result = msgs.iter().find(|m| m["role"] == "tool").unwrap();
    assert!(
        result["content"]
            .as_str()
            .unwrap()
            .contains("invalid arguments"),
        "{result}"
    );
}

#[tokio::test]
async fn arguments_missing_a_field_are_an_error_result() {
    let (addr, log) = scripted(vec![
        openai_tool_call("call_1", "lattice__echo", r#"{"txt":"typo"}"#),
        openai_text("fixed"),
    ])
    .await;
    let chats = tool_chats(addr, "lattice.echo", Arc::new(TestTools::default()));
    let session = chats.create();
    chats.submit(&session, "go".into());
    until("the loop ends", || !session.is_busy()).await;

    let blocks = tool_blocks(&session);
    assert_eq!(blocks[0].1, ToolStatus::Err);
    assert!(
        blocks[0]
            .2
            .as_deref()
            .unwrap()
            .contains("missing field `text`")
    );
    strictly_valid(&log.lock().unwrap()[1]).unwrap();
}
