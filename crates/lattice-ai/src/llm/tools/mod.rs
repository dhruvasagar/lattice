//! Tools a model may call (LL.16, design `llm-layer.md` §6).
//!
//! A [`ToolSource`] is anything that lists tools and runs them: lattice's own
//! (`lattice.rs`), the web's (`web.rs`), each external MCP server (`mcp.rs`,
//! LL.24) and each plugin exporting `llm-tools` (`plugin.rs`, AT.5). The [`ToolBroker`] holds the
//! sources, namespaces their tools and decides what each may do; the loop sees
//! only the [`Offer`] the broker makes for one request.
//!
//! **Descriptors are MCP's shape** (name, description, JSON Schema input, a
//! text result), so nothing above the broker changes when MCP arrives behind
//! it. **Trust is ours, never the source's claim**: a source says what kind a
//! tool is, and the broker believes it only for lattice's own tools.

pub mod broker;
pub mod lattice;
pub mod mcp;
pub mod plugin;
pub mod rules;
pub mod web;

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

pub use broker::{Offer, OfferedTool, Permission, ToolBroker, ToolBrokerHandle};

use super::ToolDescriptor;
use crate::conversation::ToolKind;

/// A boxed, sendable future, the shape a `dyn` source returns.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One tool as its source lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceTool {
    /// The source-local name and schema; the broker namespaces the name.
    pub descriptor: ToolDescriptor,
    /// What it does. Believed only for lattice's own tools (§6).
    pub kind: ToolKind,
}

/// What a tool returns: text the model reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    pub text: String,
}

/// Why a call produced no output. Every one reaches the model as an error
/// result it can act on, never as a failed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    /// No tool by that name is offered to this request.
    UnknownTool(String),
    /// The arguments are not what the tool takes.
    InvalidArguments(String),
    /// The tool ran and could not do it (a missing file, say).
    Failed(String),
    /// The tool was not run: outside what this request may do.
    Refused(String),
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownTool(name) => write!(f, "no tool named `{name}` is available"),
            Self::InvalidArguments(why) => write!(f, "invalid arguments: {why}"),
            Self::Failed(why) => f.write_str(why),
            Self::Refused(why) => write!(f, "not run: {why}"),
        }
    }
}

impl std::error::Error for ToolError {}

/// Who a call is for (LL.17). Lattice's edit tools read it; a source with no
/// use for it ignores it.
#[derive(Clone, Default)]
pub struct CallContext {
    /// The session the call belongs to. A review it opens carries this, so
    /// closing the session (or stopping its request) closes the review.
    pub origin: u64,
    /// The session trusts edits (`<C-t>` in the chat): they apply without a
    /// review.
    pub trusted: bool,
    /// Told `true` when a review opens and `false` when it closes, so the
    /// request shows `AwaitingReview` while the user decides.
    pub on_review: Option<std::sync::Arc<dyn Fn(bool) + Send + Sync>>,
    /// Called when an edit applies without a review (a trusted session).
    /// The chat drops its "allow for this chat" commands then: an allowed
    /// `cargo test` was allowed against the code as it was, and an unseen
    /// edit to `build.rs` or a test changes what that command runs.
    pub on_unreviewed_edit: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    /// Asks the user whether a tool may run (LL.18). `None` where nothing
    /// can ask: a tool that must ask is then refused.
    pub ask: Option<AskFn>,
    /// `llm.allow` and `llm.deny` as they were when the request was
    /// prepared. `None` (a bare test) is no rules.
    pub rules: Option<std::sync::Arc<rules::ToolRules>>,
}

/// What the user is asked before a tool runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    /// `server.name`.
    pub tool: String,
    pub title: String,
    /// What it would do: the command, or the arguments.
    pub detail: Option<String>,
    /// What an "allow for this chat" answer covers.
    pub chat_allow: ChatAllow,
}

/// What an "allow for this chat" answer covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatAllow {
    /// Every later call of the tool.
    Tool,
    /// Later calls that do exactly this: the call's arguments as JSON, so
    /// the command and where it runs cannot pass for each other. For a tool
    /// that runs commands: allowing `cargo test` must not allow
    /// whatever a prompt-injected tool result asks for next.
    Call(String),
    /// Later calls to this host: `web.fetch` (AT.2). One host per grant, so
    /// allowing a documentation site does not allow the next host a page
    /// sends the model to, and each page of the same site does not ask again.
    Host(String),
}

impl Question {
    /// The key an "allow for this chat" is remembered by.
    pub fn allow_key(&self) -> String {
        match &self.chat_allow {
            ChatAllow::Tool => self.tool.clone(),
            ChatAllow::Call(call) => format!("{}\n{call}", self.tool),
            ChatAllow::Host(host) => format!("{}\nhost:{host}", self.tool),
        }
    }

    /// The menu's label for the "allow for this chat" answer.
    pub fn chat_allow_label(&self) -> &'static str {
        match self.chat_allow {
            // Says what it covers: every later call, whatever its arguments.
            // A server tool's input cannot be judged the way a command's can,
            // so it is allowed whole or asked each time (P.3).
            ChatAllow::Tool => "Allow this tool for this chat (any arguments)",
            ChatAllow::Call(_) => "Allow this command for this chat",
            ChatAllow::Host(_) => "Allow this site for this chat",
        }
    }
}

/// The user's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// Run it this once.
    Once,
    /// Run it, and this tool again in this chat without asking.
    ForChat,
    /// Do not run it.
    Deny,
}

/// Puts a [`Question`] to the user and resolves with the answer. Dropping
/// the future withdraws the question.
pub type AskFn = std::sync::Arc<dyn Fn(Question) -> BoxFuture<'static, Answer> + Send + Sync>;

impl std::fmt::Debug for CallContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallContext")
            .field("origin", &self.origin)
            .field("trusted", &self.trusted)
            .finish_non_exhaustive()
    }
}

/// Lists tools and runs them. Implemented by lattice's tools and by each
/// external MCP server's client connection.
pub trait ToolSource: Send + Sync {
    /// The tools this source offers now.
    fn list(&self) -> BoxFuture<'_, Result<Vec<SourceTool>, ToolError>>;

    /// Run `name` (a source-local name) with `input`. Dropping the future
    /// abandons the call; a source with work to stop must stop it on drop.
    fn call(
        &self,
        name: &str,
        input: Value,
        ctx: CallContext,
    ) -> BoxFuture<'_, Result<ToolOutput, ToolError>>;

    /// What a call would do, in a line the user can judge before allowing
    /// it: `cargo test` for a command. `None` shows the arguments.
    fn describe(&self, _name: &str, _input: &Value) -> Option<String> {
        None
    }

    /// The user's own config allows `name` to run without asking (LL.24:
    /// `allow = [...]` on an `[llm.mcp.*]` table). It comes from the user's
    /// file through the source lattice built from it, never from what a
    /// server says about itself.
    fn allowed(&self, _name: &str) -> bool {
        false
    }

    /// What "allow for this chat" covers for a call that asks, when it is
    /// narrower than the whole tool: the host, for `web.fetch`. `None` is
    /// the tool. A command's exact call is decided by its kind, not here.
    fn chat_scope(&self, _name: &str, _input: &Value) -> Option<ChatAllow> {
        None
    }

    /// The row `name` sits in, when lattice built this source from something
    /// it trusts to decide it: a plugin's capability grant (AT.5). `None`
    /// leaves it to the broker, which believes kinds for its native sources
    /// only. Never from what the tool says about itself.
    fn permission(&self, _name: &str) -> Option<Permission> {
        None
    }
}
