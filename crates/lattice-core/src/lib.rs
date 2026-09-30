//! The editor's text model: rope-backed buffers, documents with undo and
//! dirty tracking, regex search, and the small renderer-neutral vocabularies
//! (buffer ids and kinds, pane geometry, folds, indent units, project roots)
//! that every other lattice crate shares.
//!
//! This crate owns the actor-protected document model. It is content-type
//! agnostic; tree-sitter, LSP, plugin, and rendering concerns live elsewhere.
//! The dispatcher that serialises edits is not here either — it lives in
//! `lattice-host`; this crate only guarantees each operation is consistent.
//!
//! # What it owns
//!
//! - **Text and editing.** [`Buffer`] wraps a `ropey::Rope` and applies
//!   [`protocol::Edit`]s, returning an [`buffer::AppliedEdit`] (inverse +
//!   tree-sitter delta). [`Document`] adds identity, path, versions,
//!   selections, the [`UndoStack`] (with insert-session coalescing via
//!   [`Document::begin_undo_group`]) and dirty tracking. Errors are
//!   [`CoreError`].
//! - **Search.** [`search::find`] / [`search::find_all`] stream a compiled
//!   `fancy_regex::Regex` over the rope without materialising it.
//! - **Shared vocabularies.** [`BufferId`] / [`BufferKind`] /
//!   [`BufferFlags`]; the pane tree and its geometry
//!   ([`ui::pane::PaneTree`]); [`Fold`] / [`FoldMethod`];
//!   [`IndentUnit`] / [`IndentMethod`]; [`ProviderChain`] for formatting;
//!   [`AutoWrap`]; the [`labeled_enum!`] macro every enum-typed option is
//!   declared with.
//! - **Service seams** for crates that sit below the host:
//!   [`Clipboard`], [`ProjectResolver`], [`FoldOverlayService`],
//!   [`ExcerptSourceResolver`], [`ViewArgsResolver`] — traits here,
//!   implementations wired in at boot.
//!
//! # Coordinates
//!
//! Every position is a [`protocol::Position`]: a **0-based line** and a
//! **0-based UTF-8 byte offset within that line** — not a char index and
//! not a display column. Ranges are half-open `[start, end)`.
//!
//! # What it must not depend on
//!
//! Nothing above [`lattice_protocol`]: no syntax, grammar, config, mode,
//! LSP, plugin or renderer crate, and no async runtime. Roughly thirty
//! crates depend on this one, so any dependency added here is a dependency
//! of the whole workspace — and one that reaches upward is a cycle. That
//! is the structural reason it is its own crate: it is the floor. Types
//! that several upper crates must share without depending on each other
//! (fold sources, pane-group row mappers, project resolution) are hoisted
//! *down* to here as data or traits, and implemented above.
//!
//! # Example
//!
//! ```
//! use lattice_core::Document;
//! use lattice_core::protocol::edit::Edit;
//! use lattice_core::protocol::position::{Position, Range};
//!
//! # fn main() -> lattice_core::CoreResult<()> {
//! let mut doc = Document::from_text("fn main() {}\n");
//!
//! // Rename `main` (line 0, bytes 3..7).
//! let range = Range::new(Position::new(0, 3), Position::new(0, 7));
//! let applied = doc.apply_edit(Edit::replace(range, "start"))?;
//! assert_eq!(doc.text(), "fn start() {}\n");
//! assert_eq!(applied.replaced_text, "main");
//! assert_eq!(doc.text_version(), 1);
//!
//! doc.undo()?;
//! assert_eq!(doc.text(), "fn main() {}\n");
//! assert!(!doc.dirty());
//! # Ok(())
//! # }
//! ```
//!
//! # Design documents
//!
//! - `docs/dev/architecture/design.md` §5.1 (buffer / document model) and
//!   §5.9 (everything is a buffer; panes)
//! - `docs/dev/architecture/owner-write-caret.md` (selection transform
//!   across edits)
//! - `docs/dev/architecture/project-resolution.md`,
//!   `docs/dev/architecture/fold-architecture.md`,
//!   `docs/dev/architecture/clipboard.md`,
//!   `docs/dev/architecture/text-reflow.md`,
//!   `docs/dev/architecture/pane-zoom.md`,
//!   `docs/dev/architecture/pane-groups.md`

#![warn(missing_docs)]

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
// RF.0: the `format.{indent,reflow,reformat}` provider vocabulary. Here
// for the same reason `indent` is — the resolver lives in the host but
// the grammar operators and the config layer both name these types, and
// this crate is the floor both already stand on.
pub mod format_chain;
/// `~` expansion, shared so every consumer resolves a home the same way.
pub mod home;
// IN.0: `IndentUnit` / `IndentMethod` — the resolved indent value the
// `>` / `<` operators consume. Here rather than in `lattice-indent`
// because `lattice-syntax` → `lattice-grammar` makes an engine-side
// home a dependency cycle; see `indent.rs`'s module doc.
pub mod indent;
pub mod indent_blocks;
// SS.1: the shared on-disk fingerprint (autoread + multibuffer sources).
pub mod on_disk;
// PR.1: which project a path belongs to. Here rather than in a crate of
// its own (heuristic #6: it carves out no dependency surface — the whole
// mechanism is `std::path` + `std::fs::exists`) and here rather than in
// `lattice-host` (no subsystem crate depends on the host, so terminal /
// compilation / magit / multibuffer would each be a cycle away).
pub mod project;
pub mod search;
pub mod ui;
pub mod undo;
// RF.0: `autowrap` — wrap-while-typing. Peer of `IndentMethod`; see
// `wrap.rs` for why it is an option rather than a minor mode.
pub mod wrap;

pub use crate::buffer::Buffer;
pub use crate::buffers::{BufferFlags, BufferId, BufferKind};
pub use crate::clipboard::{Clipboard, ClipboardHandle, FakeClipboard};
pub use crate::document::{Document, DocumentBuilder};
pub use crate::error::{CoreError, CoreResult};
pub use crate::folding::{
    Fold, FoldMethod, FoldOverlayService, FoldOverlayServiceHandle, FoldSource, ProviderId,
    ProviderKind,
};
pub use crate::format_chain::{FormatIntent, FormatProvider, ProviderChain};
pub use crate::indent::{IndentMethod, IndentUnit};
pub use crate::indent_blocks::LineShape;
pub use crate::project::{
    DEFAULT_ROOT_MARKERS, ExcerptSource, ExcerptSourceResolver, ExcerptSourceResolverHandle,
    MarkerResolver, Project, ProjectKind, ProjectResolver, ProjectResolverHandle, ViewArgsResolver,
    ViewArgsResolverHandle,
};
pub use crate::search::{Direction as SearchDir, SearchHit, find as search_find};
pub use crate::undo::{UndoEntry, UndoStack};
pub use crate::wrap::{AutoWrap, DEFAULT_TEXTWIDTH, WrapWidth};

pub use lattice_protocol as protocol;
