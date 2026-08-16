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
pub mod clipboard;
pub mod document;
pub mod error;
pub mod folding;
// IN.0: `IndentUnit` / `IndentMethod` — the resolved indent value the
// `>` / `<` operators consume. Here rather than in `lattice-indent`
// because `lattice-syntax` → `lattice-grammar` makes an engine-side
// home a dependency cycle; see `indent.rs`'s module doc.
pub mod indent;
pub mod indent_blocks;
// SS.1: the shared on-disk fingerprint (autoread + multibuffer sources).
pub mod on_disk;
pub mod search;
pub mod ui;
pub mod undo;

pub use crate::buffer::Buffer;
pub use crate::buffers::{BufferFlags, BufferId, BufferKind};
pub use crate::clipboard::{Clipboard, ClipboardHandle, FakeClipboard};
pub use crate::document::{Document, DocumentBuilder};
pub use crate::error::{CoreError, CoreResult};
pub use crate::folding::{
    Fold, FoldMethod, FoldOverlayService, FoldOverlayServiceHandle, FoldSource, ProviderId,
    ProviderKind,
};
pub use crate::indent::{IndentMethod, IndentUnit};
pub use crate::search::{Direction as SearchDir, SearchHit, find as search_find};
pub use crate::undo::{UndoEntry, UndoStack};

pub use lattice_protocol as protocol;
