//! The conversation model as a provider request's history.
//!
//! The model is the store and the buffer is its projection, so the messages a
//! provider is sent come from the [`Conversation`] and not from the buffer's
//! text. That is what lets reasoning data (a signature the provider checks on
//! the next turn) survive: no edit can reach it.

use super::{AssistantPart, Message, ReasoningData};
use crate::conversation::{Block, Conversation, Role};

/// Build the message history for the next request.
///
/// A user turn becomes one message of its text blocks. An assistant turn becomes
/// one message of its text and reasoning, with each reasoning block's round-trip
/// data re-attached; a turn with nothing to send (a reply that failed before
/// its first byte) is skipped, since providers reject an empty assistant
/// message. Tool calls are not sent until the tool loop exists.
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
                        Block::Text(s) => Some(s.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
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
            Role::Assistant => {
                let parts: Vec<AssistantPart> = turn
                    .blocks
                    .iter()
                    .enumerate()
                    .filter_map(|(b, block)| match block {
                        Block::Text(s) if !s.is_empty() => Some(AssistantPart::Text(s.clone())),
                        Block::Reasoning(s) => Some(AssistantPart::Reasoning {
                            text: s.clone(),
                            data: conv.reasoning_data.get(&(t, b)).map(|d| ReasoningData {
                                provider: d.provider.clone(),
                                data: d.data.clone(),
                            }),
                        }),
                        _ => None,
                    })
                    .collect();
                if !parts.is_empty() {
                    out.push(Message::Assistant(parts));
                }
            }
        }
    }
    out
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

    #[test]
    fn queued_prompts_the_error_and_empty_turns_are_not_history() {
        let mut c = Conversation::default();
        c.push_user_text("hi");
        c.queued.push("later".into());
        c.error = Some("boom".into());
        assert_eq!(messages(&c), vec![Message::User("hi".into())]);
    }
}
