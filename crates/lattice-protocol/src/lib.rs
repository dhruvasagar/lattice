//! The shared vocabulary of the editor: the value types, identifiers, event
//! catalogue and wire envelopes that every other lattice crate speaks. It is
//! the dependency floor — every crate depends on it, and it depends on no
//! other lattice crate.
//!
//! ## What it owns
//!
//! - **Coordinates.** [`Position`] (0-based line, 0-based UTF-8 *byte* offset
//!   within the line) and the half-open [`Range`] built from two of them. Core,
//!   plugins and the dispatcher work only in these logical coordinates; the
//!   renderer converts to screen cells, and protocol peers (LSP's UTF-16
//!   columns) convert at their own boundary.
//! - **Edits.** [`Edit`] / [`EditKind`] (one atomic replace), and
//!   [`EditDelta`], the tree-sitter-shaped by-product of applying one.
//! - **Selections.** [`Selection`] (anchor + head + [`VisualMode`]) and the
//!   never-empty [`SelectionSet`] with a designated primary.
//! - **Identifiers.** `u64` newtypes ([`DocumentId`], [`BufferId`],
//!   [`CommandId`], ...) that cannot be mixed up with each other.
//! - **Events.** The closed catalogue of editor-core transitions ([`Event`],
//!   discriminated by [`EventKind`]) and, in [`event_registry`], the open
//!   typed-event surface feature crates and plugins declare their own events
//!   through.
//! - **Chords.** [`KeyChord`] and friends — the renderer-neutral key the
//!   keymap trie indexes by — plus the `"<C-w>j"` notation parser
//!   ([`parse_chord_sequence`]) and its `Display` inverse.
//! - **Peer-protocol envelopes.** JSON-RPC 2.0 [`Message`]s, shared by the
//!   LSP client and the Claude Code IDE peer.
//! - **Small shared primitives.** [`CancellationToken`], [`ProtocolError`],
//!   and the error-list entry ([`error_list::ErrorEntry`]).
//!
//! ## What it must not depend on
//!
//! No other lattice crate, no async runtime, no parser, no renderer, no
//! plugin host. Everything here is plain data (plus the cancellation flag and
//! the event registries), because anything this crate imported would sit
//! beneath the entire editor and every plugin-facing type. That is why the
//! event payloads carry mode names and modal states as `String`s rather than
//! `ModeId` / `ModalState`, why [`EditDelta`] mirrors tree-sitter's
//! `InputEdit` without importing it, and why [`error_list::ErrorSeverity`]
//! is not LSP's severity. Its dependencies are `serde`, `serde_json`,
//! `thiserror` and `linkme`.
//!
//! Everything is in-process today: the editor is one process, and nothing
//! here defines a cross-process transport. The serde derives exist for the
//! plugin boundary, snapshots and tests, not for a client/server split.
//!
//! ## Example
//!
//! ```
//! use lattice_protocol::{
//!     Edit, KeyChord, Position, Range, Selection, SelectionSet, parse_chord_sequence,
//! };
//!
//! // Coordinates are (line, byte) — both 0-based, the byte offset in UTF-8.
//! let hello = Range::new(Position::new(0, 0), Position::new(0, 5));
//! let edit = Edit::replace(hello, "howdy");
//! assert_eq!(edit.range.end.byte, 5);
//!
//! // A cursor is a zero-width selection; a set always has a primary.
//! let set = SelectionSet::single(Selection::cursor(Position::new(2, 4)));
//! assert!(set.primary().is_cursor());
//!
//! // Chord notation parses to typed keys and prints back canonically.
//! let chords = parse_chord_sequence("<C-w>j").unwrap();
//! assert_eq!(chords, vec![KeyChord::ctrl('w'), KeyChord::char('j')]);
//! assert_eq!(chords[0].to_string(), "<C-w>");
//! ```
//!
//! Design: `docs/dev/architecture/design.md` §5.10 (events and hooks) and §6
//! (core protocol); `docs/dev/architecture/keymap-architecture.md` (chords);
//! `docs/dev/architecture/error-list.md` (error-list entries);
//! `docs/dev/architecture/cancellation.md` (cancellation).
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
#![warn(missing_docs)]

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
