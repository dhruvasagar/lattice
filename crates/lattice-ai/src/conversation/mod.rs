//! `conversation` -- the protocol-neutral conversation model (LL.1).
//!
//! The turn/block tree the `ai-conversation` mode projects into a buffer, with
//! the projection itself and the tool-call / reasoning fold sources. Nothing
//! here names a wire protocol: ACP's `SessionUpdate` is folded in by the adapter
//! in `acp::conversation`, and the native LLM layer drives the same mutators.
//! A grep test (`tests/conversation_names_no_protocol.rs`) holds that line.
//!
//! Compiled whenever a conversation producer is (`acp` or `llm`).

pub mod model;
pub mod projection;
pub mod tool_fold;

pub use model::{
    Block, Conversation, ConversationProjected, ConversationStore, ConversationUpdated, Cost,
    EditStatus, PendingPermissionView, PermissionKind, PermissionOption, PermissionStatus, Role,
    SessionStatus, ToolKind, ToolStatus, Turn, UsageSnapshot,
};
pub use projection::{project_conversation, render_conversation};
