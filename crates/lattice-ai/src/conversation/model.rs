//! Protocol-neutral conversation model (LL.1).
//!
//! A [`Conversation`] is a turn/block tree the `ai-conversation` mode projects
//! into a buffer. It names no wire protocol: ACP's `SessionUpdate` is folded
//! into it by the adapter in `acp::conversation`, and the native LLM layer will
//! fold its own streamed events the same way. The mutators here
//! ([`Conversation::push_tool_call`], [`Conversation::merge_tool_update`], ...)
//! are the neutral surface those adapters drive.
//!
//! The mutators are pure and unit-testable (no I/O, no locks).
//! [`ConversationStore`] wraps a [`Conversation`] with a shared mutex + a
//! [`ConversationUpdated`] bus publish so the mode can live-tail.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use std::fmt;
use tokio::sync::oneshot;

use lattice_agent::SessionKey;

/// Who produced a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// Execution state of a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Pending,
    Running,
    Ok,
    Err,
}

/// What a tool call does, coarsely: enough for a projection to pick a glyph or
/// a fold default without knowing the protocol that produced the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolKind {
    Read,
    Search,
    Edit,
    Execute,
    #[default]
    Other,
}

/// Review state of an agent-proposed file edit. AU‑4 drives the transitions and
/// attaches the `review_diff` session; AU‑1 only defines the shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditStatus {
    Proposed,
    Accepted,
    Rejected,
}

/// AUX‑1: status of an inline permission request shown in the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionStatus {
    Pending,
    Allowed,
    Denied,
}

/// What choosing a [`PermissionOption`] means. Protocol adapters map their own
/// option kinds onto these; an unrecognised kind maps to [`Other`](Self::Other),
/// which [`ConversationStore::resolve_permission`] treats as a denial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
    Other,
}

/// One choice offered with a permission request. `option_id` is the opaque
/// token the adapter maps back to its protocol's own option on resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: PermissionKind,
}

impl PermissionOption {
    pub fn new(
        option_id: impl Into<String>,
        name: impl Into<String>,
        kind: PermissionKind,
    ) -> Self {
        Self {
            option_id: option_id.into(),
            name: name.into(),
            kind,
        }
    }
}

/// PU-B.2b: a pending permission request projected for the popup menu.
/// `ai-permission-mode`'s `on_activate` reads [`ConversationStore::
/// oldest_pending_permission`] and renders `title`, optional `description`, and
/// one line per `options` entry (wire order); the select handler maps the chosen
/// line back to `options[i].option_id`.
#[derive(Debug, Clone)]
pub struct PendingPermissionView {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub options: Vec<PermissionOption>,
}

/// AUX‑3: global processing status of the current agent session — shown in the
/// conversation buffer's headerline.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SessionStatus {
    /// No active turn: the agent is not processing anything.
    #[default]
    Idle,
    /// The agent is streaming a text/thought response.
    Thinking,
    /// The agent is executing a tool call.
    Executing {
        /// Human-readable tool name (e.g. "edit parse.rs").
        tool: String,
    },
    /// The agent is awaiting the user's decision on a permission request.
    AwaitingPermission,
}

impl fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionStatus::Idle => write!(f, "Ready"),
            SessionStatus::Thinking => write!(f, "Thinking\u{2026}"),
            SessionStatus::Executing { tool } => write!(f, "Working: {tool}"),
            SessionStatus::AwaitingPermission => write!(f, "Awaiting your approval\u{2026}"),
        }
    }
}

/// AUX‑2: cost of the session so far, as reported by the provider.
#[derive(Debug, Clone, PartialEq)]
pub struct Cost {
    pub amount: f64,
    pub currency: String,
}

/// AUX‑2: latest token and cost snapshot reported by the session.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageSnapshot {
    pub used: u64,
    pub size: u64,
    pub cost: Option<Cost>,
}

