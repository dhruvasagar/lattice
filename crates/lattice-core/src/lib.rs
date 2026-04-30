//! Core editor state: buffers, documents, undo, and the dispatcher.
//!
//! This crate owns the actor-protected document model. It is content-type
//! agnostic; tree-sitter, LSP, plugin, and rendering concerns live elsewhere.

pub mod buffer;
pub mod document;
pub mod error;
pub mod search;
pub mod undo;

pub use crate::buffer::Buffer;
pub use crate::document::{Document, DocumentBuilder};
pub use crate::error::{CoreError, CoreResult};
pub use crate::search::{Direction as SearchDir, SearchHit, find as search_find};
pub use crate::undo::{UndoEntry, UndoStack};

pub use lattice_protocol as protocol;
