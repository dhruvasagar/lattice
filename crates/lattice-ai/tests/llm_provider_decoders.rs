//! Recorded-shape fixtures per provider: text, reasoning, tool calls, usage,
//! plus the failure modes (truncation, unknown events, garbage input).

#![cfg(feature = "llm")]
use lattice_ai::llm::*;

fn run(p: &dyn Provider, text: &str) -> Vec<StreamEvent> {
    let mut d = p.decoder();
    let mut ev = d.push(text.as_bytes());
    ev.extend(d.finish());
    ev
}

fn anthropic() -> AnthropicProvider {
    AnthropicProvider::default()
}

fn openai() -> OpenAiProvider {
    OpenAiProvider::new("http://localhost:11434/v1")
}

fn gemini() -> GeminiProvider {
    GeminiProvider::default()
}

fn usage(i: u64, o: u64, cr: u64, cw: u64) -> StreamEvent {
    StreamEvent::Usage(Usage {
        input_tokens: i,
        output_tokens: o,
        cache_read_tokens: cr,
        cache_write_tokens: cw,
    })
}

#[test]
fn anthropic_text_reasoning_and_usage() {
    let ev = run(
        &anthropic(),
        include_str!("fixtures/llm/anthropic_text_thinking.sse"),
    );
    assert_eq!(
        ev,
        vec![
            usage(25, 1, 7, 3),
            StreamEvent::Reasoning("Say hello in Japanese: 日本語".into()),
            StreamEvent::ReasoningData(ReasoningData {
                provider: "anthropic".into(),
                data: "EqQBCgIYAhIM".into()
            }),
            StreamEvent::Text("Héllo → ".into()),
            StreamEvent::Text("こんにちは ✓".into()),
            usage(25, 42, 7, 3),
            StreamEvent::Done(StopReason::EndTurn),
        ]
    );
}

#[test]
fn anthropic_tool_use_streams_input_fragments() {
    let ev = run(
        &anthropic(),
        include_str!("fixtures/llm/anthropic_tool_use.sse"),
    );
    let id = || "toolu_01".to_string();
    assert_eq!(
        ev,
        vec![
            usage(100, 1, 0, 0),
            StreamEvent::Text("Reading «file».".into()),
            StreamEvent::ToolCallStart {
                id: id(),
                name: "read_file".into()
            },
            StreamEvent::ToolCallInput {
                id: id(),
                json_fragment: "{\"path\": \"src/".into()
            },
            StreamEvent::ToolCallInput {
                id: id(),
                json_fragment: "ünï.rs\"}".into()
            },
            StreamEvent::ToolCallEnd { id: id() },
            usage(100, 61, 0, 0),
            StreamEvent::Done(StopReason::ToolUse),
        ]
    );
}

#[test]
fn openai_text_reasoning_and_usage() {
    let ev = run(
        &openai(),
        include_str!("fixtures/llm/openai_text_reasoning.sse"),
    );
    assert_eq!(
        ev,
        vec![
            StreamEvent::Reasoning("Think: 日本".into()),
            StreamEvent::Text("Héllo ".into()),
            StreamEvent::Text("→ 世界 ✓".into()),
            usage(12, 9, 4, 0),
            StreamEvent::Done(StopReason::EndTurn),
        ]
    );
}

#[test]
fn openai_tool_calls_assemble_by_index() {
    let ev = run(
        &openai(),
        include_str!("fixtures/llm/openai_tool_calls.sse"),
    );
    let call = |id: &str, frag: &str| StreamEvent::ToolCallInput {
        id: id.into(),
        json_fragment: frag.into(),
    };
    assert_eq!(
        ev,
        vec![
            StreamEvent::ToolCallStart {
                id: "call_a".into(),
                name: "read_file".into()
            },
            call("call_a", "{\"path\":"),
            StreamEvent::ToolCallStart {
                id: "call_b".into(),
                name: "grep".into()
            },
            call("call_b", "{\"q\":\"é\"}"),
            call("call_a", " \"a.rs\"}"),
            StreamEvent::ToolCallEnd {
                id: "call_a".into()
            },
            StreamEvent::ToolCallEnd {
                id: "call_b".into()
            },
            usage(50, 20, 0, 0),
            StreamEvent::Done(StopReason::ToolUse),
        ]
    );
}

