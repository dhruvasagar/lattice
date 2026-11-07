//! The tool loop's rounds for a chat (LL.16, design `llm-layer.md` §6).
//!
//! A request that may use tools is one request in the registry with several
//! exchanges: the model answers, asks for tools, the tools run, their results
//! go back, and it answers again. [`ChatRounds`] is the part between two
//! exchanges, plugged into [`RequestRegistry::start_loop`](super::RequestRegistry::start_loop).
//!
//! **The conversation store is the record, and the next request is built from
//! it.** Each call and its result live in the chat's [`Conversation`] (shown
//! as a tool block, sent back through [`history::messages`]), so the request
//! that continues a loop and the request a later prompt sends are the same
//! projection of the same data. A cancel at any point leaves a call without a
//! result, and `history` answers such a call with an error: the next payload
//! is valid however the loop was stopped.
//!
//! The next exchange is the first request as sent, plus everything after its
//! last user message: settings, system prompt and context are not resolved
//! again mid-loop, so a loop keeps the terms it started with.

use std::sync::Arc;

use lattice_agent::SessionKey;
use tokio_util::sync::CancellationToken;

use super::request::{Round, Rounds};
use super::tools::{BoxFuture, Offer, ToolError};
use super::{LlmRequest, Message, history};
use crate::conversation::{Block, Conversation, ConversationStore, Role, ToolStatus};

/// The result a call gets when its request stopped before it returned.
pub const STOPPED: &str = "the call was stopped before it returned";

/// Runs a chat's tool calls between exchanges.
pub struct ChatRounds {
    store: ConversationStore,
    key: SessionKey,
    offer: Arc<Offer>,
    /// The request as first sent; each round extends a copy.
    first: LlmRequest,
    done: usize,
    limit: usize,
}

impl ChatRounds {
    pub fn new(
        store: ConversationStore,
        key: SessionKey,
        offer: Arc<Offer>,
        first: LlmRequest,
        limit: usize,
    ) -> Self {
        Self {
            store,
            key,
            offer,
            first,
            done: 0,
            limit: limit.max(1),
        }
    }

    fn set(&self, id: &str, status: ToolStatus, output: Option<String>) {
        self.store.mutate(&self.key, |c| {
            c.merge_tool_update(id, Some(status), None, None, output);
        });
    }

    async fn round(&mut self, cancel: &CancellationToken) -> Round {
        let pending = pending_calls(&self.store.snapshot());
        if pending.is_empty() {
            // The model said it wanted tools and named none.
            return Round::Finish;
        }
        if self.done >= self.limit {
            let why = format!(
                "not run: this request reached its limit of {} tool rounds",
                self.limit
            );
            for (id, _) in &pending {
                self.set(id, ToolStatus::Err, Some(why.clone()));
            }
            return Round::Fail(format!(
                "stopped after {} tool rounds (llm.tool_rounds); send a message to let it go on",
                self.limit
            ));
        }
        self.done += 1;
        for (id, call) in pending {
            if cancel.is_cancelled() {
                // The rest stay pending; the turn's end answers them.
                break;
            }
            self.set(&id, ToolStatus::Running, None);
            let result = match &call.malformed {
                Some(why) => Err(ToolError::InvalidArguments(why.clone())),
                None => self.offer.call(&call.name, call.input.clone()).await,
            };
            match result {
                Ok(out) => self.set(&id, ToolStatus::Ok, Some(out.text)),
                Err(e) => self.set(&id, ToolStatus::Err, Some(e.to_string())),
            }
        }
        Round::Next(self.next_request())
    }

    /// The first request plus everything after its last user message.
    fn next_request(&self) -> LlmRequest {
        let all = history::messages(&self.store.snapshot());
        let start = all
            .iter()
            .rposition(|m| matches!(m, Message::User(_)))
            .map_or(0, |i| i + 1);
        let mut next = self.first.clone();
        next.messages.extend(all.into_iter().skip(start));
        next
    }
}

impl Rounds for ChatRounds {
    fn after_tools<'a>(&'a mut self, cancel: &'a CancellationToken) -> BoxFuture<'a, Round> {
        Box::pin(self.round(cancel))
    }
}

/// The latest assistant turn's calls that are complete and not yet run, in
/// the order the model made them.
fn pending_calls(conv: &Conversation) -> Vec<(String, crate::conversation::ToolCallData)> {
    let Some(turn) = conv.turns.last().filter(|t| t.role == Role::Assistant) else {
        return Vec::new();
    };
    turn.blocks
        .iter()
        .filter_map(|b| match b {
            Block::ToolCall {
                id,
                status: ToolStatus::Pending,
                ..
            } => conv.tool_calls.get(id).map(|d| (id.clone(), d.clone())),
            _ => None,
        })
        .collect()
}
