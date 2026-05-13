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
//! [`lattice_grammar::CommandInvocation`] for typed runtime invocation, and
//! the document actor exposes its own typed mailbox via
//! `lattice_runtime::DocumentHandle`. The legacy `Command` enum had no
//! callers anywhere in the workspace and was retired.
//! [`lattice_grammar::CommandInvocation`] is the canonical "runtime
//! command" type now.

pub mod cancel;
pub mod edit;
pub mod error;
pub mod event;
pub mod event_registry;
pub mod ids;
pub mod position;
pub mod selection;

pub use crate::cancel::CancellationToken;
pub use crate::edit::{Edit, EditDelta, EditKind};
pub use crate::error::{ProtocolError, Result};
pub use crate::event::{Event, EventKind};
pub use crate::ids::{
    BufferId, CommandId, DocumentId, MajorModeId, MinorModeId, PaneId, PluginId, TabId, WindowId,
};
pub use crate::position::{Position, Range};
pub use crate::selection::{Selection, SelectionSet, VisualMode};
