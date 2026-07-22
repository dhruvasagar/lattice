//! Shared types and message definitions used across the lattice editor.
//!
//! The protocol crate is the dependency floor: every other crate depends on it,
//! and it depends on no other lattice crate. It defines:
//!
//! - Newtype IDs for all major entities (`DocumentId`, `PaneId`, ...).
//! - The structural value types (`Position`, `Range`, `Edit`, `Selection`).
//! - The `Event` enum that flows from the core to subscribers.
//!
//! Wire format details (MessagePack envelope for cross-process transport) live
//! in this crate as well; in-process callers pay zero serialization cost.
//!
//! ## Note on the retired `Command` enum
//!
//! Earlier revisions exposed a `lattice_protocol::Command` enum
//! (document-management + editing variants) intended as a wire-protocol
//! message set from clients (UI / plugins) to a central core dispatcher.
//! That client-server framing was abandoned: the editor runs as one process
//! today, the keymap / cmdline / dispatcher use
//! `lattice_grammar::CommandInvocation` for typed runtime invocation, and
//! the document actor exposes its own typed mailbox via
//! `lattice_runtime::RopeDocumentHandle`. The legacy `Command` enum had no
//! callers anywhere in the workspace and was retired.
//! `lattice_grammar::CommandInvocation` is the canonical "runtime
//! command" type now.

pub mod cancel;
pub mod chord;
pub mod edit;
pub mod error;
pub mod error_list;
pub mod event;
pub mod event_registry;
pub mod ids;
/// JSON-RPC 2.0 message types. Lifted out of `lattice-lsp` (IDE-protocol
/// Risk 3) so a second peer-protocol crate (`lattice-claude-code`) can
/// reuse the wire shape without an `ide -> lsp` crate edge. The types are
/// transport-agnostic; each peer's codec writes the bytes.
pub mod jsonrpc;
pub mod position;
pub mod selection;

pub use crate::cancel::CancellationToken;
pub use crate::chord::{
    ChordParseError, ChordPattern, KeyChord, KeyKind, KeyMods, SpecialKey,
    last_chord_token_byte_len, parse_chord_sequence, special_label,
};
pub use crate::edit::{Edit, EditDelta, EditKind};
pub use crate::error::{ProtocolError, Result};
pub use crate::event::{Event, EventKind};
pub use crate::ids::{
    BufferId, CommandId, DocumentId, MajorModeId, MinorModeId, PaneId, PluginId, TabId, WindowId,
};
pub use crate::jsonrpc::{
    Message, MessageDecodeError, Notification, Request, RequestId, Response, ResponseError,
};
pub use crate::position::{Position, Range};
pub use crate::selection::{Selection, SelectionSet, VisualMode};
