//! Review focus 1: a multi-byte character split across network chunks must
//! decode exactly as if the stream arrived whole. Every fixture is fed whole,
//! then split at every byte boundary (so each multi-byte character is cut at
//! every interior position), and the events must be identical.

use lattice_ai::llm::{AnthropicProvider, OpenAiProvider, Provider, StreamEvent};

const FIXTURES: &[(&str, &str, bool)] = &[
    (
        "anthropic_text_thinking",
        include_str!("fixtures/llm/anthropic_text_thinking.sse"),
        true,
    ),
    (
        "anthropic_tool_use",
        include_str!("fixtures/llm/anthropic_tool_use.sse"),
        true,
    ),
    (
        "openai_text_reasoning",
        include_str!("fixtures/llm/openai_text_reasoning.sse"),
        false,
    ),
    (
        "openai_tool_calls",
        include_str!("fixtures/llm/openai_tool_calls.sse"),
        false,
    ),
    (
        "openai_ollama_no_usage",
        include_str!("fixtures/llm/openai_ollama_no_usage.sse"),
        false,
    ),
];

fn provider(anthropic: bool) -> Box<dyn Provider> {
    if anthropic {
        Box::new(AnthropicProvider::default())
    } else {
        Box::new(OpenAiProvider::new("http://localhost/v1"))
    }
}

fn whole(p: &dyn Provider, bytes: &[u8]) -> Vec<StreamEvent> {
    let mut d = p.decoder();
    let mut ev = d.push(bytes);
    ev.extend(d.finish());
    ev
}

fn chunked(p: &dyn Provider, bytes: &[u8], at: usize) -> Vec<StreamEvent> {
    let mut d = p.decoder();
    let mut ev = d.push(&bytes[..at]);
    ev.extend(d.push(&bytes[at..]));
    ev.extend(d.finish());
    ev
}

#[test]
fn every_fixture_decodes_identically_split_at_every_byte() {
    for (name, text, anthropic) in FIXTURES {
        let p = provider(*anthropic);
        let bytes = text.as_bytes();
        let expected = whole(p.as_ref(), bytes);
        assert!(
            !expected.iter().any(|e| matches!(e, StreamEvent::Error(_))),
            "{name}: whole stream errored: {expected:?}"
        );
        for at in 0..=bytes.len() {
            assert_eq!(
                chunked(p.as_ref(), bytes, at),
                expected,
                "{name} split at {at}"
            );
        }
        // One byte at a time, the worst case.
        let mut d = p.decoder();
        let mut ev: Vec<_> = bytes.iter().flat_map(|b| d.push(&[*b])).collect();
        ev.extend(d.finish());
        assert_eq!(ev, expected, "{name} byte-at-a-time");
    }
}

#[test]
fn multibyte_text_survives_without_replacement_characters() {
    let text = include_str!("fixtures/llm/anthropic_text_thinking.sse");
    let p = AnthropicProvider::default();
    let at = text.find('日').expect("fixture has multi-byte text") + 1;
    let ev = chunked(&p, text.as_bytes(), at);
    let joined: String = ev
        .iter()
        .filter_map(|e| match e {
            StreamEvent::Text(t) | StreamEvent::Reasoning(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert!(joined.contains("日本語") && joined.contains("こんにちは ✓"));
    assert!(!joined.contains('\u{fffd}'));
}
