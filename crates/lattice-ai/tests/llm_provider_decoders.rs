//! Recorded-shape fixtures per provider: text, reasoning, tool calls, usage,
//! plus the failure modes (truncation, unknown events, garbage input).

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
    for p in [&anthropic() as &dyn Provider, &openai()] {
        for input in inputs {
            let mut d = p.decoder();
            let _ = d.push(input);
            let _ = d.finish();
        }
    }
}
