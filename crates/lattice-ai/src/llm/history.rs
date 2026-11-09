//! The conversation model as a provider request's history.
//!
//! The model is the store and the buffer is its projection, so the messages a
//! provider is sent come from the [`Conversation`] and not from the buffer's
//! text. That is what lets reasoning data (a signature the provider checks on
//! the next turn) survive: no edit can reach it.

use super::{AssistantPart, Message, ReasoningData};
use crate::conversation::{Block, Conversation, Role, ToolStatus, Turn};

/// Build the message history for the next request.
///
/// A user turn becomes one message of its text blocks. An assistant turn becomes
/// one message of its text and reasoning, with each reasoning block's round-trip
/// data re-attached; a turn with nothing to send (a reply that failed before
/// its first byte) is skipped, since providers reject an empty assistant
/// message. A turn that used tools carries its calls and their results in
/// order (see [`assistant_messages`]).
///
/// A user turn left without a reply by the next user turn is dropped. Queued
/// prompts and the shown error are not history.
pub fn messages(conv: &Conversation) -> Vec<Message> {
    let mut out = Vec::new();
    for (t, turn) in conv.turns.iter().enumerate() {
        match turn.role {
            Role::User => {
                let text = turn
                    .blocks
                    .iter()
                    .filter_map(|b| match b {
                        Block::Text(s) => Some(s.clone()),
                        Block::Source {
                            label, lang, text, ..
                        } => Some(source_message(label, lang, text)),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
                if !text.is_empty() {
                    // A user turn whose reply never came (it failed, or was
                    // interrupted first) is superseded: two user messages in a
                    // row are rejected by providers.
                    if matches!(out.last(), Some(Message::User(_))) {
                        out.pop();
                    }
                    out.push(Message::User(text));
                }
            }
            Role::Assistant => assistant_messages(conv, t, turn, &mut out),
        }
    }
    out
}

/// One assistant turn as messages. A turn that used tools holds several
/// exchanges (text, calls, then more text after their results), so it splits
/// at each run of calls: the assistant message ends with its calls, and a
/// result for **every** call follows before anything else does. Providers
/// reject a call left without a result, so a call that never got one (the
/// request was stopped while it ran) is sent back with an error result saying
/// so; a call whose arguments never finished arriving was never a call the
/// model could expect an answer to, and is left out with nothing to answer.
fn assistant_messages(conv: &Conversation, t: usize, turn: &Turn, out: &mut Vec<Message>) {
    let mut parts: Vec<AssistantPart> = Vec::new();
    let mut results: Vec<Message> = Vec::new();
    let flush =
        |parts: &mut Vec<AssistantPart>, results: &mut Vec<Message>, out: &mut Vec<Message>| {
            if !parts.is_empty() {
                out.push(Message::Assistant(std::mem::take(parts)));
            }
            out.append(results);
        };
    for (b, block) in turn.blocks.iter().enumerate() {
        let part = match block {
            Block::ToolCall {
                id, status, output, ..
            } => {
                let Some(call) = conv.tool_calls.get(id) else {
                    continue;
                };
                parts.push(AssistantPart::ToolCall {
                    id: id.clone(),
                    name: call.name.clone(),
                    input: call.input.clone(),
                });
                results.push(tool_result(id, *status, output.as_deref()));
                continue;
            }
            Block::Text(s) if !s.is_empty() => AssistantPart::Text(s.clone()),
            Block::Reasoning(s) => AssistantPart::Reasoning {
                text: s.clone(),
                data: conv.reasoning_data.get(&(t, b)).map(|d| ReasoningData {
                    provider: d.provider.clone(),
                    data: d.data.clone(),
                }),
            },
            _ => continue,
        };
        // Content after calls is the next exchange: answer the calls first.
        if !results.is_empty() {
            flush(&mut parts, &mut results, out);
        }
        parts.push(part);
    }
    flush(&mut parts, &mut results, out);
}

/// A call's result as the model gets it back.
fn tool_result(id: &str, status: ToolStatus, output: Option<&str>) -> Message {
    let (content, is_error) = match (status, output) {
        (ToolStatus::Ok, Some(text)) => (text.to_string(), false),
        (ToolStatus::Err, Some(text)) => (text.to_string(), true),
        _ => ("the call was stopped before it returned".to_string(), true),
    };
    Message::ToolResult {
        id: id.to_string(),
        content,
        is_error,
    }
}

/// How sent text reaches the model: its label, then the text in a fence long
/// enough that nothing inside it can close it. The model sees what the user
/// saw, with the place it came from.
fn source_message(label: &str, lang: &str, text: &str) -> String {
    let mut run = 0;
    let mut best = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        best = best.max(run);
    }
    let fence = "`".repeat(best.max(2) + 1);
    format!("From {label}:\n{fence}{lang}\n{text}\n{fence}")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::conversation::OpaqueReasoning;

    #[test]
    fn a_two_turn_exchange_round_trips_with_reasoning_data() {
        let mut c = Conversation::default();
        c.push_user_text("why?");
        c.extend_reasoning("because");
        c.attach_reasoning_data(OpaqueReasoning {
            provider: "anthropic".into(),
            data: "sig-1".into(),
        });
        c.extend_text(Role::Assistant, "it is so");
        c.push_user_text("and?");

        let m = messages(&c);
        assert_eq!(m.len(), 3);
        assert_eq!(m[0], Message::User("why?".into()));
        assert_eq!(
            m[1],
            Message::Assistant(vec![
                AssistantPart::Reasoning {
                    text: "because".into(),
                    data: Some(ReasoningData {
                        provider: "anthropic".into(),
                        data: "sig-1".into()
                    }),
                },
                AssistantPart::Text("it is so".into()),
            ])
        );
        assert_eq!(m[2], Message::User("and?".into()));
    }

    #[test]
    fn data_attaches_to_the_block_it_followed_not_a_later_one() {
        let mut c = Conversation::default();
        c.push_user_text("q");
        c.extend_reasoning("first");
        c.attach_reasoning_data(OpaqueReasoning {
            provider: "anthropic".into(),
            data: "s".into(),
        });
        c.extend_text(Role::Assistant, "mid");
        c.extend_reasoning("second");
        let Message::Assistant(parts) = &messages(&c)[1] else {
            panic!("assistant message");
        };
        assert!(matches!(
            &parts[0],
            AssistantPart::Reasoning { data: Some(_), .. }
        ));
        assert!(matches!(
            &parts[2],
            AssistantPart::Reasoning { data: None, .. }
        ));
    }

    /// LL.10b: sent text reaches the model with its place, in a fence no
    /// backtick inside it can close, before the instruction.
    #[test]
    fn a_source_turn_sends_its_label_and_fenced_text_then_the_instruction() {
        let mut c = Conversation::default();
        c.push_user_source(
            Block::Source {
                label: "a.md:1".into(),
                lang: "markdown".into(),
                text: "```\ncode\n```".into(),
                link: None,
            },
            "fix it",
        );
        assert_eq!(
            messages(&c),
            vec![Message::User(
                "From a.md:1:\n````markdown\n```\ncode\n```\n````\n\nfix it".into()
            )]
        );
    }

    #[test]
    fn queued_prompts_the_error_and_empty_turns_are_not_history() {
        let mut c = Conversation::default();
        c.push_user_text("hi");
        c.queued.push("later".into());
        c.error = Some("boom".into());
        assert_eq!(messages(&c), vec![Message::User("hi".into())]);
    }

    fn call(c: &mut Conversation, id: &str, status: ToolStatus, output: Option<&str>) {
        c.push_tool_call(
            id.into(),
            None,
            "read_file".into(),
            status,
            crate::conversation::ToolKind::Read,
            None,
            output.map(str::to_string),
        );
        c.tool_calls.insert(
            id.into(),
            crate::conversation::ToolCallData {
                name: "lattice__read_file".into(),
                input: serde_json::json!({"path": id}),
                malformed: None,
            },
        );
    }

    /// LL.16: one assistant turn with two exchanges becomes assistant (calls),
    /// their results, then assistant again.
    #[test]
    fn a_turn_that_used_tools_splits_at_its_calls() {
        let mut c = Conversation::default();
        c.push_user_text("look");
        c.extend_text(Role::Assistant, "reading");
        call(&mut c, "a", ToolStatus::Ok, Some("A"));
        call(&mut c, "b", ToolStatus::Err, Some("no such file"));
        c.extend_text(Role::Assistant, "done");

        let m = messages(&c);
        assert_eq!(m.len(), 5, "{m:#?}");
        let Message::Assistant(first) = &m[1] else {
            panic!("{m:#?}")
        };
        assert_eq!(first.len(), 3);
        assert!(matches!(&first[2], AssistantPart::ToolCall { id, .. } if id == "b"));
        assert_eq!(
            m[2],
            Message::ToolResult {
                id: "a".into(),
                content: "A".into(),
                is_error: false
            }
        );
        assert_eq!(
            m[3],
            Message::ToolResult {
                id: "b".into(),
                content: "no such file".into(),
                is_error: true
            }
        );
        assert_eq!(
            m[4],
            Message::Assistant(vec![AssistantPart::Text("done".into())])
        );
    }

    /// A call stopped before its result is answered with an error; one whose
    /// arguments never arrived is not sent at all.
    #[test]
    fn every_sent_call_has_a_result_however_the_loop_stopped() {
        let mut c = Conversation::default();
        c.push_user_text("look");
        call(&mut c, "a", ToolStatus::Running, None);
        c.push_tool_call(
            "half".into(),
            None,
            "read_file".into(),
            ToolStatus::Pending,
            crate::conversation::ToolKind::Read,
            None,
            None,
        );
        c.push_user_text("never mind");

        let m = messages(&c);
        assert_eq!(m.len(), 4, "{m:#?}");
        let Message::Assistant(parts) = &m[1] else {
            panic!("{m:#?}")
        };
        assert_eq!(parts.len(), 1, "the half-arrived call is left out");
        assert!(matches!(
            &m[2],
            Message::ToolResult { id, is_error: true, .. } if id == "a"
        ));
        assert_eq!(m[3], Message::User("never mind".into()));
    }
}