#[test]
fn ollama_style_stream_without_usage_still_completes() {
    let ev = run(
        &openai(),
        include_str!("fixtures/llm/openai_ollama_no_usage.sse"),
    );
    assert_eq!(
        ev,
        vec![
            StreamEvent::Text("Ça ".into()),
            StreamEvent::Text("marche".into()),
            StreamEvent::Done(StopReason::EndTurn),
        ]
    );
}

#[test]
fn truncated_streams_yield_error_not_a_panic() {
    for (p, text) in [
        (
            &anthropic() as &dyn Provider,
            include_str!("fixtures/llm/anthropic_tool_use.sse"),
        ),
        (
            &openai(),
            include_str!("fixtures/llm/openai_text_reasoning.sse"),
        ),
        (
            &gemini(),
            include_str!("fixtures/llm/gemini_text_thinking.sse"),
        ),
    ] {
        // Cut mid-event, mid-stream.
        let cut = &text[..text.len() / 2];
        let ev = run(p, cut);
        assert_eq!(
            ev.last(),
            Some(&StreamEvent::Error(ProviderError::Truncated))
        );
        assert!(!ev.iter().any(|e| matches!(e, StreamEvent::Done(_))));
    }
}

#[test]
fn unknown_event_types_are_skipped() {
    let a = run(
        &anthropic(),
        "event: brand_new\ndata: {\"type\":\"brand_new\"}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    assert_eq!(a, vec![StreamEvent::Done(StopReason::EndTurn)]);
    let o = run(
        &openai(),
        "data: {\"something\":\"else\"}\n\ndata: [DONE]\n\n",
    );
    assert_eq!(o, vec![StreamEvent::Done(StopReason::EndTurn)]);
}

#[test]
fn in_band_errors_surface_once() {
    let a = run(
        &anthropic(),
        "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n",
    );
    assert_eq!(
        a,
        vec![StreamEvent::Error(ProviderError::Api {
            kind: "overloaded_error".into(),
            message: "Overloaded".into()
        })]
    );
}

#[test]
fn arbitrary_garbage_never_panics() {
    let inputs: [&[u8]; 6] = [
        b"",
        b"\xff\xfe\x00data: \xc3",
        b"data: {not json}\n\ndata: [DONE",
        b"data:\n\ndata:\n\n\r\n\r\n",
        b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":7}\n\n",
        b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":\"x\",\"function\":3}]}}]}\n\n",
    ];
    for p in [&anthropic() as &dyn Provider, &openai(), &gemini()] {
        for input in inputs {
            let mut d = p.decoder();
            let _ = d.push(input);
            let _ = d.finish();
        }
    }
}

fn calls(ev: &[StreamEvent]) -> Vec<(String, String)> {
    ev.iter()
        .filter_map(|e| match e {
            StreamEvent::ToolCallStart { id, name } => Some((id.clone(), name.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn openai_calls_without_index_open_a_slot_per_new_id() {
    let ev = run(
        &openai(),
        concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"a\",\"function\":{\"name\":\"f\",\"arguments\":\"{\\\"x\\\":\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"arguments\":\"1}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"b\",\"function\":{\"name\":\"g\",\"arguments\":\"{}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        ),
    );
    assert_eq!(
        calls(&ev),
        vec![("a".into(), "f".into()), ("b".into(), "g".into())]
    );
    let frag = |id: &str| -> String {
        ev.iter()
            .filter_map(|e| match e {
                StreamEvent::ToolCallInput {
                    id: i,
                    json_fragment,
                } if i == id => Some(json_fragment.as_str()),
                _ => None,
            })
            .collect()
    };
    assert_eq!(frag("a"), "{\"x\":1}");
    assert_eq!(frag("b"), "{}");
}

#[test]
fn openai_eof_after_finish_reason_without_done_completes() {
    let ev = run(
        &openai(),
        "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"length\"}]}\n\n",
    );
    assert_eq!(
        ev,
        vec![
            StreamEvent::Text("hi".into()),
            StreamEvent::Done(StopReason::MaxTokens)
        ]
    );
}

#[test]
fn openai_tool_calls_finished_with_stop_still_run() {
    let ev = run(
        &openai(),
        concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"a\",\"function\":{\"name\":\"f\",\"arguments\":\"{}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        ),
    );
    assert_eq!(ev.last(), Some(&StreamEvent::Done(StopReason::ToolUse)));
}

#[test]
fn anthropic_tool_call_with_no_arguments() {
    let ev = run(
        &anthropic(),
        concat!(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t\",\"name\":\"ping\",\"input\":{}}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        ),
    );
    assert_eq!(
        ev,
        vec![
            StreamEvent::ToolCallStart {
                id: "t".into(),
                name: "ping".into()
            },
            StreamEvent::ToolCallEnd { id: "t".into() },
            StreamEvent::Done(StopReason::EndTurn),
        ]
    );
}

#[test]
fn anthropic_thinking_without_signature_emits_no_reasoning_data() {
    let ev = run(
        &anthropic(),
        concat!(
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hm\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        ),
    );
    assert_eq!(
        ev,
        vec![
            StreamEvent::Reasoning("hm".into()),
            StreamEvent::Done(StopReason::EndTurn)
        ]
    );
}

#[test]
fn anthropic_interleaved_tool_blocks_keep_their_ids() {
    let d = |i: u32, f: &str| {
        format!(
            "data: {{\"type\":\"content_block_delta\",\"index\":{i},\"delta\":{{\"type\":\"input_json_delta\",\"partial_json\":\"{f}\"}}}}\n\n"
        )
    };
    let start = |i: u32, id: &str| {
        format!(
            "data: {{\"type\":\"content_block_start\",\"index\":{i},\"content_block\":{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"n\"}}}}\n\n"
        )
    };
    let stop = |i: u32| format!("data: {{\"type\":\"content_block_stop\",\"index\":{i}}}\n\n");
    let stream = [
        start(0, "A"),
        start(1, "B"),
        d(0, "a1"),
        d(1, "b1"),
        d(0, "a2"),
        stop(1),
        stop(0),
    ]
    .concat();
    let ev = run(&anthropic(), &stream);
    let inp = |id: &str, f: &str| StreamEvent::ToolCallInput {
        id: id.into(),
        json_fragment: f.into(),
    };
    assert_eq!(
        &ev[2..],
        &[
            inp("A", "a1"),
            inp("B", "b1"),
            inp("A", "a2"),
            StreamEvent::ToolCallEnd { id: "B".into() },
            StreamEvent::ToolCallEnd { id: "A".into() },
            StreamEvent::Error(ProviderError::Truncated),
        ]
    );
}

#[test]
fn an_endless_line_is_cut_off_with_an_error_not_buffered_forever() {
    for p in [&anthropic() as &dyn Provider, &openai(), &gemini()] {
        let mut d = p.decoder();
        let chunk = vec![b'x'; 1024 * 1024];
        let mut ev = Vec::new();
        for _ in 0..10 {
            ev.extend(d.push(&chunk));
        }
        ev.extend(d.finish());
        assert!(
            ev.iter()
                .any(|e| matches!(e, StreamEvent::Error(ProviderError::LineTooLong(_)))),
            "{ev:?}"
        );
        assert_eq!(
            ev.iter()
                .filter(|e| matches!(e, StreamEvent::Error(_)))
                .count(),
            1
        );
    }
}

#[test]
fn gemini_text_reasoning_and_usage() {
    let ev = run(
        &gemini(),
        include_str!("fixtures/llm/gemini_text_thinking.sse"),
    );
    assert_eq!(
        ev,
        vec![
            StreamEvent::Reasoning("Say hello in Japanese: 日本語".into()),
            StreamEvent::Text("Héllo → ".into()),
            StreamEvent::Text("こんにちは ✓".into()),
            // The last report, once, with thinking counted as output.
            usage(25, 42, 7, 0),
            StreamEvent::Done(StopReason::EndTurn),
        ]
    );
}

#[test]
fn gemini_calls_arrive_whole_and_keep_their_signature_on_the_call() {
    let ev = run(
        &gemini(),
        include_str!("fixtures/llm/gemini_tool_calls.sse"),
    );
    let anchor = serde_json::json!({"call": "fc_01", "signature": "CiQBVKhc7sig"}).to_string();
    assert_eq!(
        ev,
        vec![
            StreamEvent::Text("Reading both files…".into()),
            // A block to keep the signature on, then the call it belongs to.
            StreamEvent::Reasoning(String::new()),
            StreamEvent::ReasoningData(ReasoningData {
                provider: "gemini".into(),
                data: anchor,
            }),
            StreamEvent::ToolCallStart {
                id: "fc_01".into(),
                name: "lattice__read_file".into()
            },
            StreamEvent::ToolCallInput {
                id: "fc_01".into(),
                json_fragment: "{\"path\":\"src/lib.rs\"}".into()
            },
            StreamEvent::ToolCallEnd { id: "fc_01".into() },
            // No id on the wire: one is made, unique within the reply.
            StreamEvent::ToolCallStart {
                id: "gemini_call_1".into(),
                name: "lattice__read_file".into()
            },
            StreamEvent::ToolCallInput {
                id: "gemini_call_1".into(),
                json_fragment: "{\"path\":\"src/ü.rs\"}".into()
            },
            StreamEvent::ToolCallEnd {
                id: "gemini_call_1".into()
            },
            usage(40, 27, 0, 0),
            // `STOP` after calls means "run them".
            StreamEvent::Done(StopReason::ToolUse),
        ]
    );
}

#[test]
fn gemini_errors_and_blocked_prompts_surface_once() {
    let e = run(
        &gemini(),
        "data: {\"error\": {\"code\": 429, \"message\": \"Resource exhausted\", \"status\": \"RESOURCE_EXHAUSTED\"}}\n\n",
    );
    assert_eq!(
        e,
        vec![StreamEvent::Error(ProviderError::Api {
            kind: "RESOURCE_EXHAUSTED".into(),
            message: "Resource exhausted".into()
        })]
    );
    let b = run(
        &gemini(),
        "data: {\"promptFeedback\": {\"blockReason\": \"SAFETY\"}}\n\n",
    );
    assert_eq!(
        b,
        vec![StreamEvent::Error(ProviderError::Api {
            kind: "blocked".into(),
            message: "the prompt was blocked (SAFETY)".into()
        })]
    );
}

#[test]
fn gemini_unknown_chunks_are_skipped_and_other_finishes_are_named() {
    let ev = run(
        &gemini(),
        "data: {\"something\": \"else\"}\n\ndata: {\"candidates\": [{\"content\": {\"parts\": [{\"text\": \"x\"}]}, \"finishReason\": \"MAX_TOKENS\"}]}\n\n",
    );
    assert_eq!(
        ev,
        vec![
            StreamEvent::Text("x".into()),
            StreamEvent::Done(StopReason::MaxTokens)
        ]
    );
    let ev = run(
        &gemini(),
        "data: {\"candidates\": [{\"finishReason\": \"SAFETY\"}]}\n\n",
    );
    assert_eq!(
        ev,
        vec![StreamEvent::Done(StopReason::Other("safety".into()))]
    );
}
