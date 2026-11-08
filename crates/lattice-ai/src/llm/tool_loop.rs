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

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use lattice_agent::SessionKey;
use tokio_util::sync::CancellationToken;

use super::request::{Round, RoundStatus, Rounds};
use super::tools::{Answer, AskFn, BoxFuture, CallContext, Offer, Question, ToolError};
use super::{LlmRequest, Message, history};
use crate::conversation::{
    Block, Conversation, ConversationStore, PermissionKind, PermissionOption, Role, ToolStatus,
};

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
    /// The session's diff tag and its `<C-t>` trust, read per call so a
    /// toggle mid-loop applies to the next edit.
    origin: u64,
    trusted: Arc<AtomicBool>,
    /// Tools allowed "for this chat" (LL.18): asked no more in it.
    allowed: Arc<Mutex<HashSet<String>>>,
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
            origin: 0,
            trusted: Arc::default(),
            allowed: Arc::default(),
        }
    }

    /// The session the calls belong to (LL.17): its edits' diffs carry
    /// `origin`, and apply without one while `trusted` is set.
    pub fn for_session(
        mut self,
        origin: u64,
        trusted: Arc<AtomicBool>,
        allowed: Arc<Mutex<HashSet<String>>>,
    ) -> Self {
        self.origin = origin;
        self.trusted = trusted;
        self.allowed = allowed;
        self
    }

    fn set(&self, id: &str, status: ToolStatus, output: Option<String>) {
        self.store.mutate(&self.key, |c| {
            c.merge_tool_update(id, Some(status), None, None, output);
        });
    }

    async fn round(&mut self, cancel: &CancellationToken, status: &RoundStatus) -> Round {
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
            let status = status.clone();
            let ctx = CallContext {
                origin: self.origin,
                trusted: self.trusted.load(Ordering::Acquire),
                on_review: Some(Arc::new(move |open| status.reviewing(open))),
                ask: Some(asker(
                    self.store.clone(),
                    self.key.clone(),
                    self.allowed.clone(),
                )),
            };
            let result = match &call.malformed {
                Some(why) => Err(ToolError::InvalidArguments(why.clone())),
                None => self.offer.call(&call.name, call.input.clone(), ctx).await,
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
    fn after_tools<'a>(
        &'a mut self,
        cancel: &'a CancellationToken,
        status: &'a RoundStatus,
    ) -> BoxFuture<'a, Round> {
        Box::pin(self.round(cancel, status))
    }
}

static NEXT_QUESTION: AtomicU64 = AtomicU64::new(1);

const ALLOW_ONCE: &str = "allow-once";
const ALLOW_CHAT: &str = "allow-chat";
const DENY: &str = "deny";

/// Puts a tool's question in the chat's store, where the transcript shows it
/// and the shared permission popup opens over it (LL.18), and waits for the
/// answer. An `Esc` in the popup leaves it pending and the call waiting;
/// dropping the call (the request stopped) withdraws it as denied, so no
/// question stays open that nothing is waiting on.
fn asker(store: ConversationStore, key: SessionKey, allowed: Arc<Mutex<HashSet<String>>>) -> AskFn {
    Arc::new(move |q: Question| {
        let (store, key, allowed) = (store.clone(), key.clone(), allowed.clone());
        Box::pin(async move {
            if allowed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&q.allow_key())
            {
                return Answer::Once;
            }
            let id = format!("llm-ask-{}", NEXT_QUESTION.fetch_add(1, Ordering::Relaxed));
            let (tx, rx) = tokio::sync::oneshot::channel();
            store.push_permission_request(
                &key,
                id.clone(),
                q.title.clone(),
                q.detail.clone(),
                vec![
                    PermissionOption::new(ALLOW_ONCE, "Allow once", PermissionKind::AllowOnce),
                    PermissionOption::new(
                        ALLOW_CHAT,
                        q.chat_allow_label(),
                        PermissionKind::AllowAlways,
                    ),
                    PermissionOption::new(DENY, "Deny", PermissionKind::RejectOnce),
                ],
                tx,
            );
            let withdraw = Withdraw {
                store: store.clone(),
                id,
            };
            let answer = match rx.await.as_deref() {
                Ok(ALLOW_ONCE) => Answer::Once,
                Ok(ALLOW_CHAT) => {
                    allowed
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(q.allow_key());
                    Answer::ForChat
                }
                _ => Answer::Deny,
            };
            drop(withdraw);
            answer
        })
    })
}

/// Resolves a question as denied when its call goes away unanswered. After an
/// answer it is a no-op: the store has already let the id go.
struct Withdraw {
    store: ConversationStore,
    id: String,
}

impl Drop for Withdraw {
    fn drop(&mut self) {
        self.store.resolve_permission(&self.id, DENY);
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
