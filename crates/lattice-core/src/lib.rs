//! Core editor state: buffers, documents, undo, and the dispatcher.
//!
//! This crate owns the actor-protected document model. It is content-type
//! agnostic; tree-sitter, LSP, plugin, and rendering concerns live elsewhere.

// `labeled_enum!` lives at the top so its `#[macro_export]` is
// visible to the modules below that consume it (`folding`,
// `ui::display`). `#[macro_use]` makes the macro callable inside
// the crate without `use`; the `#[macro_export]` attribute on the
// macro itself exposes it to downstream crates.
#[macro_use]
pub mod labeled_enum;

pub mod buffer;
pub mod buffers;
pub mod document;
pub mod error;
pub mod folding;
pub mod search;
pub mod ui;
pub mod undo;

pub use crate::buffer::Buffer;
pub use crate::buffers::{BufferFlags, BufferId, BufferKind};
pub use crate::document::{Document, DocumentBuilder};
pub use crate::error::{CoreError, CoreResult};
pub use crate::folding::{Fold, FoldMethod};
pub use crate::search::{Direction as SearchDir, SearchHit, find as search_find};
pub use crate::undo::{UndoEntry, UndoStack};

pub use lattice_protocol as protocol;
