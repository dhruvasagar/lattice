//! Shared types and message definitions used across the lattice editor.
//!
//! The protocol crate is the dependency floor: every other crate depends on it,
//! and it depends on no other lattice crate. It defines:
//!
//! - Newtype IDs for all major entities (`DocumentId`, `PaneId`, ...).
//! - The structural value types (`Position`, `Range`, `Edit`, `Selection`).
//! - The `Command` and `Event` enums that flow between layers.
//!
//! Wire format details (MessagePack envelope for cross-process transport) live
//! in this crate as well; in-process callers pay zero serialization cost.

pub mod cancel;
pub mod command;
pub mod edit;
pub mod error;
pub mod event;
pub mod ids;
pub mod position;
pub mod selection;

pub use crate::cancel::CancellationToken;
pub use crate::command::Command;
pub use crate::edit::{Edit, EditKind};
pub use crate::error::{ProtocolError, Result};
pub use crate::event::{Event, EventKind};
pub use crate::ids::{
    BufferId, CommandId, DocumentId, MajorModeId, MinorModeId, PaneId, PluginId, TabId, WindowId,
};
pub use crate::position::{Position, Range};
pub use crate::selection::{Selection, SelectionSet, VisualMode};