/// One renderable unit within a turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// Streamed message text.
    Text(String),
    /// Streamed reasoning / thinking. TCF: the projection prefixes each line
    /// with `│`, and a multi-line block is folded **closed by default** via
    /// [`ReasoningFoldSource`](crate::conversation::tool_fold::ReasoningFoldSource) — the
    /// fold keeps the head line visible and hides the rest until `za`. (Before
    /// TCF this comment claimed folding the projection did not do.)
    Reasoning(String),
    /// A tool invocation and its live status.
    ///
    /// TCF: `kind`, `input` and `output` are the detail an expanded tool call
    /// shows. `input`/`output` are the agent's raw JSON, pretty-printed at
    /// ingest (stored as `String`, not `serde_json::Value`, so `Block` stays
    /// `Eq`). Detail commonly arrives on a later `ToolCallUpdate`, not the
    /// initial call, so it is merged in as it lands.
    ToolCall {
        id: String,
        /// Which party ran the call, for providers that distinguish one (an
        /// MCP server name, say). ACP never sets it.
        origin: Option<String>,
        title: String,
        status: ToolStatus,
        kind: ToolKind,
        input: Option<String>,
        output: Option<String>,
    },
    /// An agent-proposed file edit (AU‑4 wires the diff review).
    Edit { path: String, status: EditStatus },
    /// LL.10b: text the user sent from a buffer, as the first part of their
    /// turn. Shown as a head line naming where it came from over the text in
    /// a fence, folded when long, so what was sent stays visible without
    /// burying the answer; the model gets it with its label (see
    /// `llm::history`). A user's typed instruction follows as a `Text` block.
    Source {
        /// Where it came from, for the head line and for the model:
        /// `foo.rs:3-10`.
        label: String,
        /// The fence's language (`rust`), or `text` when unknown.
        lang: String,
        text: String,
        /// Where `gf` on the block goes back to; `None` when the source has
        /// no buffer to return to.
        link: Option<SourceLink>,
    },
    /// AUX‑1: an inline permission request awaiting or reflecting user action.
    Permission {
        id: String,
        title: String,
        description: Option<String>,
        options: Vec<PermissionOption>,
        status: PermissionStatus,
    },
}

/// Where a [`Block::Source`] came from: the buffer and the first line sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceLink {
    pub buffer: lattice_core::BufferId,
    /// 0-based.
    pub line: u32,
}

/// One turn: a role and its ordered blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub role: Role,
    pub blocks: Vec<Block>,
}

/// LL.16: a model's tool call as it must be sent back: the name it called (the
/// wire name, `lattice__read_file`) and its arguments. `input` is the parsed
/// object, or `{}` when the model's arguments were not valid JSON (the call's
/// result then says so), since a provider rejects a call whose input is not
/// an object.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallData {
    pub name: String,
    pub input: serde_json::Value,
    /// Why the arguments could not be used, when they could not.
    pub malformed: Option<String>,
}

/// Provider data that must come back verbatim with the reasoning it belongs to
/// (a signature, say). Opaque to this module: `provider` names whose it is, so a
/// different provider can drop it on the next request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpaqueReasoning {
    pub provider: String,
    pub data: String,
}

/// The full conversation as a turn/block tree. Streaming *extends* the last
/// block; earlier turns are never rewritten.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Conversation {
    pub turns: Vec<Turn>,
    /// AUX‑2: latest token usage snapshot from `usage_update` notifications.
    pub usage: Option<UsageSnapshot>,
    /// AUX‑3: global processing status derived by the supervisor from the active
    /// turn's state. Set via [`ConversationStore::set_status`].
    pub status: SessionStatus,
    /// Who the projection labels assistant turns as. `None` keeps the ACP
    /// default (`opencode`); a native chat sets its model name.
    pub assistant_label: Option<String>,
    /// Round-trip data for `Reasoning` blocks, keyed by `(turn, block)` index.
    /// Turns are append-only, so an index names the same block for life.
    pub reasoning_data: std::collections::BTreeMap<(usize, usize), OpaqueReasoning>,
    /// LL.16: what the model sent for each of its tool calls, by call id: the
    /// name it called and the arguments, as the provider needs them back on
    /// the next request. The `ToolCall` block shows them; this keeps them
    /// exact. A call whose arguments never finished arriving has no entry,
    /// and is not sent back.
    pub tool_calls: std::collections::BTreeMap<String, ToolCallData>,
    /// The last failure, shown after the transcript and cleared by the next
    /// send. Not part of the history a model is sent.
    pub error: Option<String>,
    /// Prompts submitted while a reply was streaming, oldest first. They join
    /// the transcript as user turns when they are actually sent, so a streaming
    /// reply can never be split by a later question.
    pub queued: Vec<String>,
}

