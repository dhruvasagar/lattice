//! LL.21: a Gemini chat end to end against a mock server: the request it
//! sends (URL, key header), and a tool loop whose second round carries the
//! thought signature back on the call it came with. The decoding itself is
//! in the LL.3 suite (`llm_provider_decoders.rs`, and every fixture split at
//! every byte in `a_character_split_across_chunks_decodes.rs`).

#![cfg(feature = "llm")]
#![allow(clippy::unwrap_used, clippy::panic)]

mod llm_support;

use std::sync::Arc;

use lattice_ai::llm::RequestRegistry;
use lattice_ai::llm::chat::{LlmChats, LlmChatsHandle, spawn_turn_listener};
use lattice_ai::llm::config::LlmConfig;
use llm_support::*;

fn gemini_reply(parts: serde_json::Value) -> Vec<u8> {
    let chunk = serde_json::json!({
        "candidates": [{"content": {"parts": parts, "role": "model"}, "finishReason": "STOP"}],
        "usageMetadata": {"promptTokenCount": 9, "candidatesTokenCount": 4},
    });
    format!("{SSE_HEAD}data: {chunk}\r\n\r\n").into_bytes()
}

fn gemini_chats(addr: std::net::SocketAddr, tools: Arc<TestTools>) -> LlmChatsHandle {
    let text = format!(
        r#"
        [llm]
        tools = "lattice.echo"
        [llm.providers.gemini]
        base_url = "http://{addr}/v1beta"
        model = "gemini-test"
        api_key_command = "echo AIza-test-key"
        "#
    );
    let config = LlmConfig::from_table(&text.parse::<toml::Table>().unwrap());
    assert!(config.warnings.is_empty(), "{:?}", config.warnings);
    let bus = Arc::new(lattice_runtime::EventBus::new());
    let handle = tokio::runtime::Handle::current();
    let requests = Arc::new(RequestRegistry::new(
        handle.clone(),
        Some(bus.clone()),
        None,
    ));
    let chats = Arc::new(LlmChats::new(handle, requests, bus.clone(), config));
    spawn_turn_listener(chats.clone(), &bus);
    chats
        .tools()
        .add_source(lattice_ai::llm::tools::broker::LATTICE, tools);
    chats
}

#[tokio::test]
async fn a_tool_loop_sends_the_signature_back_on_its_call() {
    let (addr, log) = scripted(vec![
        gemini_reply(serde_json::json!([
            {"functionCall": {"id": "fc_01", "name": "lattice__echo", "args": {"text": "hi"}},
             "thoughtSignature": "SIG-1"},
        ])),
        gemini_reply(serde_json::json!([{"text": "it said hi"}])),
    ])
    .await;
    let tools = Arc::new(TestTools::default());
    let chats = gemini_chats(addr, tools.clone());
    let session = chats.create();
    chats.submit(&session, "use echo".into());
    until("the loop ends", || !session.is_busy()).await;

    let sent = log.lock().unwrap().clone();
    assert_eq!(sent.len(), 2, "one call, then the answer");
    let head = sent[0].split("\r\n\r\n").next().unwrap().to_lowercase();
    assert!(
        head.starts_with("post /v1beta/models/gemini-test:streamgeneratecontent?alt=sse "),
        "{head}"
    );
    assert!(head.contains("x-goog-api-key: aiza-test-key"), "{head}");
    assert!(
        !head.lines().next().unwrap().contains("aiza"),
        "never in the URL"
    );
    assert_eq!(tools.ran.lock().unwrap().len(), 1);

    let second = body_json(&sent[1]);
    let contents = second["contents"].as_array().unwrap();
    assert_eq!(
        contents[1],
        serde_json::json!({"role": "model", "parts": [
            {"functionCall": {"id": "fc_01", "name": "lattice__echo", "args": {"text": "hi"}},
             "thoughtSignature": "SIG-1"},
        ]}),
        "the call goes back with the signature it came with"
    );
    assert_eq!(
        contents[2],
        serde_json::json!({"role": "user", "parts": [
            {"functionResponse": {"id": "fc_01", "name": "lattice__echo",
                                  "response": {"content": "echo: hi"}}},
        ]})
    );

    // The block that keeps the signature shows nothing.
    let snap = session.store.snapshot();
    let shown = lattice_ai::conversation::render_conversation(&snap);
    assert!(shown.contains("it said hi"), "{shown}");
    assert!(
        !shown.contains('\u{2502}'),
        "no empty reasoning line: {shown}"
    );
    assert_eq!(snap.reasoning_data.len(), 1);
    assert!(snap.error.is_none());
}