impl Conversation {
    /// The label assistant turns carry in the projection.
    pub fn assistant_label(&self) -> &str {
        self.assistant_label.as_deref().unwrap_or("opencode")
    }

    /// How many prompts have been submitted: sent as user turns, or queued.
    /// Only a submission raises it (a queued prompt becoming a turn leaves it
    /// unchanged), so an increase is the drain's "clear the prompt" signal.
    pub fn submission_count(&self) -> usize {
        self.turns.iter().filter(|t| t.role == Role::User).count() + self.queued.len()
    }

    /// Attach round-trip data to the most recent `Reasoning` block.
    pub fn attach_reasoning_data(&mut self, data: OpaqueReasoning) {
        let Some((t, turn)) = self
            .turns
            .iter()
            .enumerate()
            .rev()
            .find(|(_, turn)| turn.role == Role::Assistant)
        else {
            return;
        };
        if let Some(b) = turn
            .blocks
            .iter()
            .rposition(|b| matches!(b, Block::Reasoning(_)))
        {
            self.reasoning_data.insert((t, b), data);
        }
    }

    /// AU‑3: append a complete user prompt as a new `User` turn. Each Enter is
    /// a distinct turn (the terminal-REPL model), so — unlike chunk-streamed
    /// agent output — this always opens a fresh turn rather than extending.
    pub fn push_user_text(&mut self, text: &str) {
        self.turns.push(Turn {
            role: Role::User,
            blocks: vec![Block::Text(text.to_string())],
        });
    }

    /// LL.10b: a user turn that sends `source` from a buffer, followed by the
    /// typed `instruction` when there is one.
    pub fn push_user_source(&mut self, source: Block, instruction: &str) {
        let mut blocks = vec![source];
        if !instruction.is_empty() {
            blocks.push(Block::Text(instruction.to_string()));
        }
        self.turns.push(Turn {
            role: Role::User,
            blocks,
        });
    }

    /// Append `text` to the last block if it is `Text` in a turn of `role`;
    /// otherwise open a new block / turn as needed.
    pub fn extend_text(&mut self, role: Role, text: &str) {
        match self.turns.last_mut() {
            Some(turn) if turn.role == role => match turn.blocks.last_mut() {
                Some(Block::Text(s)) => s.push_str(text),
                _ => turn.blocks.push(Block::Text(text.to_string())),
            },
            _ => self.turns.push(Turn {
                role,
                blocks: vec![Block::Text(text.to_string())],
            }),
        }
    }

    /// Append `text` to the last `Reasoning` block (assistant turn), opening one
    /// as needed.
    pub fn extend_reasoning(&mut self, text: &str) {
        match self.turns.last_mut() {
            Some(turn) if turn.role == Role::Assistant => match turn.blocks.last_mut() {
                Some(Block::Reasoning(s)) => s.push_str(text),
                _ => turn.blocks.push(Block::Reasoning(text.to_string())),
            },
            _ => self.turns.push(Turn {
                role: Role::Assistant,
                blocks: vec![Block::Reasoning(text.to_string())],
            }),
        }
    }

    /// Push a new `ToolCall` block onto the current (or a fresh) assistant turn.
    #[allow(clippy::too_many_arguments)]
    pub fn push_tool_call(
        &mut self,
        id: String,
        origin: Option<String>,
        title: String,
        status: ToolStatus,
        kind: ToolKind,
        input: Option<String>,
        output: Option<String>,
    ) {
        let block = Block::ToolCall {
            id,
            origin,
            title,
            status,
            kind,
            input,
            output,
        };
        match self.turns.last_mut() {
            Some(turn) if turn.role == Role::Assistant => turn.blocks.push(block),
            _ => self.turns.push(Turn {
                role: Role::Assistant,
                blocks: vec![block],
            }),
        }
    }

    /// AUX‑1: push a `Permission` block onto the current (or a fresh) assistant turn.
    pub fn push_permission_block(
        &mut self,
        id: String,
        title: String,
        description: Option<String>,
        options: Vec<PermissionOption>,
    ) {
        let block = Block::Permission {
            id,
            title,
            description,
            options,
            status: PermissionStatus::Pending,
        };
        match self.turns.last_mut() {
            Some(turn) if turn.role == Role::Assistant => turn.blocks.push(block),
            _ => self.turns.push(Turn {
                role: Role::Assistant,
                blocks: vec![block],
            }),
        }
    }

    /// PU-B.2: kind of the option `option_id` within the `Permission` block
    /// `id` (searched newest-first), or `None` if the block or option is gone.
    /// Used by `resolve_permission` to derive the inline block status from the
    /// agent's actual option rather than a fixed 4-way bucket.
    pub fn permission_option_kind(&self, id: &str, option_id: &str) -> Option<PermissionKind> {
        for turn in self.turns.iter().rev() {
            for block in turn.blocks.iter().rev() {
                if let Block::Permission {
                    id: bid, options, ..
                } = block
                    && bid == id
                {
                    return options
                        .iter()
                        .find(|o| o.option_id == option_id)
                        .map(|o| o.kind);
                }
            }
        }
        None
    }

    /// AUX‑1: update the status of the `Permission` block with `id`.
    pub fn update_permission_status(&mut self, id: &str, new_status: PermissionStatus) {
        for turn in self.turns.iter_mut().rev() {
            for block in turn.blocks.iter_mut().rev() {
                if let Block::Permission {
                    id: bid, status, ..
                } = block
                    && bid == id
                {
                    *status = new_status;
                    return;
                }
            }
        }
    }

    /// LL.16: every tool call still pending or running is ended as failed,
    /// with `why` as its output: a request that stopped leaves no call
    /// showing as running, and the next request answers it with that error.
    pub fn settle_tool_calls(&mut self, why: &str) {
        for turn in &mut self.turns {
            for block in &mut turn.blocks {
                if let Block::ToolCall { status, output, .. } = block
                    && matches!(status, ToolStatus::Pending | ToolStatus::Running)
                {
                    *status = ToolStatus::Err;
                    *output = Some(why.to_string());
                }
            }
        }
    }

    /// TCF: merge a `ToolCallUpdate` into the tool-call block with `id`
    /// (searched newest-first). Each `Some` field overwrites; `None` leaves the
    /// existing value — detail (input/output/kind) commonly arrives on an
    /// update after the initial call, so this accumulates rather than replaces.
    pub fn merge_tool_update(
        &mut self,
        id: &str,
        new_status: Option<ToolStatus>,
        new_kind: Option<ToolKind>,
        new_input: Option<String>,
        new_output: Option<String>,
    ) {
        for turn in self.turns.iter_mut().rev() {
            for block in turn.blocks.iter_mut().rev() {
                if let Block::ToolCall {
                    id: bid,
                    status,
                    kind,
                    input,
                    output,
                    ..
                } = block
                    && bid == id
                {
                    if let Some(s) = new_status {
                        *status = s;
                    }
                    if let Some(k) = new_kind {
                        *kind = k;
                    }
                    if new_input.is_some() {
                        *input = new_input;
                    }
                    if new_output.is_some() {
                        *output = new_output;
                    }
                    return;
                }
            }
        }
    }
}

/// Fired after the [`ConversationStore`] mutates. The `ai-conversation` mode
/// subscribes to it and re-projects (mirrors `AiLogPushed`).
#[derive(Debug, Clone)]
pub struct ConversationUpdated {
    pub session: SessionKey,
}

lattice_protocol::register_event!(
    ConversationUpdated,
    "ai.conversation-updated",
    "Fired after the ACP agent conversation model changes; drives live refresh \
     of the *ai:<provider>* conversation buffer.",
    "lattice-ai",
);

/// Fired by the `ai-conversation` mode's drain AFTER a re-projection edit has
/// LANDED in the buffer. Boot wakes the editor actor on this (via
/// `wake_on_event`) so a streamed agent response repaints WITHOUT a keystroke,
/// and the per-tick prompt-focus callback runs.
///
/// Distinct from [`ConversationUpdated`], which the supervisor fires when the
/// *model* changes — that is BEFORE the drain re-projects the buffer, so waking
/// on it would repaint stale content. Sequencing the wake after the owner-write
/// edit (this event) is what makes the last streamed chunk paint reliably.
#[derive(Debug, Clone, Default)]
pub struct ConversationProjected;

lattice_protocol::register_event!(
    ConversationProjected,
    "ai.conversation-projected",
    "Fired after the *ai:<provider>* conversation buffer is re-projected; wakes \
     the render loop so streamed agent responses repaint without a keystroke.",
    "lattice-ai",
);

/// Shared, mutable conversation state plus a bus publisher. The supervisor holds
/// one clone and folds updates in via [`ConversationStore::mutate`]; the `ai-conversation` mode
/// reads [`ConversationStore::snapshot`] on each `ConversationUpdated`.
#[derive(Clone)]
pub struct ConversationStore {
    inner: Arc<Mutex<Conversation>>,
    publish: Arc<dyn Fn(ConversationUpdated) + Send + Sync>,
    /// AUX‑1: pending permission request responders keyed by tool-call id.
    /// PU-B.2: the oneshot carries the chosen [`PermissionOption::option_id`]
    /// (wire order, any arity) — the menu resolves by the option the agent
    /// actually offered, and the protocol adapter maps it back to its own type.
    pending_permissions: Arc<Mutex<HashMap<String, (SessionKey, oneshot::Sender<String>)>>>,
}

impl ConversationStore {
    /// Build a store whose mutations publish `ConversationUpdated` via `publish`.
    pub fn new(publish: Arc<dyn Fn(ConversationUpdated) + Send + Sync>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Conversation::default())),
            publish,
            pending_permissions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Run `f` against the conversation for `session`, then publish. This is
    /// the hook protocol adapters fold their own updates through (ACP's
    /// `SessionUpdate` lives in `acp::conversation`).
    pub fn mutate(&self, session: &SessionKey, f: impl FnOnce(&mut Conversation)) {
        {
            let mut conv = self.inner.lock().expect("conversation mutex poisoned");
            f(&mut conv);
        }
        (self.publish)(ConversationUpdated {
            session: session.clone(),
        });
    }

    /// AU‑3: fold a locally-composed user prompt into the conversation as a
    /// `User` turn, then publish. ACP agents don't echo the user's prompt back,
    /// so the supervisor calls this when it sends a prompt to make the user's
    /// turn appear in the transcript immediately.
    pub fn push_user_text(&self, session: &SessionKey, text: &str) {
        {
            let mut conv = self.inner.lock().expect("conversation mutex poisoned");
            conv.push_user_text(text);
        }
        (self.publish)(ConversationUpdated {
            session: session.clone(),
        });
    }

    /// AUX‑1: push a permission block and register its oneshot responder. The
    /// supervisor calls this when `classify_permission` returns `AskUser`; the
    /// receiver is awaited in `handle_permission` in the ACP supervisor.
    pub fn push_permission_request(
        &self,
        session: &SessionKey,
        id: String,
        title: String,
        description: Option<String>,
        options: Vec<PermissionOption>,
        responder: oneshot::Sender<String>,
    ) {
        {
            let mut conv = self.inner.lock().expect("conversation mutex poisoned");
            conv.push_permission_block(id.clone(), title, description, options);
            self.pending_permissions
                .lock()
                .expect("pending_permissions mutex poisoned")
                .insert(id, (session.clone(), responder));
        }
        (self.publish)(ConversationUpdated {
            session: session.clone(),
        });
    }

    /// PU-B.2: resolve a pending permission request by the agent's chosen
    /// `option_id`. Derives the inline block status from that option's `kind`,
    /// updates the block, and sends the `option_id` through the oneshot the
    /// supervisor is parked on (which answers ACP with `Selected(option_id)`),
    /// then publishes. No-op when `id` is unknown (already resolved, deferred
    /// and re-resolved, or never registered).
    pub fn resolve_permission(&self, id: &str, option_id: &str) {
        let session = {
            let mut conv = self.inner.lock().expect("conversation mutex poisoned");
            // Fail closed: an option whose kind we can't read (missing block, or
            // an unrecognised kind) marks the request Denied rather
            // than leaving it Pending or optimistically Allowed.
            let status = match conv.permission_option_kind(id, option_id) {
                Some(PermissionKind::AllowOnce | PermissionKind::AllowAlways) => {
                    PermissionStatus::Allowed
                }
                _ => PermissionStatus::Denied,
            };
            conv.update_permission_status(id, status);
            let mut pending = self
                .pending_permissions
                .lock()
                .expect("pending_permissions mutex poisoned");
            let (session, sender) = match pending.remove(id) {
                Some(entry) => entry,
                None => return, // already resolved — no-op
            };
            let _ = sender.send(option_id.to_string());
            session
        };
        (self.publish)(ConversationUpdated { session });
    }

    /// PU-B.2b: the oldest still-`Pending` permission request, projected for
    /// the popup menu (`ai-permission-mode` reads it in `on_activate`). Turns
    /// and blocks are in wire order, so the first `Pending` block is the oldest.
    pub fn oldest_pending_permission(&self) -> Option<PendingPermissionView> {
        self.oldest_pending_permission_where(|_| true)
    }

    /// PU-B.3: the oldest `Pending` request whose id satisfies `keep`. The
    /// auto-open tick callback passes `|id| !deferred(id)` so an `Esc`-deferred
    /// request is skipped (the queue advances past it) without re-opening it.
    pub fn oldest_pending_permission_where(
        &self,
        keep: impl Fn(&str) -> bool,
    ) -> Option<PendingPermissionView> {
        let conv = self.inner.lock().expect("conversation mutex poisoned");
        conv.turns
            .iter()
            .flat_map(|t| t.blocks.iter())
            .find_map(|b| match b {
                Block::Permission {
                    id,
                    title,
                    description,
                    options,
                    status: PermissionStatus::Pending,
                } if keep(id) => Some(PendingPermissionView {
                    id: id.clone(),
                    title: title.clone(),
                    description: description.clone(),
                    options: options.clone(),
                }),
                _ => None,
            })
    }

    /// AUX‑3: set the global processing status and publish a
    /// `ConversationUpdated` so the headerline re-renders.
    pub fn set_status(&self, session: &SessionKey, status: SessionStatus) {
        {
            let mut conv = self.inner.lock().expect("conversation mutex poisoned");
            conv.status = status;
        }
        (self.publish)(ConversationUpdated {
            session: session.clone(),
        });
    }

    /// Cheap-ish clone of the current conversation for projection.
    pub fn snapshot(&self) -> Conversation {
        self.inner
            .lock()
            .expect("conversation mutex poisoned")
            .clone()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    fn test_permission_option(
        id: &'static str,
        name: &'static str,
        kind: PermissionKind,
    ) -> PermissionOption {
        PermissionOption::new(id, name, kind)
    }

    /// AU‑3: a user prompt lands as its own `User` turn (the REPL model:
    /// each Enter is distinct, never merged into agent output).
    #[test]
    fn push_user_text_appends_a_user_turn() {
        let mut c = Conversation::default();
        c.extend_text(Role::Assistant, "hello");
        c.push_user_text("refactor parse_args");
        assert_eq!(c.turns.len(), 2);
        assert_eq!(c.turns[1].role, Role::User);
        assert_eq!(
            c.turns[1].blocks,
            vec![Block::Text("refactor parse_args".to_string())]
        );
        // Two consecutive prompts are two distinct turns, not merged.
        c.push_user_text("again");
        assert_eq!(c.turns.len(), 3);
        assert_eq!(c.turns[2].role, Role::User);
    }

    /// AU‑3: `ConversationStore::push_user_text` mutates the shared store and
    /// publishes a `ConversationUpdated` so the mode's drain re-projects.
    #[test]
    fn store_push_user_text_mutates_and_publishes() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let published = Arc::new(AtomicUsize::new(0));
        let p = published.clone();
        let store = ConversationStore::new(Arc::new(move |_ev| {
            p.fetch_add(1, Ordering::SeqCst);
        }));
        store.push_user_text(&SessionKey::new("opencode", 1), "hi");
        assert_eq!(published.load(Ordering::SeqCst), 1, "one publish");
        let snap = store.snapshot();
        assert_eq!(snap.turns.len(), 1);
        assert_eq!(snap.turns[0].role, Role::User);
    }

    #[test]
    fn permission_block_created_in_assistant_turn() {
        let mut c = Conversation::default();
        c.extend_text(Role::Assistant, "working");
        c.push_permission_block(
            "perm-1".to_string(),
            "Allow agent to run cargo test?".to_string(),
            None,
            vec![test_permission_option(
                "a1",
                "Allow once",
                PermissionKind::AllowOnce,
            )],
        );
        assert_eq!(c.turns.len(), 1);
        assert_eq!(c.turns[0].role, Role::Assistant);
        assert_eq!(c.turns[0].blocks.len(), 2);
        match &c.turns[0].blocks[1] {
            Block::Permission {
                id,
                title,
                status,
                options,
                ..
            } => {
                assert_eq!(id, "perm-1");
                assert_eq!(title, "Allow agent to run cargo test?");
                assert_eq!(*status, PermissionStatus::Pending);
                assert_eq!(options.len(), 1);
            }
            other => panic!("expected Permission block, got {other:?}"),
        }
    }

    #[test]
    fn permission_block_opens_fresh_assistant_turn_when_no_previous() {
        let mut c = Conversation::default();
        c.push_permission_block("perm-1".to_string(), "Allow?".to_string(), None, vec![]);
        assert_eq!(c.turns.len(), 1);
        assert_eq!(c.turns[0].role, Role::Assistant);
    }

    #[test]
    fn permission_block_updated_by_id() {
        let mut c = Conversation::default();
        c.push_permission_block("perm-1".to_string(), "Allow?".to_string(), None, vec![]);
        c.update_permission_status("perm-1", PermissionStatus::Allowed);
        match &c.turns[0].blocks[0] {
            Block::Permission { status, .. } => assert_eq!(*status, PermissionStatus::Allowed),
            other => panic!("expected Permission, got {other:?}"),
        }
    }

    #[test]
    fn store_push_permission_request_creates_block_and_registers_responder() {
        let published = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let p = published.clone();
        let store = ConversationStore::new(Arc::new(move |_ev| {
            p.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let session = SessionKey::new("opencode", 1);
        let (tx, rx) = tokio::sync::oneshot::channel();

        store.push_permission_request(
            &session,
            "perm-1".to_string(),
            "Allow?".to_string(),
            None,
            vec![],
            tx,
        );

        // One publish on push
        assert_eq!(published.load(std::sync::atomic::Ordering::SeqCst), 1);
        // Block exists in snapshot
        let snap = store.snapshot();
        assert_eq!(snap.turns.len(), 1);
        assert!(matches!(
            &snap.turns[0].blocks[0],
            Block::Permission { id, status: PermissionStatus::Pending, .. } if id == "perm-1"
        ));
        // Responder is registered — dropping rx won't hang the test
        drop(rx);
    }

    #[test]
    fn store_resolve_permission_sends_outcome_and_publishes() {
        let published = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let p = published.clone();
        let store = ConversationStore::new(Arc::new(move |_ev| {
            p.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let session = SessionKey::new("opencode", 1);
        let (tx, rx) = tokio::sync::oneshot::channel();

        // PU-B.2: resolve by the agent's actual option; the block status derives
        // from that option's kind (AllowOnce → Allowed).
        let opt = test_permission_option("a1", "Allow once", PermissionKind::AllowOnce);
        let chosen = opt.option_id.clone();
        store.push_permission_request(
            &session,
            "perm-1".to_string(),
            "Allow?".to_string(),
            None,
            vec![opt],
            tx,
        );

        // Reset publish count — the push already fired once
        published.store(0, std::sync::atomic::Ordering::SeqCst);

        store.resolve_permission("perm-1", &chosen);

        // Block updated
        let snap = store.snapshot();
        match &snap.turns[0].blocks[0] {
            Block::Permission { status, .. } => assert_eq!(*status, PermissionStatus::Allowed),
            other => panic!("expected Permission, got {other:?}"),
        }
        // Oneshot delivered the chosen option id
        assert_eq!(
            rx.blocking_recv(),
            Ok(chosen),
            "responder must receive the chosen option id",
        );
        // Publish fired
        assert_eq!(
            published.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "resolve must publish",
        );
    }

    #[test]
    fn store_resolve_permission_noop_for_unknown_id() {
        let published = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let p = published.clone();
        let store = ConversationStore::new(Arc::new(move |_ev| {
            p.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));

        // No pending permission with this id → no-op, no publish
        let opt = test_permission_option("a1", "Allow once", PermissionKind::AllowOnce);
        store.resolve_permission("nonexistent", &opt.option_id);
        assert_eq!(published.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn oldest_pending_permission_returns_the_first_in_wire_order() {
        let store = ConversationStore::new(Arc::new(|_| {}));
        let session = SessionKey::new("opencode", 1);
        let (tx1, _rx1) = tokio::sync::oneshot::channel();
        let (tx2, _rx2) = tokio::sync::oneshot::channel();
        store.push_permission_request(
            &session,
            "perm-1".to_string(),
            "First?".to_string(),
            None,
            vec![test_permission_option(
                "a",
                "Allow",
                PermissionKind::AllowOnce,
            )],
            tx1,
        );
        store.push_permission_request(
            &session,
            "perm-2".to_string(),
            "Second?".to_string(),
            None,
            vec![test_permission_option(
                "b",
                "Allow",
                PermissionKind::AllowOnce,
            )],
            tx2,
        );

        let oldest = store
            .oldest_pending_permission()
            .expect("a pending request exists");
        assert_eq!(oldest.id, "perm-1", "oldest = first in wire order");
        assert_eq!(oldest.title, "First?");
        assert_eq!(oldest.options.len(), 1);

        // Resolving the first surfaces the second as the new oldest.
        store.resolve_permission("perm-1", &oldest.options[0].option_id);
        assert_eq!(
            store.oldest_pending_permission().map(|v| v.id),
            Some("perm-2".to_string()),
        );
    }

    #[test]
    fn status_idle_default() {
        let c = Conversation::default();
        assert_eq!(c.status, SessionStatus::Idle);
    }

    #[test]
    fn set_status_updates_and_publishes() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let published = Arc::new(AtomicUsize::new(0));
        let p = published.clone();
        let store = ConversationStore::new(Arc::new(move |_ev| {
            p.fetch_add(1, Ordering::SeqCst);
        }));
        let session = SessionKey::new("opencode", 1);

        store.set_status(&session, SessionStatus::Thinking);
        assert_eq!(published.load(Ordering::SeqCst), 1);
        assert_eq!(store.snapshot().status, SessionStatus::Thinking);

        store.set_status(&session, SessionStatus::Idle);
        assert_eq!(store.snapshot().status, SessionStatus::Idle);
    }

    #[test]
    fn status_display_formats() {
        assert_eq!(SessionStatus::Idle.to_string(), "Ready");
        assert!(SessionStatus::Thinking.to_string().contains("Thinking"));
        let exec = SessionStatus::Executing {
            tool: "edit".into(),
        };
        assert_eq!(exec.to_string(), "Working: edit");
        assert!(
            SessionStatus::AwaitingPermission
                .to_string()
                .contains("Awaiting")
        );
    }

    #[test]
    fn unrecognised_option_kind_resolves_as_denied() {
        let store = ConversationStore::new(Arc::new(|_| {}));
        let session = SessionKey::new("llm", 1);
        let (tx, rx) = tokio::sync::oneshot::channel();
        let opt = test_permission_option("x", "Mystery", PermissionKind::Other);
        store.push_permission_request(&session, "p".into(), "t".into(), None, vec![opt], tx);
        store.resolve_permission("p", "x");
        assert!(matches!(
            &store.snapshot().turns[0].blocks[0],
            Block::Permission {
                status: PermissionStatus::Denied,
                ..
            }
        ));
        assert_eq!(rx.blocking_recv(), Ok("x".to_string()));
    }

    #[test]
    fn merge_tool_update_keeps_fields_the_update_omits() {
        let mut c = Conversation::default();
        c.push_tool_call(
            "t".into(),
            Some("fs".into()),
            "read".into(),
            ToolStatus::Running,
            ToolKind::Read,
            Some("in".into()),
            None,
        );
        c.merge_tool_update("t", Some(ToolStatus::Ok), None, None, Some("out".into()));
        match &c.turns[0].blocks[0] {
            Block::ToolCall {
                origin,
                status,
                kind,
                input,
                output,
                ..
            } => {
                assert_eq!(origin.as_deref(), Some("fs"));
                assert_eq!(*status, ToolStatus::Ok);
                assert_eq!(*kind, ToolKind::Read);
                assert_eq!(input.as_deref(), Some("in"));
                assert_eq!(output.as_deref(), Some("out"));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }
}
